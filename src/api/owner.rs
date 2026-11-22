//! Store registration, the owner login, and everything behind it: store
//! settings, the sales history and the CSV.
//!
//! The owner is the Ark address that registered the nym. It proves control
//! by signing with that address's key. The same login serves the nym and
//! its store.

use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use bitcoin::secp256k1::schnorr;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::App;
use crate::api::{Owner, StoreView, check_registration, public_url};
use crate::protect::{Caller, Surface};
use crate::database::{NYM_COLUMNS, Nym, STORE_COLUMNS, Store};
use crate::error::{ApiResult, bad_request, conflict, not_found};
use crate::money::currency_decimals;
use crate::rails::onchain::StoreDescriptor;
use crate::registration::StoreRegistration;
use crate::sales::{self, SaleView};
use crate::token;


/// How far a registration timestamp may be from the server clock.
const REGISTRATION_CLOCK_SKEW_SECS: i64 = 600;
const CHALLENGE_LIFETIME_MINS: i64 = 5;
const SESSION_LIFETIME_HOURS: i64 = 24;
const MAX_STORE_NAME_CHARS: usize = 64;

pub fn parse_signature(hex: &str) -> ApiResult<schnorr::Signature> {
	schnorr::Signature::from_str(hex).map_err(|_| bad_request("the signature is not valid hex"))
}

/// Refuses a registration timestamp more than 10 minutes from the clock.
pub fn check_timestamp(timestamp: u64) -> ApiResult<()> {
	let skew = (Utc::now().timestamp() - timestamp as i64).abs();
	if skew > REGISTRATION_CLOCK_SKEW_SECS {
		return Err(bad_request("the registration timestamp is too far from the current time"));
	}
	Ok(())
}

fn check_store_name(name: &str) -> ApiResult<()> {
	if name.is_empty() || name.chars().count() > MAX_STORE_NAME_CHARS {
		return Err(bad_request("the store name must be 1 to 64 characters"));
	}
	Ok(())
}

/// How a store looks to its owner.
#[derive(Debug, Serialize)]
pub struct OwnerStoreView {
	#[serde(flatten)]
	pub store: StoreView,
	/// The store's cashier link and public checkout.
	pub checkout_url: String,
	/// The next derivation index of an on-chain sale: every address the
	/// service handed out is below it. The wallet scans the store account up
	/// to it, so no payment is missed after many empty addresses.
	pub next_onchain_index: i64,
}

impl OwnerStoreView {
	fn new(app: &App, store: &Store) -> OwnerStoreView {
		OwnerStoreView {
			store: StoreView::new(app, store),
			checkout_url: format!("{}/{}", public_url(app), store.nym),
			next_onchain_index: store.next_onchain_index,
		}
	}
}

#[derive(Deserialize)]
pub struct RegisterStoreRequest {
	registration: StoreRegistration,
	signature: String,
}

/// `POST /api/v1/stores`: posd's store registration, tied to a nym.
pub async fn register_store(
	State(app): State<Arc<App>>,
	caller: Caller,
	Json(req): Json<RegisterStoreRequest>,
) -> ApiResult<Json<Value>> {
	app.protect.check(&caller, Surface::Register)?;
	let reg = req.registration;
	let signature = parse_signature(&req.signature)?;
	let info = app.ark.ark_info().await;
	let address = reg.verify(&signature, info.server_pubkey).map_err(|e| bad_request(e.to_string()))?;
	check_timestamp(reg.timestamp)?;
	let name = reg.name.trim();
	check_store_name(name)?;
	if currency_decimals(&reg.currency).is_none() {
		return Err(bad_request(format!("unsupported currency: {}", reg.currency)));
	}
	// posd's check, and no more: the wallet chooses the account.
	StoreDescriptor::parse(&reg.onchain_descriptor).map_err(|e| bad_request(format!("{:#}", e)))?;
	check_registration(&app, &caller, &reg.ark_address).await?;

	let mut conn = app.public.db.conn().await?;
	let tx = conn.transaction().await?;
	// The nym must exist and be registered by this same address. Locking
	// it serializes two registrations of a store for it.
	let nym = tx.query_opt(&format!(
		"SELECT {} FROM nym n WHERE n.name = $1 FOR UPDATE", NYM_COLUMNS,
	), &[&reg.nym]).await?.map(|r| Nym::from_row(&r))
		.ok_or_else(|| not_found("register the Lightning Address of this nym first"))?;
	if nym.ark_address != address.to_string() {
		return Err(bad_request("the store must use the Ark address its nym was registered with"));
	}
	let row = tx.query_opt("
		INSERT INTO store (nym_id, name, ark_address, onchain_descriptor, currency,
			onchain_min_sat, registration_timestamp, registration_signature)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
		ON CONFLICT DO NOTHING
		RETURNING id
	", &[
		&nym.id, &name, &nym.ark_address, &reg.onchain_descriptor, &reg.currency,
		&(app.cfg.sales.default_onchain_min_sat as i64), &(reg.timestamp as i64),
		&signature.to_string(),
	]).await?;
	if row.is_none() {
		return Err(conflict("this nym already has a store"));
	}
	let store = tx.query_one(&format!(
		"SELECT {} FROM store s JOIN nym n ON n.id = s.nym_id WHERE s.nym_id = $1", STORE_COLUMNS,
	), &[&nym.id]).await.map(|r| Store::from_row(&r))?;
	tx.commit().await?;
	tracing::info!("registered the store of nym {}", nym.name);
	Ok(Json(json!({ "store": OwnerStoreView::new(&app, &store) })))
}

