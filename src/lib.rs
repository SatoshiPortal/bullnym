//! bullnym, a Lightning Address and store service for Ark wallets.
//!
//! Every payment goes to the owner's wallet directly: the service never
//! holds the funds and nobody using it can send.
//!
//! - Lightning and Ark: one forwarding invoice from a private barkd. It
//!   carries the owner's Ark address as a route hint, so a Scuttle wallet
//!   can pay it with an Ark transfer instead.
//! - On-chain: an address from the owner's public descriptor.

pub mod api;
pub mod ark;
pub mod config;
pub mod database;
pub mod error;
pub mod money;
pub mod nym;
pub mod price;
pub mod rails;
pub mod registration;
pub mod sales;
pub mod silent_payment;
pub mod token;
pub mod watcher;

use std::sync::Arc;

use anyhow::Context;
use tokio::sync::watch;

use crate::config::Config;
use crate::database::Db;
use crate::price::PriceFeed;
use crate::ark::ArkServer;
use crate::rails::forward::Forwarder;
use crate::rails::onchain::Chain;


pub struct App {
	pub cfg: Config,
	pub db: Db,
	pub price: PriceFeed,
	pub forwarder: Forwarder,
	pub ark: ArkServer,
	pub chain: Chain,
	/// Bumped whenever a sale changes state, to wake long-polls.
	pub sale_changes: watch::Sender<u64>,
}

impl App {
	pub async fn new(cfg: Config) -> anyhow::Result<Arc<App>> {
		let db = Db::connect(&cfg.postgres, cfg.postgres.max_connections).await?;
		let ark = ArkServer::connect(&cfg).await?;
		let info = ark.ark_info().await;
		if info.network != cfg.network {
			anyhow::bail!("the Ark server runs on {}, not {}", info.network, cfg.network);
		}
		let forwarder = Forwarder::new(&cfg.barkd)?;
		let support = ark.check_support(&forwarder).await?;
		tracing::info!("Ark server support: {:?}", support);
		crate::ark::require_support(&support, &cfg.startup)?;
		let app = App {
			price: PriceFeed::new(cfg.price.clone())?,
			chain: Chain::connect(cfg.bitcoind.clone()).await.context("failed to set up bitcoind")?,
			sale_changes: watch::channel(0).0,
			forwarder,
			db,
			ark,
			cfg,
		};
		Ok(Arc::new(app))
	}

	pub fn notify_sale_change(&self) {
		self.sale_changes.send_modify(|v| *v = v.wrapping_add(1));
	}
}
