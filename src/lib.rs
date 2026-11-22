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
pub mod protect;
pub mod rails;
pub mod registration;
pub mod sales;
pub mod silent_payment;
pub mod stages;
pub mod token;
pub mod watcher;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::watch;

use crate::ark::ArkServer;
use crate::config::Config;
use crate::database::Db;
use crate::price::PriceFeed;
use crate::protect::{Backend, Protect};
use crate::rails::forward::Forwarder;
use crate::rails::onchain::Chain;


/// The connections to the service's backends. Public traffic and the
/// watchers each have their own, so a flood of requests cannot use up the
/// connections that detect payments.
pub struct Backends {
	pub db: Db,
	pub forwarder: Forwarder,
	pub chain: Chain,
}

pub struct App {
	pub cfg: Config,
	/// For public traffic.
	pub public: Backends,
	/// For the watchers.
	pub watch: Backends,
	pub price: PriceFeed,
	pub ark: ArkServer,
	pub protect: Protect,
	/// Bumped whenever a sale changes state, to wake long-polls.
	pub sale_changes: watch::Sender<u64>,
}

impl App {
	/// Connects to every backend and checks the Ark server. It refuses to
	/// start when the server cannot serve the store (see [ark]).
	pub async fn new(cfg: Config) -> anyhow::Result<Arc<App>> {
		cfg.validate()?;
		let protect = Protect::new(cfg.limits.clone(), cfg.certification.clone())?;
		let db = Db::connect(&cfg.postgres, cfg.postgres.max_connections).await?;
		let watch_db = Db::sibling(&cfg.postgres, cfg.watcher.max_connections).await?;
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
			public: Backends {
				db,
				forwarder,
				chain: Chain::connect(cfg.bitcoind.clone()).await.context("failed to set up bitcoind")?,
			},
			watch: Backends {
				db: watch_db,
				forwarder: Forwarder::new(&cfg.barkd)?,
				chain: Chain::connect(cfg.bitcoind.clone()).await.context("failed to set up bitcoind")?,
			},
			price: PriceFeed::new(cfg.price.clone())?,
			sale_changes: watch::channel(0).0,
			protect,
			ark,
			cfg,
		};
		app.probe_health().await;
		Ok(Arc::new(app))
	}

	pub fn notify_sale_change(&self) {
		self.sale_changes.send_modify(|v| *v = v.wrapping_add(1));
	}

	/// Probes every backend once and records its health.
	pub async fn probe_health(&self) {
		let (barkd, ark, bitcoind) = tokio::join!(
			self.public.forwarder.ping(),
			self.ark.ping(),
			self.public.chain.ping(),
		);
		for (backend, res) in [(Backend::Barkd, barkd), (Backend::ArkServer, ark), (Backend::Bitcoind, bitcoind)] {
			if let Err(e) = &res {
				tracing::debug!("{:?} probe failed: {:#}", backend, e);
			}
			self.protect.health.record(backend, res.is_ok());
		}
	}
}

/// Starts the watchers, the health probes and the sweeps of the rate-limit
/// counters. They run until the process ends.
pub fn spawn_background(app: Arc<App>) -> Vec<tokio::task::JoinHandle<()>> {
	let health = {
		let app = app.clone();
		tokio::spawn(async move {
			let every = Duration::from_secs(app.cfg.watcher.health_interval_secs.max(1));
			loop {
				tokio::time::sleep(every).await;
				app.probe_health().await;
			}
		})
	};
	let sweep = {
		let app = app.clone();
		tokio::spawn(async move {
			loop {
				tokio::time::sleep(Duration::from_secs(300)).await;
				app.protect.sweep();
				let res = async {
					let conn = app.watch.db.conn().await?;
					protect::sweep_shared(&*conn).await
				}.await;
				if let Err(e) = res {
					tracing::warn!("sweeping the rate-limit counters failed: {:#}", e);
				}
			}
		})
	};
	vec![tokio::spawn(watcher::run(app)), health, sweep]
}

/// Serves the API on `listener`, with each caller's address available to
/// the protections.
pub async fn serve(listener: tokio::net::TcpListener, app: Arc<App>) -> std::io::Result<()> {
	let router = api::router(app);
	axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await
}
