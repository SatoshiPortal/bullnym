//! The Lightning Address: registration of the permanent nym, and the
//! owner's view of it.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::App;
use crate::api::owner::{check_timestamp, parse_signature};
use crate::api::{Owner, StoreView, check_registration, lightning_address};
use crate::protect::{Caller, Surface};
use crate::database::{NYM_COLUMNS, Nym};
use crate::error::{ApiResult, bad_request, conflict};
use crate::registration::NymRegistration;
use crate::{nym, silent_payment};


/// The longest display name.
const MAX_DISPLAY_NAME_CHARS: usize = 64;

/// The advisory lock under which the nym cap is counted and a nym inserted.
const NYM_CAP_LOCK: &str = "bullnym:nym-cap";

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
	caller: Caller,
	Json(req): Json<RegisterRequest>,
) -> ApiResult<Json<Value>> {
	// The cheap per-source limit comes before the signature check, so a
	// flood cannot spend the service's CPU.
	app.protect.check(&caller, Surface::Register)?;
	let reg = req.registration;
	let signature = parse_signature(&req.signature)?;
	nym::check(&reg.nym, &app.cfg.nyms.reserved).map_err(|e| bad_request(e.to_string()))?;
	let info = app.ark.ark_info().await;
	let address = reg.verify(&signature, info.server_pubkey, app.cfg.network)
		.map_err(|e| bad_request(e.to_string()))?;
	check_timestamp(reg.timestamp)?;
	silent_payment::check(&reg.silent_payment_address, app.cfg.network)
		.map_err(|e| bad_request(format!("the Silent Payment address is not valid: {}", e)))?;
	check_registration(&app, &caller, &reg.ark_address).await?;

	// The cap is counted and the nym inserted in one transaction, under a
	// lock, so simultaneous registrations cannot pass the cap together.
	let mut conn = app.public.db.conn().await?;
	let tx = conn.transaction().await?;
	tx.execute("SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)", &[&NYM_CAP_LOCK]).await?;
	let nyms: i64 = tx.query_one("SELECT count(*) FROM nym", &[]).await?.get(0);
	if nyms as u64 >= app.protect.limits.max_nyms {
		return Err(conflict("the service is not taking new registrations"));
	}
	// The unique constraints refuse a second nym for one address and a
	// second address for one nym, even under concurrent registrations.
	let row = tx.query_opt(&format!("
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
			let taken = tx.query_opt("SELECT 1 FROM nym WHERE name = $1", &[&reg.nym]).await?;
			return Err(conflict(if taken.is_some() {
				"this nym is taken"
			} else {
				"this Ark address already has a nym"
			}));
		},
	};
	tx.commit().await?;
	tracing::info!("registered nym {}", nym.name);
	Ok(Json(json!({ "nym": NymView::new(&app, &nym, None) })))
}

/// `GET /api/v1/owner/nym`.
pub async fn get_own(State(app): State<Arc<App>>, owner: Owner) -> Json<Value> {
	let store = match &owner.store {
		Some(s) => Some(StoreView::new(&app, s).await),
		None => None,
	};
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
	let conn = app.public.db.conn().await?;
	let row = conn.query_one(&format!("
		UPDATE nym n SET display_name = $2, updated_at = NOW() WHERE n.id = $1 RETURNING {}
	", NYM_COLUMNS), &[&owner.nym.id, &display_name]).await?;
	let nym = Nym::from_row(&row);
	drop(conn);
	let store = match &owner.store {
		Some(s) => Some(StoreView::new(&app, s).await),
		None => None,
	};
	Ok(Json(json!({ "nym": NymView::new(&app, &nym, store) })))
}
