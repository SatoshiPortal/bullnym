//! Creating sales, and how a sale looks to the API.

use std::str::FromStr;

use anyhow::Context;
use bitcoin::{Amount, Denomination};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio_postgres::GenericClient;

use crate::App;
use crate::database::{Rail, SALE_SELECT, Sale, SaleState, Store};
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

/// The advisory lock under which the caps on open sales and open invoices
/// are counted and a new sale or invoice is reserved, so parallel requests
/// cannot pass a cap together.
pub const CAPS_LOCK: &str = "bullnym:open-caps";

/// Takes the caps lock for the rest of `tx`.
pub async fn lock_caps(tx: &tokio_postgres::Transaction<'_>) -> Result<(), tokio_postgres::Error> {
	tx.execute("SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)", &[&CAPS_LOCK]).await?;
	Ok(())
}

/// A new sale, reserved under the caps before any backend call: the sale
/// row, and the slot of its invoice when it may have one.
struct Reservation {
	sale_id: i64,
	invoice_slot: Option<i64>,
}

/// Reserves a sale in one transaction with the count of the caps: the
/// store's open sales, and whether the sale may have an invoice (barkd and
/// the Ark server healthy, the cap on open invoices not reached, room in
/// the public invoice budget).
async fn reserve_sale(
	app: &App,
	store: &Store,
	cashier_name: &Option<String>,
	memo: &Option<String>,
	amount_sat: Amount,
	fiat: &Option<(Decimal, Decimal)>,
) -> ApiResult<Reservation> {
	let mut conn = app.public.db.conn().await?;
	let tx = conn.transaction().await?;
	lock_caps(&tx).await?;
	let open_sales: i64 = tx.query_one(
		"SELECT count(*) FROM sale WHERE store_id = $1 AND state = 'open'", &[&store.id],
	).await?.get(0);
	if open_sales >= app.protect.limits.max_open_sales_per_store as i64 {
		tracing::info!("store {} has {} open sales, its cap", store.id, open_sales);
		return Err(ApiError::TooManyRequests);
	}
	let invoice_ok = if !app.protect.health.invoices() {
		tracing::info!("sale without an invoice: barkd or the Ark server is unhealthy");
		false
	} else if crate::api::open_invoices(&tx).await? >= app.protect.limits.max_open_invoices as i64 {
		tracing::info!("sale without an invoice: the open invoice cap is reached");
		false
	} else if app.protect.take_public_invoice().is_err() {
		tracing::info!("sale without an invoice: the public invoice budget is spent");
		false
	} else {
		true
	};

	let public_id = token::random_hex(16);
	let sale_id: i64 = tx.query_one("
		INSERT INTO sale (public_id, store_id, cashier_name, amount_sat, fiat_amount,
			currency, btc_price, memo, expires_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW() + make_interval(secs => $9))
		RETURNING id
	", &[
		&public_id, &store.id, cashier_name, &(amount_sat.to_sat() as i64),
		&fiat.as_ref().map(|(f, _)| f.to_string()),
		&fiat.as_ref().map(|_| store.currency.clone()),
		&fiat.as_ref().map(|(_, p)| p.to_string()),
		memo, &(app.cfg.sales.expiry().as_secs() as f64),
	]).await?.get("id");
	// The invoice slot counts against the cap on open invoices from now on.
	let invoice_slot = if invoice_ok {
		Some(tx.query_one("
			INSERT INTO sale_payment (sale_id, rail) VALUES ($1, 'lightning') RETURNING id
		", &[&sale_id]).await?.get::<_, i64>("id"))
	} else {
		None
	};
	tx.commit().await?;
	Ok(Reservation { sale_id, invoice_slot })
}

/// Hands out the store's next on-chain address, as posd does: each sale
/// that offers Bitcoin takes the next derivation index, so two sales never
/// share an address and no address is handed out twice. The index is taken
/// only if the address is watched; otherwise the sale offers no on-chain
/// payment and the index stays free.
async fn next_address(
	app: &App,
	tx: &mut tokio_postgres::Transaction<'_>,
	store: &Store,
) -> anyhow::Result<(i64, bitcoin::Address)> {
	let sp = tx.savepoint("onchain_index").await?;
	let row = sp.query_one("
		UPDATE store SET next_onchain_index = next_onchain_index + 1, updated_at = NOW()
		WHERE id = $1 RETURNING next_onchain_index - 1 AS idx
	", &[&store.id]).await?;
	let index = row.get::<_, i64>("idx");
	let desc = StoreDescriptor::parse(&store.onchain_descriptor).context("store has an invalid descriptor")?;
	let address = desc.address(u32::try_from(index).context("on-chain index overflow")?, app.cfg.network)?;
	app.public.chain.watch(&address).await?;
	sp.commit().await?;
	Ok((index, address))
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

	// The sale and its invoice slot are reserved first, under the caps.
	// Each backend step then fills or releases its part.
	let res = reserve_sale(app, store, &new.cashier_name, &memo, amount_sat, &fiat).await?;

	// The invoice names the owner's address as its Ark hint (S1), so one
	// invoice serves Lightning and Ark payers.
	let invoice = match res.invoice_slot {
		Some(slot) => match app.public.forwarder.create_invoice(
			amount_sat, &ark_address, description, expiry, true,
		).await {
			Ok(invoice) => {
				let conn = app.public.db.conn().await?;
				conn.execute("
					UPDATE sale_payment SET payment_hash = $2, invoice = $3 WHERE id = $1
				", &[&slot, &invoice.payment_hash().to_string(), &invoice.to_string()]).await?;
				true
			},
			Err(e) => {
				tracing::warn!("sale without an invoice: {:#}", e);
				let conn = app.public.db.conn().await?;
				conn.execute("DELETE FROM sale_payment WHERE id = $1", &[&slot]).await?;
				false
			},
		},
		None => false,
	};

	let onchain = if amount_sat.to_sat() >= store.onchain_min_sat as u64 && app.protect.health.onchain() {
		let mut conn = app.public.db.conn().await?;
		let mut tx = conn.transaction().await?;
		match next_address(app, &mut tx, store).await {
			Ok((index, address)) => {
				tx.execute("
					INSERT INTO sale_payment (sale_id, rail, onchain_index, onchain_address)
					VALUES ($1, 'onchain', $2, $3)
				", &[&res.sale_id, &index, &address.to_string()]).await?;
				tx.commit().await?;
				true
			},
			Err(e) => {
				tracing::warn!("sale without an on-chain address: {:#}", e);
				false
			},
		}
	} else {
		false
	};

	let conn = app.public.db.conn().await?;
	if !invoice && !onchain {
		// Nobody saw this sale: release it.
		conn.execute("DELETE FROM sale_payment WHERE sale_id = $1", &[&res.sale_id]).await?;
		conn.execute("DELETE FROM sale WHERE id = $1", &[&res.sale_id]).await?;
		return Err(ApiError::Upstream(anyhow::anyhow!("no payment rail is available for the sale")));
	}
	Ok(get_sale_by_id(&*conn, res.sale_id).await?)
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
		btc_price,memo,cashier_name,seen_sat,progress,warnings,notice,final_at,late_payment\n",
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
	fn escapes_csv() {
		assert_eq!(csv_field("plain"), "plain");
		assert_eq!(csv_field("a,b"), "\"a,b\"");
		assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
		assert_eq!(csv_field("=SUM(A1)"), "'=SUM(A1)");
	}
}
