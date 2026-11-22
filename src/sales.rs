//! Creating sales, and how a sale looks to the API.

use std::str::FromStr;

use anyhow::Context;
use bitcoin::{Amount, Denomination};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio_postgres::GenericClient;

use crate::App;
use crate::database::{CheckoutLink, Rail, SALE_SELECT, Sale, SaleState, Store};
use crate::error::{ApiError, ApiResult, bad_request, not_found};
use crate::money::{Decimal, currency_decimals};
use crate::rails::onchain::StoreDescriptor;
use crate::token;


pub const MAX_MEMO_CHARS: usize = 200;

/// The amount a sale is created for.
#[derive(Debug, Clone)]
pub enum SaleAmount {
	Fiat(Decimal),
	Sat(Amount),
}

/// A sale to create.
#[derive(Debug, Clone)]
pub struct NewSale {
	pub link: CheckoutLink,
	pub cashier_name: Option<String>,
	pub amount: SaleAmount,
	pub memo: Option<String>,
}

/// The sale as the API shows it.
#[derive(Debug, Clone, Serialize)]
pub struct SaleView {
	pub id: String,
	pub created_at: DateTime<Utc>,
	pub expires_at: DateTime<Utc>,
	pub state: SaleState,
	pub amount_sat: i64,
	pub fiat_amount: Option<String>,
	pub currency: Option<String>,
	pub btc_price: Option<String>,
	pub memo: Option<String>,
	pub payment_request: String,
	pub lightning_invoice: Option<String>,
	pub onchain_address: Option<String>,
	pub paid_at: Option<DateTime<Utc>>,
	pub paid_rail: Option<Rail>,
	/// Counted payments only: delivered invoices and confirmed outputs.
	pub received_sat: i64,
	/// Unconfirmed on-chain payments. They are shown and never counted.
	pub seen_sat: i64,
	/// Why the sale is in progress: `onchain` (in the mempool, not
	/// guaranteed) or `ark` (settled by the Ark server, not delivered yet).
	pub progress: Option<String>,
	/// `not-guaranteed`, `low-fee`, `unconfirmed-parents`, `replaced`.
	pub warnings: Vec<String>,
	/// `payment-replaced` or `payment-dropped`, after a payment went away.
	pub notice: Option<String>,
	/// Paid by an invoice, or by on-chain payments with 3 confirmations.
	pub r#final: bool,
	pub final_at: Option<DateTime<Utc>>,
	/// The sale ended unpaid and a payment came after: the owner sees it,
	/// and it never pays another sale.
	pub late_payment: bool,
	pub link: CheckoutLink,
	pub cashier_name: Option<String>,
}

impl From<Sale> for SaleView {
	fn from(s: Sale) -> SaleView {
		let payment_request = payment_request(
			s.onchain_address.as_deref(),
			Amount::from_sat(s.amount_sat as u64),
			s.lightning_invoice.as_deref(),
		);
		SaleView {
			id: s.public_id,
			created_at: s.created_at,
			expires_at: s.expires_at,
			state: s.state,
			amount_sat: s.amount_sat,
			fiat_amount: s.fiat_amount,
			currency: s.currency,
			btc_price: s.btc_price,
			memo: s.memo,
			payment_request,
			lightning_invoice: s.lightning_invoice,
			onchain_address: s.onchain_address,
			paid_at: s.paid_at,
			paid_rail: s.paid_rail,
			received_sat: s.received_sat,
			seen_sat: s.seen_sat,
			progress: s.progress.filter(|_| s.state == SaleState::InProgress),
			warnings: s.warnings,
			notice: s.notice,
			r#final: s.final_at.is_some(),
			final_at: s.final_at,
			late_payment: s.state.ended_unpaid() && (s.received_sat > 0 || s.seen_sat > 0),
			link: s.link,
			cashier_name: s.cashier_name,
		}
	}
}

/// The BIP 321 URI of a sale. The address and amount are only present when
/// the sale offers on-chain. The one invoice serves every Lightning wallet,
/// and a Scuttle wallet pays it over Ark through its route hint.
pub fn payment_request(
	onchain_address: Option<&str>,
	amount: Amount,
	lightning: Option<&str>,
) -> String {
	let mut params = Vec::new();
	if onchain_address.is_some() {
		params.push(format!("amount={}", btc_amount(amount)));
	}
	if let Some(inv) = lightning {
		params.push(format!("lightning={}", inv));
	}
	let mut uri = format!("bitcoin:{}", onchain_address.unwrap_or(""));
	if !params.is_empty() {
		uri.push('?');
		uri.push_str(&params.join("&"));
	}
	uri
}

