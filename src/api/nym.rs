//! The Lightning Address: registration of the permanent nym, its lookup,
//! and the owner's view of it.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::App;
use crate::api::owner::{check_timestamp, parse_signature};
use crate::api::{Owner, StoreView, lightning_address, public_url, store_of_nym};
use crate::database::{NYM_COLUMNS, Nym};
use crate::error::{ApiResult, bad_request, conflict};
use crate::registration::NymRegistration;
use crate::{nym, silent_payment};


/// The longest display name.
const MAX_DISPLAY_NAME_CHARS: usize = 64;

#[derive(Deserialize)]
pub struct RegisterRequest {
	registration: NymRegistration,
	signature: String,
}

/// How a nym looks to its owner.
#[derive(Debug, Serialize)]
pub struct NymView {
	pub nym: String,
	pub lightning_address: String,
	pub display_name: Option<String>,
	pub ark_address: String,
	pub silent_payment_address: String,
	pub created_at: DateTime<Utc>,
	pub store: Option<StoreView>,
}

impl NymView {
	fn new(app: &App, nym: &Nym, store: Option<StoreView>) -> NymView {
		NymView {
			nym: nym.name.clone(),
			lightning_address: lightning_address(app, &nym.name),
			display_name: nym.display_name.clone(),
			ark_address: nym.ark_address.clone(),
			silent_payment_address: nym.silent_payment_address.clone(),
			created_at: nym.created_at,
			store,
		}
	}
}

/// `POST /api/v1/nym`: registers a nym, which creates its Lightning
/// Address. Nothing about it can change afterwards, apart from its display
/// name.
pub async fn register(
	State(app): State<Arc<App>>,
	Json(req): Json<RegisterRequest>,
) -> ApiResult<Json<Value>> {
	let reg = req.registration;
	let signature = parse_signature(&req.signature)?;
	nym::check(&reg.nym, &app.cfg.nyms.reserved).map_err(|e| bad_request(e.to_string()))?;
	let info = app.ark.ark_info().await;
	let address = reg.verify(&signature, info.server_pubkey).map_err(|e| bad_request(e.to_string()))?;
	check_timestamp(reg.timestamp)?;
	silent_payment::check(&reg.silent_payment_address, app.cfg.network)
		.map_err(|e| bad_request(format!("the Silent Payment address is not valid: {}", e)))?;

	let conn = app.db.conn().await?;
	// The unique constraints refuse a second nym for one address and a
	// second address for one nym, even under concurrent registrations.
	let row = conn.query_opt(&format!("
		INSERT INTO nym AS n (name, ark_address, silent_payment_address, registration_timestamp,
			registration_signature)
		VALUES ($1, $2, $3, $4, $5)
		ON CONFLICT DO NOTHING
		RETURNING {}
	", NYM_COLUMNS), &[
		&reg.nym, &address.to_string(), &reg.silent_payment_address, &(reg.timestamp as i64),
		&signature.to_string(),
	]).await?;
	let nym = match row {
		Some(row) => Nym::from_row(&row),
		None => {
			let taken = conn.query_opt("SELECT 1 FROM nym WHERE name = $1", &[&reg.nym]).await?;
			return Err(conflict(if taken.is_some() {
				"this nym is taken"
			} else {
				"this Ark address already has a nym"
			}));
		},
	};
	tracing::info!("registered nym {}", nym.name);
	Ok(Json(json!({ "nym": NymView::new(&app, &nym, None) })))
}