#[derive(Deserialize)]
pub struct ChallengeRequest {
	ark_address: String,
}

pub async fn challenge(
	State(app): State<Arc<App>>,
	caller: Caller,
	Json(req): Json<ChallengeRequest>,
) -> ApiResult<Json<Value>> {
	app.protect.check(&caller, Surface::Owner)?;
	if req.ark_address.is_empty() || req.ark_address.len() > 512 || req.ark_address.contains(['\n', '\r']) {
		return Err(bad_request("not a valid Ark address"));
	}
	let expires_at = Utc::now() + Duration::minutes(CHALLENGE_LIFETIME_MINS);
	let challenge = format!(
		"bullnym login v1\nark_address:{}\nnonce:{}\nexpires_at:{}",
		req.ark_address, token::random_hex(16), expires_at.timestamp(),
	);
	let conn = app.public.db.conn().await?;
	conn.execute(
		"INSERT INTO owner_challenge (ark_address, challenge, expires_at) VALUES ($1, $2, $3)",
		&[&req.ark_address, &challenge, &expires_at],
	).await?;
	Ok(Json(json!({ "challenge": challenge })))
}

#[derive(Deserialize)]
pub struct LoginRequest {
	ark_address: String,
	challenge: String,
	signature: String,
}

pub async fn login(
	State(app): State<Arc<App>>,
	caller: Caller,
	Json(req): Json<LoginRequest>,
) -> ApiResult<Json<Value>> {
	app.protect.check(&caller, Surface::Owner)?;
	let address = ark::Address::from_str(&req.ark_address)
		.map_err(|_| bad_request("not a valid Ark address"))?;
	let signature = parse_signature(&req.signature)?;
	if !ark::message::verify_for_address(&address, req.challenge.as_bytes(), &signature) {
		return Err(bad_request("the signature does not verify for this address"));
	}

	let mut conn = app.public.db.conn().await?;
	let tx = conn.transaction().await?;
	let used = tx.execute("
		UPDATE owner_challenge SET used_at = NOW()
		WHERE challenge = $1 AND ark_address = $2 AND used_at IS NULL AND expires_at > NOW()
	", &[&req.challenge, &req.ark_address]).await?;
	if used == 0 {
		return Err(bad_request("the challenge is unknown, used or expired"));
	}
	let nym_id: i64 = tx.query_opt("SELECT id FROM nym WHERE ark_address = $1", &[&req.ark_address])
		.await?.ok_or_else(|| not_found("no nym is registered for this Ark address"))?
		.get("id");
	let session_token = token::new_token();
	let expires_at = Utc::now() + Duration::hours(SESSION_LIFETIME_HOURS);
	tx.execute(
		"INSERT INTO owner_session (nym_id, token_hash, expires_at) VALUES ($1, $2, $3)",
		&[&nym_id, &token::hash(&session_token), &expires_at],
	).await?;
	tx.commit().await?;
	Ok(Json(json!({ "token": session_token, "expires_at": expires_at })))
}

pub async fn get_store(State(app): State<Arc<App>>, owner: Owner) -> ApiResult<Json<OwnerStoreView>> {
	Ok(Json(OwnerStoreView::new(&app, owner.store()?)))
}

/// The settings an owner can change. Nothing else about a store can.
#[derive(Deserialize)]
pub struct UpdateStore {
	name: Option<String>,
	currency: Option<String>,
	onchain_min_sat: Option<u64>,
}

pub async fn update_store(
	State(app): State<Arc<App>>,
	owner: Owner,
	Json(req): Json<UpdateStore>,
) -> ApiResult<Json<OwnerStoreView>> {
	let store = owner.store()?;
	if let Some(name) = &req.name {
		check_store_name(name.trim())?;
	}
	if let Some(c) = &req.currency {
		if currency_decimals(c).is_none() {
			return Err(bad_request(format!("unsupported currency: {}", c)));
		}
	}
	let conn = app.public.db.conn().await?;
	conn.execute("
		UPDATE store SET
			name = coalesce($2, name),
			currency = coalesce($3, currency),
			onchain_min_sat = coalesce($4, onchain_min_sat),
			updated_at = NOW()
		WHERE id = $1
	", &[
		&store.id, &req.name.map(|n| n.trim().to_string()), &req.currency,
		&req.onchain_min_sat.map(|v| v as i64),
	]).await?;
	let store = reload_store(&app, store.id).await?;
	Ok(Json(OwnerStoreView::new(&app, &store)))
}

