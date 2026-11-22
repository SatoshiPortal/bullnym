//! The Ark server, through its public gRPC API only.
//!
//! The service is a client of the Ark server like any wallet. It reads no
//! database of the server and has no admin access. At startup it checks
//! that the server can serve the store: expiry payout mode, Ark address
//! route hints, a hint that names a forwarded receive's destination (S1)
//! and the delivery status of Ark payments (S4).

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, bail};
use ark::ArkInfo;
use ark::encode::ProtocolEncoding;
use ark::lightning::{Bolt11InvoiceExt, PaymentHash, Preimage};
use ark::mailbox::MailboxIdentifier;
use bitcoin::secp256k1::{Keypair, rand};
use bitcoin::{Amount, Network};
use bitcoin_ext::BlockDelta;
use lightning_invoice::Bolt11Invoice;
use server_rpc::{RequestExt, ServerConnection, protos};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::config;
use crate::rails::forward::Forwarder;


/// The CLTV margins bark adds for a receive: its default VTXO exit margin,
/// HTLC claim delta and claim preparation delta.
const RECEIVE_CLTV_MARGIN: u16 = 12 + 18 + 2;

/// The amount of the invoices the startup checks create and cancel.
const PROBE_AMOUNT: Amount = Amount::from_sat(1_000);
const PROBE_EXPIRY: Duration = Duration::from_secs(120);

/// `CheckLightningReceiveResponse` with the `delivered` field that S4 adds.
/// The protocol types of the pinned Scuttle release do not have it, and a
/// server without S4 leaves it unset.
#[derive(Clone, PartialEq, prost::Message)]
struct CheckLightningReceiveResponse {
	#[prost(string, tag = "1")]
	invoice: String,
	#[prost(uint64, tag = "2")]
	amount_sat: u64,
	#[prost(int32, tag = "3")]
	status: i32,
	#[prost(bytes = "vec", repeated, tag = "4")]
	htlc_vtxos: Vec<Vec<u8>>,
	#[prost(bool, optional, tag = "5")]
	delivered: Option<bool>,
}

/// The server's view of one Lightning receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerReceive {
	/// Whether the server granted HTLC VTXOs, which only a Lightning payment
	/// does. A receive settled by an Ark transfer has none.
	pub has_htlcs: bool,
	/// `Some` when the server reports delivery of an Ark payment (S4).
	pub delivered: Option<bool>,
}

/// What the startup checks found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSupport {
	pub expiry_payouts: bool,
	pub address_hints: bool,
	/// S1: the forwarded invoice names the destination in its hint.
	pub forwarded_hint: bool,
	/// S4: the receive status reports delivery.
	pub delivery_status: bool,
}

impl ServerSupport {
	/// The reasons the service cannot serve stores with this server.
	pub fn problems(&self) -> Vec<&'static str> {
		let mut out = Vec::new();
		if !self.expiry_payouts {
			out.push("the Ark server is not in expiry payout mode");
		}
		if !self.address_hints {
			out.push("the Ark server does not put Ark addresses in invoices \
				(ln_receive_ark_address_hints is off)");
		}
		if !self.forwarded_hint {
			out.push("barkd and the Ark server do not put the destination of a forwarded \
				receive in its invoice (S1)");
		}
		if !self.delivery_status {
			out.push("the Ark server does not report the delivery of Ark payments (S4)");
		}
		out
	}
}

pub struct ArkServer {
	conn: ServerConnection,
	/// A plain channel for the calls whose replies have fields the pinned
	/// protocol types lack.
	channel: Channel,
	network: Network,
}

impl ArkServer {
	pub async fn connect(cfg: &config::Config) -> anyhow::Result<ArkServer> {
		let conn = ServerConnection::builder()
			.address(&cfg.ark_server_url)
			.network(cfg.network)
			.user_agent(format!("bullnym/{}", env!("CARGO_PKG_VERSION")))
			.connect().await
			.context("failed to connect to the Ark server")?;
		let mut endpoint = Endpoint::from_shared(cfg.ark_server_url.clone())
			.context("the Ark server URL is not valid")?
			.timeout(Duration::from_secs(30));
		if cfg.ark_server_url.starts_with("https") {
			endpoint = endpoint.tls_config(ClientTlsConfig::new().with_native_roots())?;
		}
		let channel = endpoint.connect_lazy();
		Ok(ArkServer { conn, channel, network: cfg.network })
	}

