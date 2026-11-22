//! Creating sales, and how a sale looks to the API.

use std::str::FromStr;

use anyhow::Context;
use ark::lightning::Bolt11InvoiceExt;
use bitcoin::{Amount, Denomination};
use chrono::{DateTime, Utc};
use lightning_invoice::Bolt11Invoice;
use serde::Serialize;
use tokio_postgres::GenericClient;

use crate::App;
use crate::database::{Rail, SALE_SELECT, Sale, SaleState, Store};
use crate::error::{ApiError, ApiResult, bad_request, not_found};
use crate::money::{Decimal, MAX_FIAT_INTEGER, MAX_SAT, currency_decimals};
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
	/// The sale ended unpaid and a payment came after: the owner sees it,
	/// and it never pays another sale.
	pub late_payment: bool,
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
			late_payment: s.state.ended_unpaid() && (s.received_sat > 0 || s.seen_sat > 0),
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
			if fiat.integer_part() >= MAX_FIAT_INTEGER {
				return Err(bad_request("the amount is too large"));
			}
			let quote = app.price.quote(&store.currency).await.map_err(ApiError::Upstream)?;
			let sat = fiat.fiat_to_sat(&quote.btc_price)
				.ok_or_else(|| bad_request("the amount is too large"))?;
			(sat, Some((fiat, quote.btc_price)))
		},
	};
	check_sale_amount(amount_sat)?;
	Ok((amount_sat, fiat))
}

/// A sale is for at least 1 sat and at most [MAX_SAT], checked before
/// anything is allocated for it.
pub fn check_sale_amount(amount: Amount) -> ApiResult<()> {
	if amount == Amount::ZERO {
		return Err(bad_request("the amount must be at least 1 sat"));
	}
	if amount.to_sat() > MAX_SAT {
		return Err(bad_request("the amount is too large"));
	}
	Ok(())
}

/// Why a store invoice from barkd cannot be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HintError {
	#[error("the Ark server must allow Ark address hints: barkd returned the sale's invoice \
		without the store's Ark address in its route hints")]
	Missing,
	#[error("barkd returned the sale's invoice with an Ark address that is not the store's")]
	Other,
}