/// A BIP 321 amount: bitcoin, without trailing zeros.
fn btc_amount(amount: Amount) -> String {
	let s = amount.to_string_in(Denomination::Bitcoin);
	if s.contains('.') {
		s.trim_end_matches('0').trim_end_matches('.').to_string()
	} else {
		s
	}
}

/// The sat amount of a new sale, and its fiat amount and rate when it was
/// keyed in fiat.
async fn resolve_amount(
	app: &App,
	store: &Store,
	amount: SaleAmount,
) -> ApiResult<(Amount, Option<(Decimal, Decimal)>)> {
	let (amount_sat, fiat) = match amount {
		SaleAmount::Sat(sat) => (sat, None),
		SaleAmount::Fiat(fiat) => {
			let decimals = currency_decimals(&store.currency)
				.context("store has an unsupported currency")?;
			if fiat.scale() > decimals {
				return Err(bad_request(format!(
					"{} has {} decimal places", store.currency, decimals,
				)));
			}
			let fiat = fiat.round(decimals);
			let quote = app.price.quote(&store.currency).await.map_err(ApiError::Upstream)?;
			let sat = fiat.fiat_to_sat(&quote.btc_price)
				.ok_or_else(|| bad_request("the amount is too large"))?;
			(sat, Some((fiat, quote.btc_price)))
		},
	};
	if amount_sat == Amount::ZERO {
		return Err(bad_request("the amount must be at least 1 sat"));
	}
	Ok((amount_sat, fiat))
}

/// Whether a new address at `next_index` stays within `gap_limit` of the
/// last address that received anything, so a wallet scanning with that gap
/// limit sees it.
pub fn within_gap(next_index: i64, last_used: Option<i64>, gap_limit: u32) -> bool {
	next_index <= last_used.unwrap_or(-1) + gap_limit as i64
}

