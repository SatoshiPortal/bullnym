//! The cashier link, which is also the store's public checkout.
//!
//! Any device with the link acts as a cashier: there is no pairing, no
//! device token and no login. The link can create a sale, and whoever holds
//! a sale's unguessable id can follow it and cancel it while it is open.
//! The link cannot list the store's sales or export them: the history and
//! the CSV are behind the owner login.
//!
//! A store has two kinds of link:
//! - public: `/<nym>`, open to anyone, unless the owner turns it off;
//! - unlisted: `/c/<secret>`, which the owner shares with staff and can
//!   rotate, which kills the previous one.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::App;
use crate::api::StoreView;
use crate::database::{CheckoutLink, STORE_COLUMNS, SaleState, Store};
use crate::error::{ApiError, ApiResult, bad_request, conflict, not_found};
use crate::money::Decimal;
use crate::sales::{self, NewSale, SaleAmount, SaleView};
use crate::{token, watcher};


/// The longest a long-poll waits.
const LONG_POLL: Duration = Duration::from_secs(25);

/// The longest cashier name.
const MAX_CASHIER_NAME_CHARS: usize = 64;

/// The store behind a cashier link.
struct Checkout {
	store: Store,
	link: CheckoutLink,
}

async fn by_nym(app: &App, nym: &str) -> ApiResult<Checkout> {
	let conn = app.db.conn().await?;
	let row = conn.query_opt(&format!(
		"SELECT {} FROM store s JOIN nym n ON n.id = s.nym_id WHERE n.name = $1", STORE_COLUMNS,
	), &[&nym]).await?;
	let store = row.map(|r| Store::from_row(&r)).filter(|s| s.public_checkout)
		.ok_or_else(|| not_found("no such store"))?;
	Ok(Checkout { store, link: CheckoutLink::Public })
}

async fn by_secret(app: &App, secret: &str) -> ApiResult<Checkout> {
	let conn = app.db.conn().await?;
	let row = conn.query_opt(&format!(
		"SELECT {} FROM store s JOIN nym n ON n.id = s.nym_id WHERE s.unlisted_link_hash = $1",
		STORE_COLUMNS,
	), &[&token::hash(secret)]).await?;
	let store = row.map(|r| Store::from_row(&r))
		.ok_or_else(|| not_found("this cashier link does not work any more"))?;
	Ok(Checkout { store, link: CheckoutLink::Unlisted })
}

fn store_view(app: &App, c: &Checkout) -> Json<Value> {
	Json(json!({ "store": StoreView::new(app, &c.store), "link": c.link }))
}

async fn rate(app: &App, c: &Checkout) -> ApiResult<Json<Value>> {
	let quote = app.price.quote(&c.store.currency).await.map_err(ApiError::Upstream)?;
	Ok(Json(json!({
		"currency": quote.currency,
		"btc_price": quote.btc_price.to_string(),
		"fetched_at": quote.fetched_at,
	})))
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

async fn create_sale(app: &App, c: Checkout, req: CreateSale) -> ApiResult<Json<SaleView>> {
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
	let sale = sales::create_sale(app, &c.store, NewSale {
		link: c.link,
		cashier_name,
		amount,
		memo: req.memo,
	}).await?;
	Ok(Json(sale.into()))
}

pub async fn public_store(State(app): State<Arc<App>>, Path(nym): Path<String>) -> ApiResult<Json<Value>> {
	Ok(store_view(&app, &by_nym(&app, &nym).await?))
}

pub async fn public_rate(State(app): State<Arc<App>>, Path(nym): Path<String>) -> ApiResult<Json<Value>> {
	rate(&app, &by_nym(&app, &nym).await?).await
}

pub async fn public_create_sale(
	State(app): State<Arc<App>>,
	Path(nym): Path<String>,
	Json(req): Json<CreateSale>,
) -> ApiResult<Json<SaleView>> {
	let c = by_nym(&app, &nym).await?;
	create_sale(&app, c, req).await
}

pub async fn unlisted_store(State(app): State<Arc<App>>, Path(secret): Path<String>) -> ApiResult<Json<Value>> {
	Ok(store_view(&app, &by_secret(&app, &secret).await?))
}

pub async fn unlisted_rate(State(app): State<Arc<App>>, Path(secret): Path<String>) -> ApiResult<Json<Value>> {
	rate(&app, &by_secret(&app, &secret).await?).await
}

pub async fn unlisted_create_sale(
	State(app): State<Arc<App>>,
	Path(secret): Path<String>,
	Json(req): Json<CreateSale>,
) -> ApiResult<Json<SaleView>> {
	let c = by_secret(&app, &secret).await?;
	create_sale(&app, c, req).await
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
	Path(id): Path<String>,
	Query(q): Query<WaitQuery>,
) -> ApiResult<Json<SaleView>> {
	let wait = q.wait.map(|w| w.parse::<SaleState>().map_err(|e| bad_request(e.to_string())))
		.transpose()?;
	let deadline = tokio::time::Instant::now() + LONG_POLL;
	loop {
		// Subscribe before reading, so a change between the read and the
		// wait is not missed.
		let mut changes = app.sale_changes.subscribe();
		let sale = {
			let conn = app.db.conn().await?;
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
	Path(id): Path<String>,
) -> ApiResult<Json<SaleView>> {
	let sale = {
		let conn = app.db.conn().await?;
		sales::get_sale(&*conn, &id).await?
	};
	if sale.state != SaleState::Open {
		return Err(conflict(format!("the sale is {}", sale.state)));
	}
	// The invoice is canceled first. barkd refuses once a payment is
	// arriving, and the sale then stays open for it.
	if let Some(hash) = watcher::invoice_hash(&app, sale.id).await? {
		match watcher::cancel_invoice(&app, &hash).await {
			Ok(()) => {},
			Err(watcher::CancelError::PaymentArriving) => {
				return Err(conflict("a payment is arriving for this sale"));
			},
			Err(watcher::CancelError::Unavailable(e)) => return Err(ApiError::Upstream(e)),
		}
	}
	let sale = {
		let conn = app.db.conn().await?;
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
	watcher::sale_ended(&app, sale.id).await;
	app.notify_sale_change();
	Ok(Json(sale.into()))
}