	pub async fn ark_info(&self) -> ArkInfo {
		self.conn.ark_info().await
	}

	/// Whether the server answers its handshake.
	pub async fn ping(&self) -> anyhow::Result<()> {
		self.conn.check_connection().await.context("the Ark server handshake failed")
	}

	/// The server's view of the receive with `payment_hash`.
	pub async fn receive(&self, payment_hash: &str) -> anyhow::Result<ServerReceive> {
		let hash = PaymentHash::from_str(payment_hash).context("not a payment hash")?;
		let mut grpc = tonic::client::Grpc::new(self.channel.clone());
		grpc.ready().await.context("the Ark server is not reachable")?;
		let mut req = tonic::Request::new(protos::CheckLightningReceiveRequest {
			hash: hash.to_vec(),
			wait: false,
		});
		req.set_pver(server_rpc::pver::PROTOCOL_VERSION_SCUTTLE_EXPIRY_PAYOUT);
		let path = http::uri::PathAndQuery::from_static(
			"/bark_server.ArkService/CheckLightningReceive",
		);
		let codec = tonic_prost::ProstCodec::<
			protos::CheckLightningReceiveRequest, CheckLightningReceiveResponse,
		>::default();
		let resp = grpc.unary(req, path, codec).await
			.context("the Ark server refused the receive check")?
			.into_inner();
		Ok(ServerReceive {
			has_htlcs: !resp.htlc_vtxos.is_empty(),
			delivered: resp.delivered,
		})
	}

	/// Checks what the server supports. It creates two invoices and cancels
	/// them: one straight on the server, to see the Ark address hint, and
	/// one through barkd, to see the hint of a forwarded receive (S1) and
	/// its delivery status (S4).
	pub async fn check_support(&self, forwarder: &Forwarder) -> anyhow::Result<ServerSupport> {
		let expiry_payouts = self.conn.expiry_payouts().await;
		let address_hints = self.probe_address_hints().await
			.context("the Ark address hint check failed")?;
		let (forwarded_hint, delivery_status) = self.probe_forwarded_hint(forwarder).await
			.context("the forwarded invoice check failed")?;
		Ok(ServerSupport { expiry_payouts, address_hints, forwarded_hint, delivery_status })
	}

	/// An Ark address of this server for a key nobody keeps. The startup
	/// checks use it; no payment can reach it, because its invoices are
	/// canceled at once and never shown.
	async fn throwaway_address(&self) -> anyhow::Result<ark::Address> {
		let info = self.ark_info().await;
		let key = Keypair::new(&ark::SECP, &mut rand::thread_rng());
		let mailbox = MailboxIdentifier::from_pubkey(
			Keypair::new(&ark::SECP, &mut rand::thread_rng()).public_key(),
		);
		ark::Address::builder()
			.testnet(self.network != Network::Bitcoin)
			.server_pubkey(info.server_pubkey)
			.pubkey_policy(key.public_key())
			.mailbox(info.mailbox_pubkey, mailbox, &key)
			.map_err(|e| anyhow::anyhow!("{}", e))?
			.into_address()
			.map_err(|e| anyhow::anyhow!("{}", e))
	}

	async fn probe_address_hints(&self) -> anyhow::Result<bool> {
		let info = self.ark_info().await;
		let min_cltv_delta = info.vtxo_exit_delta
			.checked_add(info.htlc_expiry_delta)
			.and_then(|d| d.checked_add(BlockDelta::new(RECEIVE_CLTV_MARGIN)))
			.context("CLTV delta overflow")?;
		let key = Keypair::new(&ark::SECP, &mut rand::thread_rng());
		let mailbox = MailboxIdentifier::from_pubkey(key.public_key());
		let payment_hash = Preimage::random().compute_payment_hash();
		let req = protos::StartLightningReceiveRequest {
			payment_hash: payment_hash.to_vec(),
			amount_sat: PROBE_AMOUNT.to_sat(),
			min_cltv_delta: min_cltv_delta.into(),
			mailbox_id: Some(mailbox.serialize()),
			description: Some("bullnym startup check".into()),
			expiry_secs: Some(PROBE_EXPIRY.as_secs()),
			claim_pubkey: Some(key.public_key().serialize().to_vec()),
			ark_address_hint: true,
		};
		let resp = self.conn.client.clone().start_lightning_receive(req).await
			.context("the Ark server refused the invoice")?
			.into_inner();
		let cancel = protos::CancelLightningReceiveRequest { payment_hash: payment_hash.to_vec() };
		if let Err(e) = self.conn.client.clone().cancel_lightning_receive(cancel).await {
			tracing::warn!("failed to cancel the startup check invoice: {}", e);
		}
		let invoice = Bolt11Invoice::from_str(&resp.bolt11)
			.context("the Ark server returned an invalid invoice")?;
		Ok(invoice.ark_address().is_some())
	}

