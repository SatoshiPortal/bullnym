//! The Ark server, through its public gRPC API only.
//!
//! The service is a client of the Ark server like any wallet. It reads no
//! database of the server and has no admin access. At startup it checks
//! only what posd checked: that the server runs in expiry payout mode,
//! which the delivery report of Ark payments (S4) needs. Each sale then
//! checks that its invoice names the store's address as its Ark hint (see
//! [crate::sales]).

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, bail};
use ark::ArkInfo;
use ark::lightning::PaymentHash;
use bitcoin::Amount;
use server_rpc::{RequestExt, ServerConnection, protos};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::config;


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
	pub amount: Amount,
}

pub struct ArkServer {
	conn: ServerConnection,
	/// A plain channel for the calls whose replies have fields the pinned
	/// protocol types lack.
	channel: Channel,
}

impl ArkServer {
	pub async fn connect(cfg: &config::Config) -> anyhow::Result<ArkServer> {
		let conn = ServerConnection::builder()
			.address(&cfg.ark_server_url)
			.network(cfg.network)
			.user_agent(format!("bullnym/{}", env!("CARGO_PKG_VERSION")))
			.connect().await
			.context("failed to connect to the Ark server")?;
		// posd's startup check, unchanged in substance.
		if !conn.expiry_payouts().await {
			bail!("the Ark server does not run in expiry payout mode, which tracking the \
				delivery of Ark payments (S4) needs");
		}
		let mut endpoint = Endpoint::from_shared(cfg.ark_server_url.clone())
			.context("the Ark server URL is not valid")?
			.timeout(Duration::from_secs(30));
		if cfg.ark_server_url.starts_with("https") {
			endpoint = endpoint.tls_config(ClientTlsConfig::new().with_native_roots())?;
		}
		let channel = endpoint.connect_lazy();
		Ok(ArkServer { conn, channel })
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
			amount: Amount::from_sat(resp.amount_sat),
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;

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
