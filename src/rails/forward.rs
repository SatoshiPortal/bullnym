//! The invoice rail: a forwarding invoice from a private barkd.
//!
//! barkd holds the preimage, claims a Lightning payment straight to the
//! owner's Ark key and posts the claim to the owner's mailbox. A forwarded
//! receive is `settled` only after that post, so `settled` means delivered.
//!
//! A store invoice also carries the owner's Ark address as a route hint
//! (S1), so a Scuttle wallet on the same Ark server can pay it with an Ark
//! transfer instead. barkd then reports `awaiting-delivery` until the Ark
//! server reports the transfer delivered to the owner's mailbox (S4), and
//! `settled` after that.
//!
//! The calls use small serde types of their own rather than the barkd REST
//! client of the pinned Scuttle release, which lacks `ark_address_hint` and
//! `awaiting-delivery`. A barkd without S1 ignores the unknown field and
//! returns an invoice without the hint.

use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use bitcoin::Amount;
use lightning_invoice::Bolt11Invoice;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::config;


/// `POST /api/v1/lightning/receives/invoice/for-address`.
#[derive(Debug, Clone, Serialize)]
struct InvoiceForAddressRequest<'a> {
	amount_sat: u64,
	address: &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	description: Option<&'a str>,
	#[serde(skip_serializing_if = "Option::is_none")]
	expiry_secs: Option<u64>,
	/// Put `address` in the invoice's Ark address route hint (S1).
	ark_address_hint: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct InvoiceInfo {
	invoice: String,
}

/// The phase of a receive, as barkd reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReceiveState {
	AwaitingPayment,
	HtlcsReady,
	PreimageRevealed,
	Delivering,
	/// Paid by an Ark transfer that the Ark server has not reported
	/// delivered yet (S4).
	AwaitingDelivery,
	Settled,
	/// A state this service does not know.
	#[serde(other)]
	Other,
}

#[derive(Debug, Clone, Deserialize)]
struct ReceiveInfo {
	state: ReceiveState,
	amount_sat: u64,
}

pub struct Forwarder {
	base: String,
	http: reqwest::Client,
}

/// What barkd reports about a forwarding invoice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardStatus {
	/// Waiting for a payment.
	Unpaid,
	/// A Lightning payment is arriving: barkd holds its HTLCs or revealed
	/// the preimage, and has not delivered the claim yet.
	Arriving,
	/// Paid by an Ark transfer that is not delivered yet.
	AwaitingDelivery,
	/// Delivered to the owner's mailbox.
	Delivered(Amount),
	/// Canceled, or expired unpaid.
	Gone,
	/// A state this service does not know. It is never read as paid.
	Unknown,
}

impl ForwardStatus {
	fn from_state(state: ReceiveState, amount: Amount) -> ForwardStatus {
		match state {
			ReceiveState::AwaitingPayment => ForwardStatus::Unpaid,
			ReceiveState::HtlcsReady | ReceiveState::PreimageRevealed | ReceiveState::Delivering => {
				ForwardStatus::Arriving
			},
			ReceiveState::AwaitingDelivery => ForwardStatus::AwaitingDelivery,
			ReceiveState::Settled => ForwardStatus::Delivered(amount),
			ReceiveState::Other => ForwardStatus::Unknown,
		}
	}
}

impl Forwarder {
	pub fn new(cfg: &config::Barkd) -> anyhow::Result<Forwarder> {
		let mut headers = HeaderMap::new();
		if let Some(token) = &cfg.token {
			headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", token))?);
		}
		let http = reqwest::Client::builder()
			.default_headers(headers)
			.timeout(Duration::from_secs(30))
			.build()?;
		Ok(Forwarder { base: cfg.url.trim_end_matches('/').to_string(), http })
	}

	fn url(&self, path: &str) -> String {
		format!("{}/api/v1/lightning/receives{}", self.base, path)
	}

	/// Creates an invoice that barkd claims to `address`. With
	/// `ark_address_hint`, the invoice also names `address` as its Ark
	/// address route hint.
	pub async fn create_invoice(
		&self,
		amount: Amount,
		address: &ark::Address,
		description: Option<String>,
		expiry: Duration,
		ark_address_hint: bool,
	) -> anyhow::Result<Bolt11Invoice> {
		let address = address.to_string();
		let req = InvoiceForAddressRequest {
			amount_sat: amount.to_sat(),
			address: &address,
			description: description.as_deref(),
			expiry_secs: Some(expiry.as_secs()),
			ark_address_hint,
		};
		let resp = self.http.post(self.url("/invoice/for-address")).json(&req).send().await
			.context("barkd is not reachable")?;
		let resp = check(resp).await.context("barkd failed to create a forwarding invoice")?;
		let info = resp.json::<InvoiceInfo>().await.context("barkd returned no invoice")?;
		Bolt11Invoice::from_str(&info.invoice).context("barkd returned an invalid invoice")
	}