	async fn probe_forwarded_hint(&self, forwarder: &Forwarder) -> anyhow::Result<(bool, bool)> {
		let address = self.throwaway_address().await?;
		let invoice = forwarder.create_invoice(
			PROBE_AMOUNT, &address, Some("bullnym startup check".into()), PROBE_EXPIRY, true,
		).await?;
		let hash = invoice.payment_hash().to_string();
		let hinted = invoice.ark_address().is_some_and(|a| a.to_string() == address.to_string());
		let delivery = match self.receive(&hash).await {
			Ok(r) => r.delivered.is_some(),
			Err(e) => {
				tracing::warn!("the receive check of the startup invoice failed: {:#}", e);
				false
			},
		};
		if let Err(e) = forwarder.cancel(&hash).await {
			tracing::warn!("failed to cancel the startup check invoice: {:#}", e);
		}
		Ok((hinted, delivery))
	}
}

/// Refuses to go on unless the server supports everything, or the
/// config allows a server without S1 and S4 (for local tests only).
pub fn require_support(support: &ServerSupport, cfg: &config::Startup) -> anyhow::Result<()> {
	let mut fatal = Vec::new();
	for p in support.problems() {
		let s1_s4 = p.contains("(S1)") || p.contains("(S4)");
		if s1_s4 && cfg.allow_missing_s1_s4 {
			tracing::warn!("{}; going on because startup.allow_missing_s1_s4 is set", p);
		} else {
			fatal.push(p);
		}
	}
	if !fatal.is_empty() {
		bail!("the service cannot start: {}", fatal.join("; "));
	}
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	fn all() -> ServerSupport {
		ServerSupport {
			expiry_payouts: true, address_hints: true, forwarded_hint: true, delivery_status: true,
		}
	}

	#[test]
	fn startup_requires_payouts_hints_s1_and_s4() {
		let strict = config::Startup { allow_missing_s1_s4: false };
		let lax = config::Startup { allow_missing_s1_s4: true };
		assert!(require_support(&all(), &strict).is_ok());

		let s = ServerSupport { expiry_payouts: false, ..all() };
		assert!(require_support(&s, &strict).is_err());
		assert!(require_support(&s, &lax).is_err(), "the override never covers payout mode");

		let s = ServerSupport { address_hints: false, ..all() };
		assert!(require_support(&s, &lax).is_err(), "the override never covers hints");

		let s = ServerSupport { forwarded_hint: false, ..all() };
		assert!(require_support(&s, &strict).is_err());
		assert!(require_support(&s, &lax).is_ok());

		let s = ServerSupport { delivery_status: false, ..all() };
		assert!(require_support(&s, &strict).is_err());
		assert!(require_support(&s, &lax).is_ok());
	}

	#[test]
	fn reads_the_delivered_field() {
		use prost::Message;
		let without = CheckLightningReceiveResponse {
			invoice: "lnbc".into(), amount_sat: 1, status: 3, htlc_vtxos: vec![], delivered: None,
		};
		let bytes = without.encode_to_vec();
		assert_eq!(CheckLightningReceiveResponse::decode(&bytes[..]).unwrap().delivered, None);
		// The pinned protocol type ignores the field, which is what an older
		// client does.
		let with = CheckLightningReceiveResponse { delivered: Some(false), ..without };
		let decoded = protos::CheckLightningReceiveResponse::decode(&with.encode_to_vec()[..]).unwrap();
		assert_eq!(decoded.amount_sat, 1);
		assert_eq!(
			CheckLightningReceiveResponse::decode(&with.encode_to_vec()[..]).unwrap().delivered,
			Some(false),
		);
	}
}
