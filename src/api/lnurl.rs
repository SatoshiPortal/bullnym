//! The Lightning Address: LUD-06 and LUD-16, with LUD-22 payment methods.
//!
//! - The default callback returns a forwarding invoice from barkd, with no
//!   Ark hint, paying the registered Ark address. Any Lightning wallet pays
//!   it, and barkd forwards the payment to the owner.
//! - LUD-22 (Bullnym's negotiation shape): the metadata lists
//!   `payment_methods`, `["SCUTTLE", "SP"]`. A callback with
//!   `payment_method=SCUTTLE` returns the registered Ark address,
//!   `payment_method=SP` the Silent Payment address, and
//!   `payment_method=SCUTTLE,SP` both. A reply carries only the requested
//!   methods and creates no invoice, so a Scuttle or Silent Payment payer
//!   costs the Ark server nothing. An unknown method is refused.
//!
//! The Ark server cannot put a description hash in an invoice, so the
//! invoice carries the metadata's text description, not its hash. Wallets
//! that enforce the LUD-06 hash check refuse these invoices.

use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bitcoin::Amount;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::App;
use crate::api::{check_lookup, check_lookup_name, lightning_address, open_invoices, public_url};
use crate::database::{NYM_COLUMNS, Nym};
use crate::error::{ApiError, UNAVAILABLE_MESSAGE};
use crate::protect::{self, Caller, Surface};
use crate::sales::lock_caps;


/// The LUD-22 methods this service offers: a Scuttle Ark address, and a
/// Silent Payment address.
pub const METHOD_SCUTTLE: &str = "SCUTTLE";
pub const METHOD_SP: &str = "SP";

/// An LNURL answer. Errors use the LNURL error shape.
pub struct LnurlResponse(Result<Value, (StatusCode, String)>);

impl LnurlResponse {
	fn ok(v: Value) -> LnurlResponse {
		LnurlResponse(Ok(v))
	}

	fn err(status: StatusCode, reason: impl Into<String>) -> LnurlResponse {
		LnurlResponse(Err((status, reason.into())))
	}
}

impl From<ApiError> for LnurlResponse {
	fn from(e: ApiError) -> LnurlResponse {
		let reason = match &e {
			ApiError::BadRequest(m) | ApiError::NotFound(m) | ApiError::Conflict(m) => m.clone(),
			ApiError::TooManyRequests => e.to_string(),
			_ => UNAVAILABLE_MESSAGE.to_string(),
		};
		LnurlResponse::err(e.into_response().status(), reason)
	}
}

/// The LUD-22 methods offered now: `SCUTTLE` while the Ark server is
/// healthy, and `SP` always.
fn offered_methods(app: &App) -> Vec<&'static str> {
	let mut methods = Vec::new();
	if app.protect.health.ark_payments() {
		methods.push(METHOD_SCUTTLE);
	}
	methods.push(METHOD_SP);
	methods
}

impl IntoResponse for LnurlResponse {
	fn into_response(self) -> Response {
		match self.0 {
			Ok(v) => Json(v).into_response(),
			Err((status, reason)) => {
				(status, Json(json!({ "status": "ERROR", "reason": reason }))).into_response()
			},
		}
	}
}

async fn nym_by_name(app: &App, name: &str) -> Result<Option<Nym>, ApiError> {
	let conn = app.public.db.conn().await?;
	let row = conn.query_opt(
		&format!("SELECT {} FROM nym n WHERE n.name = $1", NYM_COLUMNS), &[&name],
	).await?;
	Ok(row.map(|r| Nym::from_row(&r)))
}

fn metadata(app: &App, nym: &Nym) -> String {
	json!([
		["text/plain", format!("Payment to {}", nym.shown_name())],
		["text/identifier", lightning_address(app, &nym.name)],
	]).to_string()
}

/// `GET /.well-known/lnurlp/<nym>`.
pub async fn pay_request(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(name): Path<String>,
) -> LnurlResponse {
	if let Err(e) = check_lookup(&app, &caller, &name).await {
		return e.into();
	}
	let nym = match nym_by_name(&app, &name).await {
		Ok(Some(n)) => n,
		Ok(None) => return LnurlResponse::err(StatusCode::NOT_FOUND, "no such Lightning Address"),
		Err(e) => return e.into(),
	};
	LnurlResponse::ok(json!({
		"tag": "payRequest",
		"callback": format!("{}/api/v1/lnurlp/{}/callback", public_url(&app), nym.name),
		"minSendable": app.cfg.nyms.min_sendable_sat * 1000,
		"maxSendable": app.cfg.nyms.max_sendable_sat * 1000,
		"metadata": metadata(&app, &nym),
		"payment_methods": offered_methods(&app),
	}))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
	amount: Option<String>,
	/// LUD-22: a comma-separated list of methods, in the payer's order of
	/// preference. Absent means Lightning.
	payment_method: Option<String>,
}

/// A LUD-22 payment method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
	Scuttle,
	SilentPayment,
}

