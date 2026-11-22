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
	pub received_sat: i64,
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

/// Creates a sale with all the rails it offers: one invoice for Lightning
/// and Ark payers, and an address above the store's on-chain minimum.
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
	let invoice = app.forwarder.create_invoice(
		amount_sat, &ark_address, description, expiry, true,
	).await.map_err(ApiError::Upstream)?;

	let mut conn = app.db.conn().await?;
	let tx = conn.transaction().await?;

	let onchain = if amount_sat.to_sat() >= store.onchain_min_sat as u64 {
		// Each on-chain sale reserves its own index, so two open sales never
		// share an address.
		let row = tx.query_one("
			UPDATE store SET next_onchain_index = next_onchain_index + 1, updated_at = NOW()
			WHERE id = $1 RETURNING next_onchain_index - 1 AS idx
		", &[&store.id]).await?;
		let index = row.get::<_, i64>("idx");
		let desc = StoreDescriptor::parse(&store.onchain_descriptor)
			.context("store has an invalid descriptor")?;
		let index_u32 = u32::try_from(index).context("on-chain index overflow")?;
		let address = desc.address(index_u32, app.cfg.network)?;
		app.chain.watch(&address).await.map_err(ApiError::Upstream)?;
		Some((index, address))
	} else {
		None
	};

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

	tx.execute("
		INSERT INTO sale_payment (sale_id, rail, payment_hash, invoice)
		VALUES ($1, 'lightning', $2, $3)
	", &[&sale_id, &invoice.payment_hash().to_string(), &invoice.to_string()]).await?;
	if let Some((index, address)) = &onchain {
		tx.execute("
			INSERT INTO sale_payment (sale_id, rail, onchain_index, onchain_address)
			VALUES ($1, 'onchain', $2, $3)
		", &[&sale_id, index, &address.to_string()]).await?;
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
		btc_price,memo,link,cashier_name\n",
	);
	for row in rows {
		let s = Sale::from_row(&row);
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