	pub async fn status(&self, payment_hash: &str) -> anyhow::Result<ForwardStatus> {
		let resp = self.http.get(self.url(&format!("/{}", payment_hash))).send().await
			.context("barkd is not reachable")?;
		// barkd answers 404 for a receive that ended unsettled.
		if resp.status() == StatusCode::NOT_FOUND {
			return Ok(ForwardStatus::Gone);
		}
		let resp = check(resp).await.context("barkd status check failed")?;
		let info = resp.json::<ReceiveInfo>().await.context("barkd returned no receive")?;
		Ok(ForwardStatus::from_state(info.state, Amount::from_sat(info.amount_sat)))
	}

	/// Whether barkd answers.
	pub async fn ping(&self) -> anyhow::Result<()> {
		let resp = self.http.get(format!("{}/api/v1/bitcoin/tip", self.base)).send().await
			.context("barkd is not reachable")?;
		check(resp).await?;
		Ok(())
	}

	/// Cancels an unpaid invoice. barkd refuses once it revealed the
	/// preimage; the payment is then delivered and counted as excess.
	pub async fn cancel(&self, payment_hash: &str) -> anyhow::Result<()> {
		let resp = self.http.delete(self.url(&format!("/{}", payment_hash))).send().await
			.context("barkd is not reachable")?;
		check(resp).await.context("barkd refused to cancel the invoice")?;
		Ok(())
	}
}

/// Turns a non-2xx answer into an error with its body.
async fn check(resp: reqwest::Response) -> anyhow::Result<reqwest::Response> {
	let status = resp.status();
	if status.is_success() {
		return Ok(resp);
	}
	let body = resp.text().await.unwrap_or_default();
	anyhow::bail!("status {}: {}", status, body.chars().take(300).collect::<String>())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn reads_receive_states() {
		let info: ReceiveInfo = serde_json::from_str(
			r#"{"payment_hash":"00","state":"settled","invoice":"lnbc","amount_sat":21000}"#,
		).unwrap();
		assert_eq!(info.state, ReceiveState::Settled);
		assert_eq!(info.amount_sat, 21_000);
		let info: ReceiveInfo = serde_json::from_str(
			r#"{"state":"preimage-revealed","amount_sat":1}"#,
		).unwrap();
		assert_eq!(info.state, ReceiveState::PreimageRevealed);
		// The state that S4 adds.
		let info: ReceiveInfo = serde_json::from_str(
			r#"{"state":"awaiting-delivery","amount_sat":1}"#,
		).unwrap();
		assert_eq!(info.state, ReceiveState::AwaitingDelivery);
		// A state added by a newer barkd is read, not refused.
		let info: ReceiveInfo = serde_json::from_str(
			r#"{"state":"something-new","amount_sat":1}"#,
		).unwrap();
		assert_eq!(info.state, ReceiveState::Other);
	}

	#[test]
	fn maps_states_to_payment_status() {
		let a = Amount::from_sat(5);
		assert_eq!(ForwardStatus::from_state(ReceiveState::AwaitingPayment, a), ForwardStatus::Unpaid);
		assert_eq!(ForwardStatus::from_state(ReceiveState::HtlcsReady, a), ForwardStatus::Arriving);
		assert_eq!(ForwardStatus::from_state(ReceiveState::PreimageRevealed, a), ForwardStatus::Arriving);
		assert_eq!(ForwardStatus::from_state(ReceiveState::Delivering, a), ForwardStatus::Arriving);
		assert_eq!(
			ForwardStatus::from_state(ReceiveState::AwaitingDelivery, a), ForwardStatus::AwaitingDelivery,
		);
		assert_eq!(ForwardStatus::from_state(ReceiveState::Settled, a), ForwardStatus::Delivered(a));
		// An unknown state is never paid.
		assert_eq!(ForwardStatus::from_state(ReceiveState::Other, a), ForwardStatus::Unknown);
	}

	#[test]
	fn sends_the_hint_flag() {
		let req = InvoiceForAddressRequest {
			amount_sat: 1000, address: "tark1x", description: None, expiry_secs: Some(60),
			ark_address_hint: true,
		};
		assert_eq!(
			serde_json::to_value(&req).unwrap(),
			serde_json::json!({"amount_sat": 1000, "address": "tark1x", "expiry_secs": 60, "ark_address_hint": true}),
		);
	}
}