impl Method {
	pub fn name(&self) -> &'static str {
		match self {
			Method::Scuttle => METHOD_SCUTTLE,
			Method::SilentPayment => METHOD_SP,
		}
	}
}

/// The methods a callback asks for, in the payer's order, each once. An
/// empty or absent list asks for a Lightning invoice. Any method the
/// service does not know refuses the callback.
pub fn requested_methods(payment_method: Option<&str>) -> Result<Vec<Method>, String> {
	let mut methods = Vec::new();
	let list = payment_method.unwrap_or("");
	for m in list.split(',').map(str::trim).filter(|m| !m.is_empty()) {
		let method = if m.eq_ignore_ascii_case(METHOD_SCUTTLE) {
			Method::Scuttle
		} else if m.eq_ignore_ascii_case(METHOD_SP) {
			Method::SilentPayment
		} else {
			return Err(format!(
				"unsupported payment method {}; this Lightning Address offers {} and {}",
				m, METHOD_SCUTTLE, METHOD_SP,
			));
		};
		if !methods.contains(&method) {
			methods.push(method);
		}
	}
	Ok(methods)
}

/// Checks the LUD-06 amount, in msat: within the limits and whole sats.
pub fn check_amount(amount: Option<&str>, min_sat: u64, max_sat: u64) -> Result<Amount, String> {
	let msat = amount.and_then(|a| u64::from_str(a).ok()).ok_or("the amount is missing or not a number")?;
	if msat % 1000 != 0 {
		return Err("the amount must be whole sats".into());
	}
	let sat = msat / 1000;
	if sat < min_sat || sat > max_sat {
		return Err("the amount is out of range".into());
	}
	Ok(Amount::from_sat(sat))
}

/// `GET /api/v1/lnurlp/<nym>/callback?amount=<msat>[&payment_method=<m>]`.
pub async fn callback(
	State(app): State<Arc<App>>,
	caller: Caller,
	Path(name): Path<String>,
	Query(q): Query<CallbackQuery>,
) -> LnurlResponse {
	if let Err(e) = app.protect.check(&caller, Surface::Callback) {
		return e.into();
	}
	if let Err(e) = check_lookup_name(&app, &caller, &name).await {
		return e.into();
	}
	let nym = match nym_by_name(&app, &name).await {
		Ok(Some(n)) => n,
		Ok(None) => return LnurlResponse::err(StatusCode::NOT_FOUND, "no such Lightning Address"),
		Err(e) => return e.into(),
	};
	let amount = match check_amount(q.amount.as_deref(), app.cfg.nyms.min_sendable_sat, app.cfg.nyms.max_sendable_sat) {
		Ok(a) => a,
		Err(e) => return LnurlResponse::err(StatusCode::BAD_REQUEST, e),
	};
	let methods = match requested_methods(q.payment_method.as_deref()) {
		Ok(m) => m,
		Err(e) => return LnurlResponse::err(StatusCode::BAD_REQUEST, e),
	};
	if !caller.bypass {
		let res = async {
			let mut conn = app.public.db.conn().await?;
			protect::shared_window(
				&mut conn, &format!("callback-nym:{}", nym.id),
				app.protect.limits.callbacks_per_nym_per_minute, 60,
			).await
		}.await;
		if let Err(e) = res {
			return e.into();
		}
	}
	// Each option is offered only while its backend is healthy. The Silent
	// Payment address needs none: the owner's wallet scans for it. A method
	// whose backend is down is left out of the reply.
	if methods.is_empty() {
		if !app.protect.health.invoices() {
			return LnurlResponse::err(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE_MESSAGE);
		}
		return match lightning_invoice(&app, &nym, amount).await {
			Ok(pr) => LnurlResponse::ok(json!({ "pr": pr, "routes": [] })),
			Err(e) => e.into(),
		};
	}
	let available = methods.into_iter().filter(|m| match m {
		Method::Scuttle => app.protect.health.ark_payments(),
		Method::SilentPayment => true,
	}).collect::<Vec<_>>();
	if available.is_empty() {
		return LnurlResponse::err(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE_MESSAGE);
	}
	LnurlResponse::ok(method_reply(&available, &nym))
}

/// The reply to a LUD-22 callback: the requested methods only, each with
/// the nym's static address.
pub fn method_reply(methods: &[Method], nym: &Nym) -> Value {
	let mut reply = serde_json::Map::new();
	for m in methods {
		let address = match m {
			Method::Scuttle => &nym.ark_address,
			Method::SilentPayment => &nym.silent_payment_address,
		};
		reply.insert(m.name().to_string(), json!({ "address": address }));
	}
	Value::Object(reply)
}

