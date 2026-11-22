//! The Lightning Address: LUD-06 and LUD-16, with LUD-22 payment methods.
//!
//! - The default callback returns a forwarding invoice from barkd, with no
//!   Ark hint, paying the registered Ark address. Any Lightning wallet pays
//!   it, and barkd forwards the payment to the owner.
//! - LUD-22 (Bullnym's negotiation shape): the metadata lists
//!   `payment_methods`, and a callback with `payment_method=ARK` returns
//!   the registered Ark address, `payment_method=SP` the Silent Payment
//!   address. A method response carries only that method and creates no
//!   invoice, so an Ark or Silent Payment payer costs the Ark server
//!   nothing.
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
use crate::api::{lightning_address, public_url};
use crate::database::{NYM_COLUMNS, Nym};
use crate::error::ApiError;


/// The LUD-22 methods this service offers.
pub const METHOD_ARK: &str = "ARK";
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
		let resp = e.into_response();
		LnurlResponse::err(resp.status(), "the payment cannot be made right now")
	}
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
	let conn = app.db.conn().await?;
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
pub async fn pay_request(State(app): State<Arc<App>>, Path(name): Path<String>) -> LnurlResponse {
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
		"payment_methods": [METHOD_ARK, METHOD_SP],
	}))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
	amount: Option<String>,
	/// LUD-22: a comma-separated list of methods, in the payer's order of
	/// preference. Absent means Lightning.
	payment_method: Option<String>,
}

/// The payment method a callback asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
	Lightning,
	Ark,
	SilentPayment,
}

/// Picks the method of a callback: the first one in the list that the
/// service offers. An empty or absent list means Lightning; a list with no
/// method the service offers is refused.
pub fn pick_method(payment_method: Option<&str>) -> Result<Method, String> {
	let Some(list) = payment_method.map(str::trim).filter(|l| !l.is_empty()) else {
		return Ok(Method::Lightning);
	};
	for m in list.split(',').map(str::trim) {
		if m.eq_ignore_ascii_case(METHOD_ARK) {
			return Ok(Method::Ark);
		}
		if m.eq_ignore_ascii_case(METHOD_SP) {
			return Ok(Method::SilentPayment);
		}
	}
	Err(format!("unsupported payment method; this Lightning Address offers {} and {}", METHOD_ARK, METHOD_SP))
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
	Path(name): Path<String>,
	Query(q): Query<CallbackQuery>,
) -> LnurlResponse {
	let nym = match nym_by_name(&app, &name).await {
		Ok(Some(n)) => n,
		Ok(None) => return LnurlResponse::err(StatusCode::NOT_FOUND, "no such Lightning Address"),
		Err(e) => return e.into(),
	};
	let amount = match check_amount(q.amount.as_deref(), app.cfg.nyms.min_sendable_sat, app.cfg.nyms.max_sendable_sat) {
		Ok(a) => a,
		Err(e) => return LnurlResponse::err(StatusCode::BAD_REQUEST, e),
	};
	let method = match pick_method(q.payment_method.as_deref()) {
		Ok(m) => m,
		Err(e) => return LnurlResponse::err(StatusCode::BAD_REQUEST, e),
	};
	if let Some(reply) = static_reply(method, &nym) {
		return LnurlResponse::ok(reply);
	}
	match lightning_invoice(&app, &nym, amount).await {
		Ok(pr) => LnurlResponse::ok(json!({ "pr": pr, "routes": [] })),
		Err(e) => e.into(),
	}
}

/// The reply to an `ARK` or `SP` callback: only the requested method, with
/// the nym's static address. `None` for Lightning.
pub fn static_reply(method: Method, nym: &Nym) -> Option<Value> {
	match method {
		Method::Ark => Some(json!({ METHOD_ARK: { "address": nym.ark_address } })),
		Method::SilentPayment => Some(json!({ METHOD_SP: { "address": nym.silent_payment_address } })),
		Method::Lightning => None,
	}
}

/// A forwarding invoice from barkd, without an Ark hint, paying the nym's
/// registered Ark address. The service records it, watches barkd for its
/// payment and cancels it at expiry.
async fn lightning_invoice(app: &App, nym: &Nym, amount: Amount) -> Result<String, ApiError> {
	let address = ark::Address::from_str(&nym.ark_address)
		.map_err(|e| ApiError::Internal(anyhow::anyhow!("nym {} has an invalid address: {}", nym.name, e)))?;
	let expiry = app.cfg.nyms.invoice_expiry();
	let description = Some(format!("Payment to {}", nym.shown_name()));
	let invoice = app.forwarder.create_invoice(amount, &address, description, expiry, false).await
		.map_err(ApiError::Upstream)?;
	let conn = app.db.conn().await?;
	conn.execute("
		INSERT INTO lnurl_invoice (nym_id, payment_hash, invoice, amount_sat, expires_at)
		VALUES ($1, $2, $3, $4, NOW() + make_interval(secs => $5))
	", &[
		&nym.id, &invoice.payment_hash().to_string(), &invoice.to_string(),
		&(amount.to_sat() as i64), &(expiry.as_secs() as f64),
	]).await?;
	Ok(invoice.to_string())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn picks_one_method() {
		assert_eq!(pick_method(None), Ok(Method::Lightning));
		assert_eq!(pick_method(Some("")), Ok(Method::Lightning));
		assert_eq!(pick_method(Some("ARK")), Ok(Method::Ark));
		assert_eq!(pick_method(Some("SP")), Ok(Method::SilentPayment));
		assert_eq!(pick_method(Some("sp")), Ok(Method::SilentPayment));
		// The first method the service offers, in the payer's order.
		assert_eq!(pick_method(Some("L-BTC,SP,ARK")), Ok(Method::SilentPayment));
		assert_eq!(pick_method(Some("ARK, SP")), Ok(Method::Ark));
		assert!(pick_method(Some("L-BTC")).is_err());
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
	fn method_replies_carry_only_their_method() {
		let nym = Nym {
			id: 1, name: "cafe".into(), ark_address: "tark1x".into(),
			silent_payment_address: "tsp1y".into(), display_name: None, created_at: chrono::Utc::now(),
		};
		assert_eq!(static_reply(Method::Ark, &nym), Some(json!({ "ARK": { "address": "tark1x" } })));
		assert_eq!(static_reply(Method::SilentPayment, &nym), Some(json!({ "SP": { "address": "tsp1y" } })));
		assert_eq!(static_reply(Method::Lightning, &nym), None);
	}
}
