//! The cashier link, which is also the store's public checkout.
//!
//! A store has one link, `/<nym>`, always on and open to anyone. Any device
//! with it acts as a cashier: there is no pairing, no device token and no
//! login. The link can create a sale, and whoever holds a sale's
//! unguessable id can follow it, add Bitcoin to it and cancel it while it
//! is open. A sale has only its Lightning invoice until Bitcoin is added.
//! The link cannot list the store's sales or export them: the history and
//! the CSV are behind the owner login. The cashier screen keeps the ids of the
//! sales it created, and shows them only after the service checked the
//! owner's cashier PIN (see [crate::pin]).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::App;
use crate::api::{StoreView, check_lookup};
use crate::pin;
use crate::protect::{Caller, Surface};
use crate::database::{STORE_COLUMNS, SaleState, Store};
use crate::error::{ApiError, ApiResult, bad_request, conflict, not_found};
use crate::money::Decimal;
use crate::sales::{self, NewSale, SaleAmount, SaleView};
use crate::watcher;


/// The longest a long-poll waits.
const LONG_POLL: Duration = Duration::from_secs(25);

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
	// Open, unpaid sales are capped per store, counted and reserved in one
	// transaction; sales that get paid free their place.
	let sale = sales::create_sale(&app, &store, NewSale {
		amount,
		memo: req.memo,
	}).await?;
	Ok(Json(sale.into()))
}

#[derive(Deserialize)]
pub struct CheckPin {
	pin: String,
}

/// The answer to a PIN check that did not unlock the list.
pub enum PinRefusal {
	/// The PIN is wrong. Further checks are locked for `retry_after`
	/// seconds, 0 while wrong PINs are still free.
	Wrong { retry_after: i64 },
	/// Too many wrong PINs: checks are locked for `retry_after` seconds.
	Locked { retry_after: i64 },
	Api(ApiError),
}

impl From<ApiError> for PinRefusal {
	fn from(e: ApiError) -> PinRefusal {
		PinRefusal::Api(e)
	}
}

impl From<anyhow::Error> for PinRefusal {
	fn from(e: anyhow::Error) -> PinRefusal {
		PinRefusal::Api(ApiError::Internal(e))
	}
}

impl IntoResponse for PinRefusal {
	fn into_response(self) -> Response {
		let (status, error, retry_after) = match self {
			PinRefusal::Wrong { retry_after } => (StatusCode::UNAUTHORIZED, "wrong PIN", retry_after),
			PinRefusal::Locked { retry_after } => {
				(StatusCode::TOO_MANY_REQUESTS, "too many wrong PINs; try again later", retry_after)
			},
			PinRefusal::Api(e) => return e.into_response(),
		};
		let mut resp = (status, Json(json!({ "error": error, "retry_after": retry_after }))).into_response();
		if retry_after > 0 {
			resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
		}
		resp
	}
}

/// `POST /api/v1/checkout/<nym>/pin`: checks the cashier PIN, which
/// unlocks the screen's list of its own sales. The answer says only
/// whether it matched.
pub async fn check_pin(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(nym): Path<String>,
	Json(req): Json<CheckPin>,
) -> Result<Json<Value>, PinRefusal> {
	app.protect.check(&caller, Surface::PinCheck)?;
	let store = store_of(&app, &nym).await?;
	if req.pin.len() > pin::MAX_PIN_DIGITS * 4 {
		return Err(bad_request("the PIN is too long").into());
	}
	let scopes = pin::scopes(store.id, &caller.source);
	// The check is admitted, verified and counted in one transaction that
	// holds the store's and the source's counters, so concurrent checks
	// cannot pass the limits together. The backoff holds for every caller,
	// certified or not.
	let mut conn = app.public.db.conn().await?;
	let tx = conn.transaction().await.map_err(ApiError::from)?;
	let locked = pin::admit(&tx, &scopes).await?;
	if locked > 0 {
		return Err(PinRefusal::Locked { retry_after: locked });
	}
	let hash: Option<String> = tx.query_one("SELECT cashier_pin_hash FROM store WHERE id = $1", &[&store.id])
		.await.map_err(ApiError::from)?.get("cashier_pin_hash");
	let Some(hash) = hash else {
		return Err(not_found("the owner has not set a cashier PIN").into());
	};
	let given = req.pin;
	let matches = tokio::task::spawn_blocking(move || pin::verify(&given, &hash)).await
		.map_err(|e| anyhow::anyhow!("PIN check task: {}", e))?;
	if !matches {
		let retry_after = pin::record_failure(&tx, &scopes).await?;
		tx.commit().await.map_err(ApiError::from)?;
		tracing::info!("wrong cashier PIN for store {} from {}", store.id, caller.source);
		return Err(PinRefusal::Wrong { retry_after });
	}
	tx.commit().await.map_err(ApiError::from)?;
	Ok(Json(json!({ "ok": true })))
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

/// `POST /api/v1/sales/<id>/onchain`: adds a Bitcoin address to an open
/// sale, at the merchant's request. A second call returns the same address.
pub async fn add_onchain(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(id): Path<String>,
) -> ApiResult<Json<SaleView>> {
	app.protect.check(&caller, Surface::SaleCreate)?;
	let sale = sales::add_onchain(&app, &id).await?;
	Ok(Json(sale.into()))
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
			Err(watcher::CancelError::PaymentArriving(_) | watcher::CancelError::UnknownState) => {
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