/// A forwarding invoice from barkd, without an Ark hint, paying the nym's
/// registered Ark address. The service records it, watches barkd for its
/// payment and cancels it at expiry.
async fn lightning_invoice(app: &App, nym: &Nym, amount: Amount) -> Result<String, ApiError> {
	let address = ark::Address::from_str(&nym.ark_address)
		.map_err(|e| ApiError::Internal(anyhow::anyhow!("nym {} has an invalid address: {}", nym.name, e)))?;
	let expiry = app.cfg.nyms.invoice_expiry();

	// Open invoices are capped per nym and in total, so the service cannot
	// be used to flood the Ark server with hold invoices. The invoice is
	// reserved in the same transaction as the count, so parallel callbacks
	// cannot pass a cap together.
	let slot: i64 = {
		let mut conn = app.public.db.conn().await?;
		let tx = conn.transaction().await?;
		lock_caps(&tx).await?;
		let of_nym: i64 = tx.query_one(
			"SELECT count(*) FROM lnurl_invoice WHERE nym_id = $1 AND state = 'open'", &[&nym.id],
		).await?.get(0);
		if of_nym >= app.protect.limits.max_open_invoices_per_nym as i64 {
			return Err(ApiError::TooManyRequests);
		}
		if open_invoices(&tx).await? >= app.protect.limits.max_open_invoices as i64 {
			return Err(ApiError::Upstream(anyhow::anyhow!("the open invoice cap is reached")));
		}
		app.protect.take_public_invoice()?;
		let slot = tx.query_one("
			INSERT INTO lnurl_invoice (nym_id, amount_sat, expires_at)
			VALUES ($1, $2, NOW() + make_interval(secs => $3))
			RETURNING id
		", &[&nym.id, &(amount.to_sat() as i64), &(expiry.as_secs() as f64)]).await?.get("id");
		tx.commit().await?;
		slot
	};

	let description = Some(format!("Payment to {}", nym.shown_name()));
	let issued = app.public.forwarder.create_invoice(amount, &address, description, expiry, false).await;
	let conn = app.public.db.conn().await?;
	match issued {
		Ok(invoice) => {
			let hash = invoice.payment_hash().to_string();
			let saved = conn.execute("
				UPDATE lnurl_invoice SET payment_hash = $2, invoice = $3 WHERE id = $1 AND expires_at > clock_timestamp()
			", &[&slot, &hash, &invoice.to_string()]).await?;
			if saved == 0 {
				drop(conn);
				if let Err(e) = app.public.forwarder.cancel(&hash).await {
					tracing::warn!("failed to cancel unpublished invoice {}: {:#}", hash, e);
				}
				return Err(ApiError::Upstream(anyhow::anyhow!("the invoice reservation expired")));
			}
			Ok(invoice.to_string())
		},
		Err(e) => {
			conn.execute("DELETE FROM lnurl_invoice WHERE id = $1", &[&slot]).await?;
			Err(ApiError::Upstream(e))
		},
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn reads_the_requested_methods() {
		use Method::*;
		// None: a Lightning invoice.
		assert_eq!(requested_methods(None), Ok(vec![]));
		assert_eq!(requested_methods(Some("")), Ok(vec![]));
		assert_eq!(requested_methods(Some("SCUTTLE")), Ok(vec![Scuttle]));
		assert_eq!(requested_methods(Some("SP")), Ok(vec![SilentPayment]));
		assert_eq!(requested_methods(Some("sp")), Ok(vec![SilentPayment]));
		// A list, in the payer's order, each once.
		assert_eq!(requested_methods(Some("SCUTTLE,SP")), Ok(vec![Scuttle, SilentPayment]));
		assert_eq!(requested_methods(Some("SP, SCUTTLE,SP")), Ok(vec![SilentPayment, Scuttle]));
		// Unknown methods are refused, and ARK is not an alias.
		for unknown in ["ARK", "L-BTC", "SCUTTLE,L-BTC", "SP,ARK"] {
			let e = requested_methods(Some(unknown)).unwrap_err();
			assert!(e.starts_with("unsupported payment method") && e.contains("SCUTTLE and SP"), "{}", e);
		}
	}

	#[test]
	fn checks_amounts() {
		assert_eq!(check_amount(Some("21000"), 1, 100), Ok(Amount::from_sat(21)));
		assert!(check_amount(Some("21500"), 1, 100).is_err(), "not whole sats");
		assert!(check_amount(Some("0"), 1, 100).is_err());
		assert!(check_amount(Some("101000"), 1, 100).is_err());
		assert!(check_amount(None, 1, 100).is_err());
		assert!(check_amount(Some("abc"), 1, 100).is_err());
	}

	#[test]
	fn method_replies_carry_only_the_requested_methods() {
		let nym = Nym {
			id: 1, name: "cafe".into(), ark_address: "tark1x".into(),
			silent_payment_address: "tsp1y".into(), display_name: None, created_at: chrono::Utc::now(),
		};
		assert_eq!(method_reply(&[Method::Scuttle], &nym), json!({ "SCUTTLE": { "address": "tark1x" } }));
		assert_eq!(method_reply(&[Method::SilentPayment], &nym), json!({ "SP": { "address": "tsp1y" } }));
		assert_eq!(
			method_reply(&[Method::Scuttle, Method::SilentPayment], &nym),
			json!({ "SCUTTLE": { "address": "tark1x" }, "SP": { "address": "tsp1y" } }),
		);
	}
}