/// Hands out an on-chain address for a new sale: an address that never
/// received anything and whose last sale ended more than the grace period
/// ago, or else a new one within the gap limit. `None` when there is none.
async fn allocate_address(
	app: &App,
	tx: &tokio_postgres::Transaction<'_>,
	store: &Store,
) -> anyhow::Result<Option<(i64, bitcoin::Address)>> {
	// Allocations of one store run one at a time.
	tx.query_one("SELECT id FROM store WHERE id = $1 FOR UPDATE", &[&store.id]).await?;
	let grace = app.cfg.sales.onchain_reuse_grace_secs as f64;
	let reusable = tx.query("
		SELECT id, address FROM onchain_address
		WHERE store_id = $1 AND NOT ever_received AND sale_id IS NULL
			AND released_at < NOW() - make_interval(secs => $2)
		ORDER BY derivation_index LIMIT 5
	", &[&store.id, &grace]).await?;
	for row in reusable {
		let id: i64 = row.get("id");
		let address = bitcoin::Address::from_str(row.get::<_, &str>("address"))?
			.require_network(app.cfg.network)?;
		// The chain has the last word: an address that ever received
		// anything is never handed out again.
		if app.chain.has_received(&address).await? {
			tx.execute("UPDATE onchain_address SET ever_received = TRUE WHERE id = $1", &[&id]).await?;
			continue;
		}
		return Ok(Some((id, address)));
	}

	let row = tx.query_one("
		SELECT next_onchain_index, (
			SELECT max(derivation_index) FROM onchain_address WHERE store_id = $1 AND ever_received
		) AS last_used
		FROM store WHERE id = $1
	", &[&store.id]).await?;
	let next: i64 = row.get("next_onchain_index");
	let last_used: Option<i64> = row.get("last_used");
	if !within_gap(next, last_used, app.cfg.sales.onchain_gap_limit) {
		tracing::warn!("store {} has {} unpaid addresses past its last payment, the gap limit; \
			the sale offers no on-chain payment", store.id, next - last_used.unwrap_or(-1) - 1);
		return Ok(None);
	}
	let desc = StoreDescriptor::parse(&store.onchain_descriptor).context("store has an invalid descriptor")?;
	let address = desc.address(u32::try_from(next).context("on-chain index overflow")?, app.cfg.network)?;
	app.chain.watch(&address).await?;
	let id: i64 = tx.query_one("
		INSERT INTO onchain_address (store_id, derivation_index, address) VALUES ($1, $2, $3)
		RETURNING id
	", &[&store.id, &next, &address.to_string()]).await?.get("id");
	tx.execute("
		UPDATE store SET next_onchain_index = $2, updated_at = NOW() WHERE id = $1
	", &[&store.id, &(next + 1)]).await?;
	Ok(Some((id, address)))
}

/// Creates a sale with all the rails it offers: one invoice for Lightning
/// and Ark payers, and an address above the store's on-chain minimum. A
/// rail whose backend fails is left out; with no rail left, the sale is
/// refused with the fixed "unavailable" message.
pub async fn create_sale(app: &App, store: &Store, new: NewSale) -> ApiResult<Sale> {
	let memo = new.memo.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
	if memo.as_ref().is_some_and(|m| m.chars().count() > MAX_MEMO_CHARS) {
		return Err(bad_request(format!("the memo is longer than {} characters", MAX_MEMO_CHARS)));
	}
	let (amount_sat, fiat) = resolve_amount(app, store, new.amount).await?;

	let ark_address = ark::Address::from_str(&store.ark_address)
		.context("store has an invalid Ark address")?;
	let expiry = app.cfg.sales.expiry();
	let description = Some(memo.clone().unwrap_or_else(|| store.name.clone()));

	// The invoice is created first: it needs the network, and an invoice
	// left over from a failed insert just expires. It names the owner's
	// address as its Ark hint (S1), so one invoice serves Lightning and Ark
	// payers.
	let invoice = match app.forwarder.create_invoice(
		amount_sat, &ark_address, description, expiry, true,
	).await {
		Ok(i) => Some(i),
		Err(e) => {
			tracing::warn!("sale without an invoice: {:#}", e);
			None
		},
	};

	let mut conn = app.db.conn().await?;
	let tx = conn.transaction().await?;

	// Each on-chain sale gets its own address, so two open sales never
	// share one.
	let onchain = if amount_sat.to_sat() >= store.onchain_min_sat as u64 {
		match allocate_address(app, &tx, store).await {
			Ok(a) => a,
			Err(e) => {
				tracing::warn!("sale without an on-chain address: {:#}", e);
				None
			},
		}
	} else {
		None
	};
	if invoice.is_none() && onchain.is_none() {
		return Err(ApiError::Upstream(anyhow::anyhow!("no payment rail is available for the sale")));
	}

	let public_id = token::random_hex(16);
	let row = tx.query_one("
		INSERT INTO sale (public_id, store_id, link, cashier_name, amount_sat, fiat_amount,
			currency, btc_price, memo, expires_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW() + make_interval(secs => $10))
		RETURNING id
	", &[
		&public_id, &store.id, &new.link, &new.cashier_name, &(amount_sat.to_sat() as i64),
		&fiat.as_ref().map(|(f, _)| f.to_string()),
		&fiat.as_ref().map(|_| store.currency.clone()),
		&fiat.as_ref().map(|(_, p)| p.to_string()),
		&memo, &(expiry.as_secs() as f64),
	]).await?;
	let sale_id = row.get::<_, i64>("id");

	if let Some(invoice) = &invoice {
		tx.execute("
			INSERT INTO sale_payment (sale_id, rail, payment_hash, invoice)
			VALUES ($1, 'lightning', $2, $3)
		", &[&sale_id, &invoice.payment_hash().to_string(), &invoice.to_string()]).await?;
	}
	if let Some((address_id, address)) = &onchain {
		tx.execute("
			UPDATE onchain_address SET sale_id = $2, released_at = NULL WHERE id = $1
		", &[address_id, &sale_id]).await?;
		tx.execute("
			INSERT INTO sale_payment (sale_id, rail, onchain_address_id, onchain_address)
			VALUES ($1, 'onchain', $2, $3)
		", &[&sale_id, address_id, &address.to_string()]).await?;
	}

	let sale = get_sale_by_id(&tx, sale_id).await?;
	tx.commit().await?;
	Ok(sale)
}

pub async fn get_sale_by_id(conn: &impl GenericClient, id: i64) -> anyhow::Result<Sale> {
	let row = conn.query_one(&format!("{} WHERE s.id = $1", SALE_SELECT), &[&id]).await?;
	Ok(Sale::from_row(&row))
}

/// A sale by its public id. Holding the id is what lets a cashier follow
/// and cancel a sale.
pub async fn get_sale(conn: &impl GenericClient, public_id: &str) -> ApiResult<Sale> {
	let row = conn.query_opt(&format!("{} WHERE s.public_id = $1", SALE_SELECT), &[&public_id]).await?;
	row.map(|r| Sale::from_row(&r)).ok_or_else(|| not_found("no such sale"))
}

/// Sales of `store`, newest first, older than the sale `before`.
pub async fn list_sales(
	conn: &impl GenericClient,
	store_id: i64,
	before: Option<&str>,
	limit: i64,
) -> ApiResult<Vec<Sale>> {
	let limit = limit.clamp(1, 100);
	let rows = conn.query(&format!("{}
		WHERE s.store_id = $1 AND ($2::TEXT IS NULL OR s.id < (
			SELECT id FROM sale WHERE public_id = $2 AND store_id = $1))
		ORDER BY s.id DESC LIMIT $3
	", SALE_SELECT), &[&store_id, &before, &limit]).await?;
	Ok(rows.iter().map(Sale::from_row).collect())
}

/// Sales of `store` created in `[from, to)`, oldest first, as CSV.
pub async fn sales_csv(
	conn: &impl GenericClient,
	store_id: i64,
	from: Option<DateTime<Utc>>,
	to: Option<DateTime<Utc>>,
) -> ApiResult<String> {
	let rows = conn.query(&format!("{}
		WHERE s.store_id = $1 AND ($2::TIMESTAMPTZ IS NULL OR s.created_at >= $2)
			AND ($3::TIMESTAMPTZ IS NULL OR s.created_at < $3)
		ORDER BY s.id
	", SALE_SELECT), &[&store_id, &from, &to]).await?;
	let mut out = String::from(
		"id,created_at,paid_at,state,paid_rail,amount_sat,received_sat,fiat_amount,currency,\
		btc_price,memo,link,cashier_name,seen_sat,progress,warnings,notice,final_at,late_payment\n",
	);
	for row in rows {
		let s = Sale::from_row(&row);
		let late = s.state.ended_unpaid() && (s.received_sat > 0 || s.seen_sat > 0);
		let progress = s.progress.clone().filter(|_| s.state == SaleState::InProgress);
		let fields = [
			s.public_id,
			s.created_at.to_rfc3339(),
			s.paid_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
			s.state.as_str().to_string(),
			s.paid_rail.map(|r| r.as_str().to_string()).unwrap_or_default(),
			s.amount_sat.to_string(),
			s.received_sat.to_string(),
			s.fiat_amount.unwrap_or_default(),
			s.currency.unwrap_or_default(),
			s.btc_price.unwrap_or_default(),
			s.memo.unwrap_or_default(),
			s.link.as_str().to_string(),
			s.cashier_name.unwrap_or_default(),
			s.seen_sat.to_string(),
			progress.unwrap_or_default(),
			s.warnings.join(" "),
			s.notice.unwrap_or_default(),
			s.final_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
			late.to_string(),
		];
		out.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
		out.push('\n');
	}
	Ok(out)
}

/// Quotes a CSV field when needed. A leading formula character is escaped,
/// so a memo cannot run as a spreadsheet formula.
fn csv_field(s: &str) -> String {
	let s = if s.starts_with(['=', '+', '-', '@']) { format!("'{}", s) } else { s.to_string() };
	if s.contains([',', '"', '\n', '\r']) {
		format!("\"{}\"", s.replace('"', "\"\""))
	} else {
		s
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn builds_payment_requests() {
		assert_eq!(
			payment_request(Some("bc1qaddr"), Amount::from_sat(35_000), Some("lnbc1a")),
			"bitcoin:bc1qaddr?amount=0.00035&lightning=lnbc1a",
		);
		assert_eq!(
			payment_request(None, Amount::from_sat(1_000), Some("lnbc1a")),
			"bitcoin:?lightning=lnbc1a",
		);
		// One invoice only: no second Ark invoice.
		assert!(!payment_request(None, Amount::from_sat(1_000), Some("lnbc1a")).contains("ark-lightning"));
		// On-chain only, when the invoice rail is unavailable.
		assert_eq!(
			payment_request(Some("bc1qaddr"), Amount::from_sat(100_000_000), None),
			"bitcoin:bc1qaddr?amount=1",
		);
		assert_eq!(btc_amount(Amount::from_sat(100_000_000)), "1");
		assert_eq!(btc_amount(Amount::from_sat(1)), "0.00000001");
	}

	#[test]
	fn gap_limit() {
		// No address used yet: indexes 0 to 19 for a gap limit of 20.
		assert!(within_gap(0, None, 20));
		assert!(within_gap(19, None, 20));
		assert!(!within_gap(20, None, 20));
		// The gap counts from the last address that received anything.
		assert!(within_gap(25, Some(5), 20));
		assert!(!within_gap(26, Some(5), 20));
		assert!(within_gap(1005, Some(5), 1000));
	}

	#[test]
	fn escapes_csv() {
		assert_eq!(csv_field("plain"), "plain");
		assert_eq!(csv_field("a,b"), "\"a,b\"");
		assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
		assert_eq!(csv_field("=SUM(A1)"), "'=SUM(A1)");
	}
}