/// `GET /api/v1/nym/<nym>`: whether a nym is free, and what it serves.
pub async fn lookup(
	State(app): State<Arc<App>>,
	Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
	if let Err(e) = nym::check(&name, &app.cfg.nyms.reserved) {
		return Ok(Json(json!({
			"nym": name, "registered": false, "available": false, "reason": e.to_string(),
		})));
	}
	let conn = app.db.conn().await?;
	let row = conn.query_opt(
		&format!("SELECT {} FROM nym n WHERE n.name = $1", NYM_COLUMNS), &[&name],
	).await?;
	let Some(row) = row else {
		return Ok(Json(json!({ "nym": name, "registered": false, "available": true })));
	};
	let nym = Nym::from_row(&row);
	let store = store_of_nym(&*conn, nym.id).await?;
	let checkout_url = store.as_ref().filter(|s| s.public_checkout)
		.map(|_| format!("{}/{}", public_url(&app), nym.name));
	Ok(Json(json!({
		"nym": nym.name,
		"registered": true,
		"available": false,
		"lightning_address": lightning_address(&app, &nym.name),
		"has_store": store.is_some(),
		"checkout_url": checkout_url,
	})))
}

/// `GET /api/v1/owner/nym`.
pub async fn get_own(State(app): State<Arc<App>>, owner: Owner) -> Json<Value> {
	let store = owner.store.as_ref().map(|s| StoreView::new(&app, s));
	Json(json!({ "nym": NymView::new(&app, &owner.nym, store) }))
}

#[derive(Deserialize)]
pub struct UpdateNym {
	/// An empty name removes the display name.
	display_name: Option<String>,
}

/// `PATCH /api/v1/owner/nym`: only the display name can change.
pub async fn update_own(
	State(app): State<Arc<App>>,
	owner: Owner,
	Json(req): Json<UpdateNym>,
) -> ApiResult<Json<Value>> {
	let display_name = match req.display_name.as_deref().map(str::trim) {
		None => return Ok(get_own(State(app), owner).await),
		Some("") => None,
		Some(n) if n.chars().count() <= MAX_DISPLAY_NAME_CHARS && !n.contains(['\n', '\r']) => {
			Some(n.to_string())
		},
		Some(_) => return Err(bad_request("the display name must be at most 64 characters on one line")),
	};
	let conn = app.db.conn().await?;
	let row = conn.query_one(&format!("
		UPDATE nym n SET display_name = $2, updated_at = NOW() WHERE n.id = $1 RETURNING {}
	", NYM_COLUMNS), &[&owner.nym.id, &display_name]).await?;
	let nym = Nym::from_row(&row);
	let store = owner.store.as_ref().map(|s| StoreView::new(&app, s));
	Ok(Json(json!({ "nym": NymView::new(&app, &nym, store) })))
}

#[derive(Deserialize)]
pub struct PaymentsQuery {
	before: Option<i64>,
	limit: Option<i64>,
}

/// `GET /api/v1/owner/nym/payments`: the Lightning payments to the
/// Lightning Address, newest first. Ark and Silent Payment payments do not
/// pass through the service and appear only in the owner's wallet.
pub async fn list_payments(
	State(app): State<Arc<App>>,
	owner: Owner,
	Query(q): Query<PaymentsQuery>,
) -> ApiResult<Json<Value>> {
	let limit = q.limit.unwrap_or(50).clamp(1, 100);
	let conn = app.db.conn().await?;
	let rows = conn.query("
		SELECT id, payment_hash, amount_sat, state, received_sat, created_at, expires_at, settled_at
		FROM lnurl_invoice
		WHERE nym_id = $1 AND ($2::BIGINT IS NULL OR id < $2)
		ORDER BY id DESC LIMIT $3
	", &[&owner.nym.id, &q.before, &limit]).await?;
	let payments = rows.iter().map(|r| json!({
		"id": r.get::<_, i64>("id"),
		"payment_hash": r.get::<_, String>("payment_hash"),
		"amount_sat": r.get::<_, i64>("amount_sat"),
		"state": r.get::<_, String>("state"),
		"received_sat": r.get::<_, i64>("received_sat"),
		"created_at": r.get::<_, DateTime<Utc>>("created_at"),
		"expires_at": r.get::<_, DateTime<Utc>>("expires_at"),
		"settled_at": r.get::<_, Option<DateTime<Utc>>>("settled_at"),
	})).collect::<Vec<_>>();
	Ok(Json(json!({ "payments": payments })))
}
