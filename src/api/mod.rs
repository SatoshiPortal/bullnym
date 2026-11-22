//! The HTTP API. The contract is in `docs/api.md`.
//!
//! - `/api/v1/nym`: the Lightning Address registration and lookup.
//! - `/api/v1/stores`: posd's store registration.
//! - `/api/v1/owner/...`: everything behind the owner login, including the
//!   sales history and CSV.
//! - `/api/v1/checkout/<nym>/...`: the store's cashier link, which is also
//!   its public checkout. It needs no token.
//! - `/api/v1/sales/<id>`: following and canceling one sale, for whoever
//!   holds its unguessable id.
//! - LNURL: `/.well-known/lnurlp/<nym>` and its callback.

mod checkout;
mod lnurl;
mod nym;
mod owner;
mod web;

use std::sync::Arc;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::routing::{get, post, put};
use serde::Serialize;

use crate::App;
use crate::database::{NYM_COLUMNS, Nym, STORE_COLUMNS, Store};
use crate::error::{ApiError, ApiResult, not_found};
use crate::protect::{self, Caller, Surface};
use crate::token;


pub fn router(app: Arc<App>) -> Router {
	let api = Router::new()
		.route("/nym", post(nym::register))
		.route("/nym/{nym}", get(nym::lookup))
		.route("/stores", post(owner::register_store))
		.route("/owner/challenge", post(owner::challenge))
		.route("/owner/login", post(owner::login))
		.route("/owner/nym", get(nym::get_own).patch(nym::update_own))
		.route("/owner/nym/payments", get(nym::list_payments))
		.route("/owner/store", get(owner::get_store).patch(owner::update_store))
		.route("/owner/store/cashier-pin", put(owner::set_cashier_pin))
		.route("/owner/sales", get(owner::list_sales))
		.route("/owner/sales.csv", get(owner::sales_csv))
		.route("/checkout/{nym}", get(checkout::store))
		.route("/checkout/{nym}/rate", get(checkout::rate))
		.route("/checkout/{nym}/sales", post(checkout::create_sale))
		.route("/checkout/{nym}/pin", post(checkout::check_pin))
		.route("/sales/{id}", get(checkout::get_sale))
		.route("/sales/{id}/cancel", post(checkout::cancel_sale))
		.route("/lnurlp/{nym}/callback", get(lnurl::callback))
		.fallback(|| async { ApiError::NotFound("no such endpoint".into()) });

	Router::new()
		.nest("/api/v1", api)
		.route("/.well-known/lnurlp/{nym}", get(lnurl::pay_request))
		.route("/{nym}/manifest.webmanifest", get(web::public_manifest))
		.fallback(web::asset)
		.layer(axum::middleware::from_fn_with_state(app.clone(), protect::resolve_caller))
		.with_state(app)
}

/// Counts a name lookup: per source per minute, and distinct names per
/// source per hour.
pub async fn check_lookup(app: &App, caller: &Caller, name: &str) -> ApiResult<()> {
	app.protect.check(caller, Surface::Lookup)?;
	if caller.bypass {
		return Ok(());
	}
	let mut conn = app.public.db.conn().await?;
	protect::distinct_names(
		&mut conn, "lookup", &caller.source, name, app.protect.limits.lookup_names_per_hour, 3600,
	).await
}

/// Counts a registration: per source per minute, and new registering Ark
/// addresses per source per hour.
pub async fn check_registration(app: &App, caller: &Caller, ark_address: &str) -> ApiResult<()> {
	if caller.bypass {
		return Ok(());
	}
	let mut conn = app.public.db.conn().await?;
	protect::distinct_names(
		&mut conn, "register", &caller.source, ark_address,
		app.protect.limits.register_identities_per_hour, 3600,
	).await
}

/// The open invoices of every nym and sale: each is a hold invoice on the
/// Ark server and a pending receive in barkd.
pub async fn open_invoices(conn: &impl tokio_postgres::GenericClient) -> ApiResult<i64> {
	Ok(conn.query_one("
		SELECT (SELECT count(*) FROM lnurl_invoice WHERE state = 'open')
			+ (SELECT count(*) FROM sale_payment p JOIN sale s ON s.id = p.sale_id
				WHERE p.rail = 'lightning' AND p.settled_at IS NULL AND s.state IN ('open', 'in-progress'))
	", &[]).await?.get(0))
}

fn bearer(parts: &Parts) -> Result<&str, ApiError> {
	parts.headers.get(AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.strip_prefix("Bearer "))
		.map(str::trim)
		.filter(|t| !t.is_empty())
		.ok_or(ApiError::Unauthorized)
}

/// The owner of a nym, from an owner session token. The owner of the nym
/// also owns its store, if it has one.
pub struct Owner {
	pub nym: Nym,
	pub store: Option<Store>,
}

impl FromRequestParts<Arc<App>> for Owner {
	type Rejection = ApiError;

	async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
		let hash = token::hash(bearer(parts)?);
		let conn = app.public.db.conn().await?;
		let row = conn.query_opt(&format!("
			SELECT {} FROM owner_session o JOIN nym n ON n.id = o.nym_id
			WHERE o.token_hash = $1 AND o.expires_at > NOW()
		", NYM_COLUMNS), &[&hash]).await?.ok_or(ApiError::Unauthorized)?;
		let nym = Nym::from_row(&row);
		let store = store_of_nym(&*conn, nym.id).await?;
		Ok(Owner { nym, store })
	}
}

impl Owner {
	pub fn store(&self) -> ApiResult<&Store> {
		self.store.as_ref().ok_or_else(|| not_found("this nym has no store"))
	}
}

pub async fn store_of_nym(
	conn: &impl tokio_postgres::GenericClient,
	nym_id: i64,
) -> ApiResult<Option<Store>> {
	let row = conn.query_opt(&format!(
		"SELECT {} FROM store s JOIN nym n ON n.id = s.nym_id WHERE s.nym_id = $1", STORE_COLUMNS,
	), &[&nym_id]).await?;
	Ok(row.map(|r| Store::from_row(&r)))
}

/// How a store looks to anyone with its cashier link.
#[derive(Debug, Serialize)]
pub struct StoreView {
	pub name: String,
	pub nym: String,
	/// Always the nym: a store has no Lightning address of its own.
	pub lightning_address: String,
	pub currency: String,
	pub onchain_min_sat: i64,
	/// Whether the owner set a cashier PIN. Without one, the cashier screen
	/// hides its list of sales.
	pub cashier_pin_set: bool,
}

impl StoreView {
	pub fn new(app: &App, store: &Store) -> StoreView {
		StoreView {
			name: store.name.clone(),
			nym: store.nym.clone(),
			lightning_address: lightning_address(app, &store.nym),
			currency: store.currency.clone(),
			onchain_min_sat: store.onchain_min_sat,
			cashier_pin_set: store.cashier_pin_set,
		}
	}
}

/// `<nym>@<domain>`.
pub fn lightning_address(app: &App, nym: &str) -> String {
	format!("{}@{}", nym, public_host(app))
}

/// The host of the public URL, for the Lightning address.
pub fn public_host(app: &App) -> String {
	url::Url::parse(&app.cfg.http.public_url).ok()
		.and_then(|u| u.host_str().map(|h| match u.port() {
			Some(p) => format!("{}:{}", h, p),
			None => h.to_string(),
		}))
		.unwrap_or_default()
}

/// The public base URL, without a trailing slash.
pub fn public_url(app: &App) -> &str {
	app.cfg.http.public_url.trim_end_matches('/')
}
