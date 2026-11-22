//! The cashier link, which is also the store's public checkout.
//!
//! A store has one link, `/<nym>`, always on and open to anyone. Any device
//! with it acts as a cashier: there is no pairing, no device token and no
//! login. The link can create a sale, and whoever holds a sale's
//! unguessable id can follow it and cancel it while it is open. The link
//! cannot list the store's sales or export them: the history and the CSV
//! are behind the owner login.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::App;
use crate::api::{StoreView, check_lookup};
use crate::protect::{Caller, Surface};
use crate::database::{STORE_COLUMNS, SaleState, Store};
use crate::error::{ApiError, ApiResult, bad_request, conflict, not_found};
use crate::money::Decimal;
use crate::sales::{self, NewSale, SaleAmount, SaleView};
use crate::watcher;


/// The longest a long-poll waits.
const LONG_POLL: Duration = Duration::from_secs(25);

/// The longest cashier name.
const MAX_CASHIER_NAME_CHARS: usize = 64;

/// The store of a nym.
async fn store_of(app: &App, nym: &str) -> ApiResult<Store> {
	let conn = app.public.db.conn().await?;
	let row = conn.query_opt(&format!(
		"SELECT {} FROM store s JOIN nym n ON n.id = s.nym_id WHERE n.name = $1", STORE_COLUMNS,
	), &[&nym]).await?;
	row.map(|r| Store::from_row(&r)).ok_or_else(|| not_found("no such store"))
}

#[derive(Deserialize)]
pub struct CreateSale {
	fiat_amount: Option<String>,
	amount_sat: Option<u64>,
	memo: Option<String>,
	/// A name typed on the cashier screen. It is shown in the history and
	/// never verified.
	cashier_name: Option<String>,
}

/// `GET /api/v1/checkout/<nym>`: the store behind its link. Viewing
/// allocates nothing.
pub async fn store(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(nym): Path<String>,
) -> ApiResult<Json<Value>> {
	check_lookup(&app, &caller, &nym).await?;
	let store = store_of(&app, &nym).await?;
	Ok(Json(json!({ "store": StoreView::new(&app, &store) })))
}

/// `GET /api/v1/checkout/<nym>/rate`.
pub async fn rate(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(nym): Path<String>,
) -> ApiResult<Json<Value>> {
	app.protect.check(&caller, Surface::StatusRead)?;
	let store = store_of(&app, &nym).await?;
	let quote = app.price.quote(&store.currency).await.map_err(ApiError::Upstream)?;
	Ok(Json(json!({
		"currency": quote.currency,
		"btc_price": quote.btc_price.to_string(),
		"fetched_at": quote.fetched_at,
	})))
}

/// `POST /api/v1/checkout/<nym>/sales`.
pub async fn create_sale(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(nym): Path<String>,
	Json(req): Json<CreateSale>,
) -> ApiResult<Json<SaleView>> {
	app.protect.check(&caller, Surface::SaleCreate)?;
	let store = store_of(&app, &nym).await?;
	let amount = match (req.fiat_amount, req.amount_sat) {
		(Some(fiat), None) => SaleAmount::Fiat(fiat.parse::<Decimal>()
			.map_err(|e| bad_request(e.to_string()))?),
		(None, Some(sat)) => SaleAmount::Sat(bitcoin::Amount::from_sat(sat)),
		_ => return Err(bad_request("give exactly one of fiat_amount and amount_sat")),
	};
	let cashier_name = req.cashier_name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
	if cashier_name.as_ref().is_some_and(|n| n.chars().count() > MAX_CASHIER_NAME_CHARS || n.contains(['\n', '\r'])) {
		return Err(bad_request("the cashier name must be at most 64 characters on one line"));
	}
	// Open, unpaid sales are capped per store, counted and reserved in one
	// transaction; sales that get paid free their place.
	let sale = sales::create_sale(&app, &store, NewSale {
		cashier_name,
		amount,
		memo: req.memo,
	}).await?;
	Ok(Json(sale.into()))
}

#[derive(Deserialize)]
pub struct WaitQuery {
	wait: Option<String>,
}

/// `GET /api/v1/sales/<id>`: one sale, for whoever holds its id. With
/// `?wait=<state>` it returns when the sale leaves that state, or after
/// 25 seconds.
pub async fn get_sale(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(id): Path<String>,
	Query(q): Query<WaitQuery>,
) -> ApiResult<Json<SaleView>> {
	app.protect.check(&caller, Surface::StatusRead)?;
	let wait = q.wait.map(|w| w.parse::<SaleState>().map_err(|e| bad_request(e.to_string())))
		.transpose()?;
	let deadline = tokio::time::Instant::now() + LONG_POLL;
	loop {
		// Subscribe before reading, so a change between the read and the
		// wait is not missed.
		let mut changes = app.sale_changes.subscribe();
		let sale = {
			let conn = app.public.db.conn().await?;
			sales::get_sale(&*conn, &id).await?
		};
		let done = match wait {
			Some(state) => sale.state != state,
			None => true,
		};
		let now = tokio::time::Instant::now();
		if done || now >= deadline {
			return Ok(Json(sale.into()));
		}
		let _ = tokio::time::timeout(deadline - now, changes.changed()).await;
	}
}

/// `POST /api/v1/sales/<id>/cancel`: cancels a sale that is still open. A
/// sale with a payment in progress cannot be canceled.
pub async fn cancel_sale(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(id): Path<String>,
) -> ApiResult<Json<SaleView>> {
	app.protect.check(&caller, Surface::SaleCreate)?;
	let sale = {
		let conn = app.public.db.conn().await?;
		sales::get_sale(&*conn, &id).await?
	};
	if sale.state != SaleState::Open {
		return Err(conflict(format!("the sale is {}", sale.state)));
	}
	// The invoice is canceled first. barkd refuses once a payment is
	// arriving, and the sale then stays open for it.
	if let Some(hash) = watcher::invoice_hash(&app.public, sale.id).await? {
		match watcher::cancel_invoice(&app.public, &hash).await {
			Ok(()) => {},
			Err(watcher::CancelError::PaymentArriving) => {
				return Err(conflict("a payment is arriving for this sale"));
			},
			Err(watcher::CancelError::Unavailable(e)) => return Err(ApiError::Upstream(e)),
		}
	}
	let sale = {
		let conn = app.public.db.conn().await?;
		let n = conn.execute("
			UPDATE sale SET state = 'canceled', updated_at = NOW()
			WHERE public_id = $1 AND state = 'open'
		", &[&id]).await?;
		let sale = sales::get_sale(&*conn, &id).await?;
		if n == 0 {
			return Err(conflict(format!("the sale is {}", sale.state)));
		}
		sale
	};
	watcher::sale_ended(&app.public, sale.id).await;
	app.notify_sale_change();
	Ok(Json(sale.into()))
}