async fn reload_store(app: &App, store_id: i64) -> ApiResult<Store> {
	let conn = app.public.db.conn().await?;
	let row = conn.query_one(&format!(
		"SELECT {} FROM store s JOIN nym n ON n.id = s.nym_id WHERE s.id = $1", STORE_COLUMNS,
	), &[&store_id]).await?;
	Ok(Store::from_row(&row))
}

#[derive(Deserialize)]
pub struct ListQuery {
	pub before: Option<String>,
	pub limit: Option<i64>,
}

/// `GET /api/v1/owner/sales`: the history, owner only.
pub async fn list_sales(
	State(app): State<Arc<App>>,
	owner: Owner,
	Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
	let store = owner.store()?;
	let conn = app.public.db.conn().await?;
	let sales = sales::list_sales(&*conn, store.id, q.before.as_deref(), q.limit.unwrap_or(50)).await?;
	let sales = sales.into_iter().map(SaleView::from).collect::<Vec<_>>();
	Ok(Json(json!({ "sales": sales })))
}

#[derive(Deserialize)]
pub struct CsvQuery {
	pub from: Option<String>,
	pub to: Option<String>,
}

/// `GET /api/v1/owner/sales.csv`: the CSV export, owner only.
pub async fn sales_csv(
	State(app): State<Arc<App>>,
	owner: Owner,
	Query(q): Query<CsvQuery>,
) -> ApiResult<Response> {
	let store = owner.store()?;
	let from = q.from.as_deref().map(|d| parse_date(d, false)).transpose()?;
	let to = q.to.as_deref().map(|d| parse_date(d, true)).transpose()?;
	let conn = app.public.db.conn().await?;
	let body = sales::sales_csv(&*conn, store.id, from, to).await?;
	Ok((
		[
			(header::CONTENT_TYPE, "text/csv; charset=utf-8"),
			(header::CONTENT_DISPOSITION, "attachment; filename=\"sales.csv\""),
		],
		body,
	).into_response())
}

/// An RFC 3339 time, or a date in UTC. A date as the end of a range
/// includes that whole day.
fn parse_date(s: &str, end: bool) -> ApiResult<DateTime<Utc>> {
	if let Ok(t) = DateTime::parse_from_rfc3339(s) {
		return Ok(t.with_timezone(&Utc));
	}
	let day = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
		.ok_or_else(|| bad_request(format!("not a date: {}", s)))?;
	let day = if end { day.succ_opt().ok_or_else(|| bad_request("date out of range"))? } else { day };
	Ok(day.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn date_ranges() {
		assert_eq!(parse_date("2026-11-20", false).unwrap().to_rfc3339(), "2026-11-20T00:00:00+00:00");
		assert_eq!(parse_date("2026-11-20", true).unwrap().to_rfc3339(), "2026-11-21T00:00:00+00:00");
		assert_eq!(parse_date("2026-11-20T10:00:00Z", true).unwrap().to_rfc3339(), "2026-11-20T10:00:00+00:00");
		assert!(parse_date("20/11/2026", false).is_err());
	}

	#[test]
	fn registration_clock() {
		let now = Utc::now().timestamp() as u64;
		assert!(check_timestamp(now).is_ok());
		assert!(check_timestamp(now - 590).is_ok());
		assert!(check_timestamp(now + 590).is_ok());
		assert!(check_timestamp(now - 700).is_err());
		assert!(check_timestamp(now + 700).is_err());
	}
}
