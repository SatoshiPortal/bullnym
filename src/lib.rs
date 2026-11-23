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
pub mod pin;
pub mod price;
pub mod protect;
pub mod rails;
pub mod registration;
pub mod sales;
pub mod silent_payment;
pub mod stages;
pub mod token;
pub mod watcher;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::broadcast;

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
	/// Sale IDs whose public view changed. Lagged subscribers read the current snapshot.
	pub sale_changes: broadcast::Sender<i64>,
	/// Always the default outside the integration tests.
	pub overrides: TestOverrides,
}

/// Behavior that only the integration tests can change. It is not part of
/// [Config], so no config file or environment variable can set it, and it
/// can only be passed in a build with the `test-overrides` feature, which
/// the service binary never enables.
#[derive(Debug, Clone, Default)]
pub struct TestOverrides {
	/// Accept a store invoice that barkd returned without the requested Ark
	/// hint, instead of refusing the sale. Scuttle releases without S1, such
	/// as 1.0.0-rc.6, ignore the hint request, and the integration tests run
	/// sales against them with this.
	pub accept_unhinted_invoices: bool,
}

impl App {
	/// Connects to every backend and checks that the Ark server runs in
	/// expiry payout mode (see [ark]).
	pub async fn new(cfg: Config) -> anyhow::Result<Arc<App>> {
		App::build(cfg, TestOverrides::default()).await
	}

	/// [App::new] with test overrides. For the integration tests only.
	#[cfg(feature = "test-overrides")]
	pub async fn new_with_overrides(cfg: Config, overrides: TestOverrides) -> anyhow::Result<Arc<App>> {
		App::build(cfg, overrides).await
	}

	async fn build(cfg: Config, overrides: TestOverrides) -> anyhow::Result<Arc<App>> {
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
		if overrides.accept_unhinted_invoices {
			tracing::warn!("test override: store invoices without the Ark hint are accepted");
		}
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
			sale_changes: broadcast::channel(256).0,
			protect,
			ark,
			cfg,
			overrides,
		};
		app.probe_health().await;
		Ok(Arc::new(app))
	}

	pub fn notify_sale_change(&self, id: i64) {
		let _ = self.sale_changes.send(id);
	}

	/// Probes every backend once and records its health.
	pub async fn probe_health(&self) {
		tokio::join!(
			self.probe(Backend::Barkd, self.public.forwarder.ping()),
			self.probe(Backend::ArkServer, self.ark.ping()),
			self.probe(Backend::Bitcoind, self.public.chain.ping()),
		);
	}

	async fn probe(&self, backend: Backend, check: impl Future<Output = anyhow::Result<()>>) {
		let result = tokio::time::timeout(Duration::from_secs(30), check).await;
		let healthy = matches!(result, Ok(Ok(())));
		if !healthy { tracing::debug!(?backend, ?result, "backend health check failed"); }
		self.protect.health.record(backend, healthy);
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
					protect::sweep_shared(&*conn).await?;
					pin::sweep(&*conn).await?;
					conn.execute("DELETE FROM owner_challenge WHERE expires_at <= NOW()", &[]).await?;
					anyhow::Ok(())
				}.await;
				if let Err(e) = res {
					tracing::warn!("sweeping the rate-limit counters failed: {:#}", e);
				}
			}
		})
	};
	vec![tokio::spawn(watcher::run_labels(app.clone())), tokio::spawn(watcher::run(app)), health, sweep]
}

/// Serves the API on `listener`, with each caller's address available to
/// the protections.
pub async fn serve(listener: tokio::net::TcpListener, app: Arc<App>) -> std::io::Result<()> {
	let router = api::router(app);
	axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await
}
