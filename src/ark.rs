//! The Ark server, through its public gRPC API only.
//!
//! The service is a client of the Ark server like any wallet. It reads no
//! database of the server and has no admin access. At startup it checks
//! only what posd checked: that the server runs in expiry payout mode,
//! which the delivery report of Ark payments (S4) needs. Each sale then
//! checks that its invoice names the store's address as its Ark hint (see
//! [crate::sales]).

use std::str::FromStr;

use anyhow::{Context, bail};
use ark::ArkInfo;
use ark::lightning::PaymentHash;
use server_rpc::{ServerConnection, protos};

use crate::config;


pub struct ArkServer {
	conn: ServerConnection,
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
		Ok(ArkServer { conn })
	}

	pub async fn ark_info(&self) -> ArkInfo {
		self.conn.ark_info().await
	}

	/// Whether the server answers its handshake.
	pub async fn ping(&self) -> anyhow::Result<()> {
		self.conn.check_connection().await.context("the Ark server handshake failed")
	}

	/// Whether the server granted HTLC VTXOs to the receive with
	/// `payment_hash`, which only a Lightning payment gets. A receive an Ark
	/// transfer settled has none.
	pub async fn has_htlcs(&self, payment_hash: &str) -> anyhow::Result<bool> {
		let hash = PaymentHash::from_str(payment_hash).context("not a payment hash")?;
		let resp = self.conn.client.clone().check_lightning_receive(protos::CheckLightningReceiveRequest {
			hash: hash.to_vec(),
			wait: false,
		}).await.context("the Ark server refused the receive check")?.into_inner();
		Ok(!resp.htlc_vtxos.is_empty())
	}
}