/// Checks that a store invoice names the store's Ark address as its Ark
/// hint, as posd checked its hinted invoice. A Scuttle payer pays that
/// address instead of the invoice, so any other address is refused.
pub fn check_invoice_hint(invoice: &Bolt11Invoice, store_address: &ark::Address) -> Result<(), HintError> {
	match invoice.ark_address() {
		Some(a) if a.to_string() == store_address.to_string() => Ok(()),
		Some(_) => Err(HintError::Other),
		None => Err(HintError::Missing),
	}
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
		INSERT INTO sale (public_id, store_id, amount_sat, fiat_amount, currency, btc_price, memo,
			expires_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, NOW() + make_interval(secs => $8))
		RETURNING id
	", &[
		&public_id, &store.id, &(amount_sat.to_sat() as i64),
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

/// The most addresses one sale skips because another store issued them.
const MAX_ADDRESS_SKIPS: u32 = 1_000;

/// Hands out the store's next on-chain address, as posd does: each sale
/// that offers Bitcoin takes the next derivation index, so no address is
/// handed out twice. The index is taken only if the address is watched;
/// otherwise the sale offers no on-chain payment and the index stays free.
///
/// Two stores may register the same descriptor. An address another sale
/// already has, from any store, is skipped and its index used up, so every
/// address belongs to one sale and a payment to it counts for that sale
/// only. The address stays locked until `tx` commits the sale's row with
/// it, so simultaneous checkouts of two such stores cannot both take it.
async fn next_address(
	app: &App,
	tx: &mut tokio_postgres::Transaction<'_>,
	store: &Store,
) -> anyhow::Result<(i64, bitcoin::Address)> {
	let desc = StoreDescriptor::parse(&store.onchain_descriptor).context("store has an invalid descriptor")?;
	let sp = tx.savepoint("onchain_index").await?;
	for _ in 0..=MAX_ADDRESS_SKIPS {
		let index: i64 = sp.query_one("
			UPDATE store SET next_onchain_index = next_onchain_index + 1, updated_at = NOW()
			WHERE id = $1 RETURNING next_onchain_index - 1 AS idx
		", &[&store.id]).await?.get("idx");
		let address = desc.address(u32::try_from(index).context("on-chain index overflow")?, app.cfg.network)?;
		let text = address.to_string();
		sp.execute(
			"SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)", &[&format!("bullnym:address:{}", text)],
		).await?;
		let taken: bool = sp.query_one(
			"SELECT EXISTS (SELECT 1 FROM sale_payment WHERE onchain_address = $1)", &[&text],
		).await?.get(0);
		if taken {
			tracing::info!("store {} skips index {}: another sale has its address", store.id, index);
			continue;
		}
		app.public.chain.watch(&address).await?;
		sp.commit().await?;
		return Ok((index, address));
	}
	anyhow::bail!("{} addresses in a row of store {} belong to other sales", MAX_ADDRESS_SKIPS + 1, store.id)
}

/// Creates a sale with all the rails it offers: one invoice for Lightning
/// and Ark payers, and an address above the store's on-chain minimum. A
/// rail whose backend fails is left out; with no rail left, the sale is
/// refused with the fixed "unavailable" message. An invoice without the
/// store's Ark hint refuses the sale, as in posd, with its own message.
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
	let res = reserve_sale(app, store, &memo, amount_sat, &fiat).await?;

	// The invoice names the owner's address as its Ark hint (S1), so one
	// invoice serves Lightning and Ark payers.
	let invoice = match res.invoice_slot {
		Some(slot) => match app.public.forwarder.create_invoice(
			amount_sat, &ark_address, description, expiry, true,
		).await {
			Ok(invoice) => {
				let hash = invoice.payment_hash().to_string();
				match check_invoice_hint(&invoice, &ark_address) {
					Ok(()) => {},
					Err(HintError::Missing) if app.overrides.accept_unhinted_invoices => {
						tracing::warn!("test override: sale invoice {} has no Ark hint", hash);
					},
					Err(e) => {
						tracing::error!("sale refused: {} (invoice {})", e, hash);
						if let Err(e) = app.public.forwarder.cancel(&hash).await {
							tracing::warn!("failed to cancel invoice {}: {:#}", hash, e);
						}
						release_sale(app, res.sale_id).await?;
						return Err(ApiError::Misconfigured(e.to_string()));
					},
				}
				let conn = app.public.db.conn().await?;
				conn.execute("
					UPDATE sale_payment SET payment_hash = $2, invoice = $3 WHERE id = $1
				", &[&slot, &hash, &invoice.to_string()]).await?;
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
		let taken = match next_address(app, &mut tx, store).await {
			// The address column is unique: should another sale have the
			// address after all, this sale offers no on-chain payment.
			Ok((index, address)) => tx.execute("
				INSERT INTO sale_payment (sale_id, rail, onchain_index, onchain_address)
				VALUES ($1, 'onchain', $2, $3)
			", &[&res.sale_id, &index, &address.to_string()]).await.map_err(anyhow::Error::from),
			Err(e) => Err(e),
		};
		match taken {
			Ok(_) => {
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

	if !invoice && !onchain {
		release_sale(app, res.sale_id).await?;
		return Err(ApiError::Upstream(anyhow::anyhow!("no payment rail is available for the sale")));
	}
	let conn = app.public.db.conn().await?;
	Ok(get_sale_by_id(&*conn, res.sale_id).await?)
}

/// Deletes a reserved sale that nobody saw, with its rails.
async fn release_sale(app: &App, sale_id: i64) -> ApiResult<()> {
	let conn = app.public.db.conn().await?;
	conn.execute("DELETE FROM sale_payment WHERE sale_id = $1", &[&sale_id]).await?;
	conn.execute("DELETE FROM sale WHERE id = $1", &[&sale_id]).await?;
	Ok(())
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
		btc_price,memo,seen_sat,progress,warnings,notice,late_payment\n",
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
			s.seen_sat.to_string(),
			progress.unwrap_or_default(),
			s.warnings.join(" "),
			s.notice.unwrap_or_default(),
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

	fn key(byte: u8) -> bitcoin::secp256k1::Keypair {
		let secp = bitcoin::secp256k1::Secp256k1::new();
		bitcoin::secp256k1::Keypair::from_seckey_slice(&secp, &[byte; 32]).unwrap()
	}

	fn address(owner: u8) -> ark::Address {
		let k = key(owner);
		ark::Address::builder()
			.testnet(true)
			.server_pubkey(key(3).public_key())
			.pubkey_policy(k.public_key())
			.mailbox(key(4).public_key(), ark::mailbox::MailboxIdentifier::from_pubkey(k.public_key()), &k)
			.unwrap()
			.into_address().unwrap()
	}

	fn invoice(hint: Option<&ark::Address>) -> Bolt11Invoice {
		use bitcoin::hashes::{Hash, sha256};
		use lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret, RouteHint, RouteHintHop, RoutingFees};
		let secp = bitcoin::secp256k1::Secp256k1::new();
		let mut builder = InvoiceBuilder::new(Currency::Regtest)
			.description("test".into())
			.payment_hash(sha256::Hash::hash(&[0; 32]))
			.payment_secret(PaymentSecret([0; 32]))
			.duration_since_epoch(std::time::Duration::from_secs(1_700_000_000))
			.min_final_cltv_expiry_delta(144)
			.amount_milli_satoshis(1_000_000);
		for (src_node_id, short_channel_id) in hint.map(|a| ark::lightning::ark_address_route_hints(a).unwrap())
			.into_iter().flatten()
		{
			builder = builder.private_route(RouteHint(vec![RouteHintHop {
				src_node_id, short_channel_id,
				fees: RoutingFees { base_msat: 0, proportional_millionths: 0 },
				cltv_expiry_delta: 0, htlc_minimum_msat: None, htlc_maximum_msat: None,
			}]));
		}
		builder.build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &key(1).secret_key())).unwrap()
	}

	#[test]
	fn store_invoices_name_the_store_address() {
		let store = address(5);
		assert_eq!(check_invoice_hint(&invoice(Some(&store)), &store), Ok(()));
		// No hint: the Ark server does not allow hints, or barkd does not
		// pass the request on (a Scuttle release without S1).
		assert_eq!(check_invoice_hint(&invoice(None), &store), Err(HintError::Missing));
		assert!(HintError::Missing.to_string().contains("must allow Ark address hints"));
		// Another address, such as barkd's own: an Ark payer would pay it.
		assert_eq!(check_invoice_hint(&invoice(Some(&address(6))), &store), Err(HintError::Other));
	}

	#[test]
	fn sale_amount_bounds() {
		assert!(check_sale_amount(Amount::from_sat(1)).is_ok());
		assert!(check_sale_amount(Amount::from_sat(MAX_SAT)).is_ok());
		assert!(check_sale_amount(Amount::ZERO).is_err());
		assert!(check_sale_amount(Amount::from_sat(MAX_SAT + 1)).is_err());
		assert!(check_sale_amount(Amount::from_sat(u64::MAX)).is_err(), "would wrap in a signed column");
	}

	#[test]
	fn escapes_csv() {
		assert_eq!(csv_field("plain"), "plain");
		assert_eq!(csv_field("a,b"), "\"a,b\"");
		assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
		assert_eq!(csv_field("=SUM(A1)"), "'=SUM(A1)");
	}
}
