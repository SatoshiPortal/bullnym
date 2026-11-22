use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use bitcoin::Network;
use serde::{Deserialize, Serialize};


/// A Postgres connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Postgres {
	pub host: String,
	#[serde(default = "default_postgres_port")]
	pub port: u16,
	pub name: String,
	pub user: Option<String>,
	#[serde(skip_serializing)]
	pub password: Option<String>,
	#[serde(default = "default_max_connections")]
	pub max_connections: u32,
}

fn default_postgres_port() -> u16 { 5432 }
fn default_max_connections() -> u32 { 10 }

impl Postgres {
	pub fn to_pg_config(&self) -> tokio_postgres::Config {
		let mut cfg = tokio_postgres::Config::new();
		cfg.host(&self.host).port(self.port).dbname(&self.name);
		if let Some(user) = &self.user {
			cfg.user(user);
		}
		if let Some(password) = &self.password {
			cfg.password(password);
		}
		cfg
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Http {
	/// The address the service listens on.
	pub listen: SocketAddr,
	/// The public base URL, without a trailing slash. Its host is the domain
	/// of every Lightning Address, and cashier links start with it.
	pub public_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Barkd {
	/// The base URL of the private barkd that issues forwarding invoices.
	pub url: String,
	/// The barkd bearer token.
	#[serde(skip_serializing)]
	pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Price {
	/// The Bull Bitcoin price API endpoint.
	#[serde(default = "default_price_url")]
	pub url: String,
	/// A fixed price per bitcoin in every currency, in place of the API. For
	/// test networks only.
	pub fixed_btc_price: Option<String>,
	#[serde(default = "default_price_cache_secs")]
	pub cache_secs: u64,
}

fn default_price_url() -> String { "https://api.bullbitcoin.com/public/price".into() }
fn default_price_cache_secs() -> u64 { 60 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sales {
	/// How long an open sale and its invoices stay valid.
	#[serde(default = "default_sale_expiry_secs")]
	pub expiry_secs: u64,
	/// The on-chain minimum for a new store, in sats.
	#[serde(default = "default_onchain_min_sat")]
	pub default_onchain_min_sat: u64,
	/// How often the watcher checks open sales.
	#[serde(default = "default_poll_interval_ms")]
	pub poll_interval_ms: u64,
	/// How long the watcher keeps checking an on-chain address after its
	/// sale was created, to record late or extra payments. An address is
	/// never handed out again, so a late payment always shows on its own
	/// sale.
	#[serde(default = "default_onchain_watch_secs")]
	pub onchain_watch_secs: u64,
	/// The confirmations that make an on-chain payment final.
	#[serde(default = "default_final_confirmations")]
	pub final_confirmations: u32,
	/// A payment whose fee rate is below this share of bitcoind's estimate
	/// gets the low-fee warning.
	#[serde(default = "default_low_fee_ratio")]
	pub low_fee_ratio: f64,
	/// A payment below this fee rate, in sat/vB, gets the low-fee warning
	/// whatever the estimate.
	#[serde(default = "default_low_fee_floor_sat_vb")]
	pub low_fee_floor_sat_vb: f64,
	/// The confirmation target of the fee estimate, in blocks.
	#[serde(default = "default_fee_estimate_blocks")]
	pub fee_estimate_blocks: u16,
}

fn default_sale_expiry_secs() -> u64 { 600 }
fn default_onchain_min_sat() -> u64 { 50_000 }
fn default_poll_interval_ms() -> u64 { 2_000 }
fn default_onchain_watch_secs() -> u64 { 7 * 24 * 3600 }
fn default_final_confirmations() -> u32 { 3 }
fn default_low_fee_ratio() -> f64 { 0.5 }
fn default_low_fee_floor_sat_vb() -> f64 { 1.0 }
fn default_fee_estimate_blocks() -> u16 { 3 }

impl Default for Sales {
	fn default() -> Self {
		Sales {
			expiry_secs: default_sale_expiry_secs(),
			default_onchain_min_sat: default_onchain_min_sat(),
			poll_interval_ms: default_poll_interval_ms(),
			onchain_watch_secs: default_onchain_watch_secs(),
			final_confirmations: default_final_confirmations(),
			low_fee_ratio: default_low_fee_ratio(),
			low_fee_floor_sat_vb: default_low_fee_floor_sat_vb(),
			fee_estimate_blocks: default_fee_estimate_blocks(),
		}
	}
}

impl Sales {
	pub fn expiry(&self) -> Duration { Duration::from_secs(self.expiry_secs) }
	pub fn poll_interval(&self) -> Duration { Duration::from_millis(self.poll_interval_ms) }
}

/// The Lightning Address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nyms {
	/// Names that nobody can register, on top of the built-in list of route
	/// and brand names.
	#[serde(default)]
	pub reserved: Vec<String>,
	/// The smallest Lightning Address payment, in sats.
	#[serde(default = "default_min_sendable_sat")]
	pub min_sendable_sat: u64,
	/// The largest Lightning Address payment, in sats.
	#[serde(default = "default_max_sendable_sat")]
	pub max_sendable_sat: u64,
	/// How long a Lightning Address invoice stays payable. barkd cancels it
	/// at expiry.
	#[serde(default = "default_invoice_expiry_secs")]
	pub invoice_expiry_secs: u64,
}

fn default_min_sendable_sat() -> u64 { 1 }
fn default_max_sendable_sat() -> u64 { 10_000_000 }
fn default_invoice_expiry_secs() -> u64 { 600 }

impl Default for Nyms {
	fn default() -> Self {
		Nyms {
			reserved: Vec::new(),
			min_sendable_sat: default_min_sendable_sat(),
			max_sendable_sat: default_max_sendable_sat(),
			invoice_expiry_secs: default_invoice_expiry_secs(),
		}
	}
}

impl Nyms {
	pub fn invoice_expiry(&self) -> Duration { Duration::from_secs(self.invoice_expiry_secs) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
	pub network: Network,
	pub http: Http,
	pub postgres: Postgres,
	/// The Ark server's public gRPC address.
	pub ark_server_url: String,
	pub barkd: Barkd,
	/// The bitcoind that watches on-chain payments.
	pub bitcoind: crate::rails::onchain::BitcoindConfig,
	pub price: Price,
	#[serde(default)]
	pub nyms: Nyms,
	#[serde(default)]
	pub sales: Sales,
	#[serde(default)]
	pub startup: Startup,
	#[serde(default)]
	pub limits: crate::protect::Limits,
	#[serde(default)]
	pub certification: crate::protect::Certification,
	#[serde(default)]
	pub watcher: Watcher,
}

/// The watchers' own resources, kept apart from public traffic so a flood
/// cannot stop payment detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watcher {
	/// Database connections of the watchers, on top of
	/// `postgres.max_connections` for public traffic.
	#[serde(default = "default_watcher_connections")]
	pub max_connections: u32,
	/// How often the backends are probed for health.
	#[serde(default = "default_health_interval_secs")]
	pub health_interval_secs: u64,
}

fn default_watcher_connections() -> u32 { 4 }
fn default_health_interval_secs() -> u64 { 5 }

impl Default for Watcher {
	fn default() -> Self {
		Watcher {
			max_connections: default_watcher_connections(),
			health_interval_secs: default_health_interval_secs(),
		}
	}
}

/// What the service checks before it starts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Startup {
	/// Starts even when barkd and the Ark server cannot put a forwarded
	/// receive's destination in its invoice (S1), or the Ark server does not
	/// report the delivery of Ark payments (S4). Store invoices then have
	/// no Ark hint, and are paid over Lightning only. For local tests
	/// against older Scuttle releases only; never set it in production.
	#[serde(default)]
	pub allow_missing_s1_s4: bool,
}

impl Config {
	/// Loads a config file, then applies `BULLNYM__` environment variables, for
	/// example `BULLNYM__POSTGRES__HOST`.
	pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
		let mut builder = config::Config::builder();
		if let Some(path) = path {
			builder = builder.add_source(config::File::from(path));
		}
		let cfg: Config = builder.add_source(config::Environment::with_prefix("BULLNYM").separator("__"))
			.build().context("failed to read the config")?
			.try_deserialize().context("the config is not valid")?;
		cfg.validate()?;
		Ok(cfg)
	}

	/// Checks the rules between settings.
	pub fn validate(&self) -> anyhow::Result<()> {
		let s = &self.sales;
		if s.final_confirmations == 0 {
			anyhow::bail!("sales.final_confirmations must be at least 1");
		}
		if self.nyms.min_sendable_sat == 0 || self.nyms.min_sendable_sat > self.nyms.max_sendable_sat {
			anyhow::bail!("nyms.min_sendable_sat must be at least 1 and at most nyms.max_sendable_sat");
		}
		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn settings_rules() {
		let mut sales = Sales::default();
		assert!(sales.onchain_watch_secs >= 7 * 24 * 3600, "late payments are watched for 7 days");
		let cfg = |sales: Sales| Config {
			network: Network::Regtest,
			http: Http { listen: "127.0.0.1:1".parse().unwrap(), public_url: "http://x".into() },
			postgres: Postgres {
				host: "h".into(), port: 1, name: "n".into(), user: None, password: None, max_connections: 1,
			},
			ark_server_url: "http://a".into(),
			barkd: Barkd { url: "http://b".into(), token: None },
			bitcoind: crate::rails::onchain::BitcoindConfig {
				url: "http://c".into(), cookie: None, rpc_user: None, rpc_password: None, wallet: "w".into(),
			},
			price: Price { url: String::new(), fixed_btc_price: None, cache_secs: 1 },
			nyms: Nyms::default(),
			sales,
			startup: Startup::default(),
			limits: Default::default(),
			certification: Default::default(),
			watcher: Watcher::default(),
		};
		assert!(cfg(sales.clone()).validate().is_ok());
		sales.final_confirmations = 0;
		assert!(cfg(sales).validate().is_err());
	}

	/// The documented default config parses, and documents the defaults.
	#[test]
	fn default_config_file() {
		let cfg: Config = config::Config::builder()
			.add_source(config::File::from_str(
				include_str!("../bullnym.default.toml"), config::FileFormat::Toml,
			))
			.build().unwrap()
			.try_deserialize().unwrap();
		cfg.validate().unwrap();
		assert_eq!(json(&cfg.nyms), json(&Nyms::default()));
		assert_eq!(json(&cfg.sales), json(&Sales::default()));
		assert_eq!(json(&cfg.startup), json(&Startup::default()));
		assert_eq!(json(&cfg.limits), json(&crate::protect::Limits::default()));
		assert_eq!(json(&cfg.certification), json(&crate::protect::Certification::default()));
		assert_eq!(json(&cfg.watcher), json(&Watcher::default()));
		assert_eq!(cfg.postgres.max_connections, default_max_connections());
		assert_eq!(cfg.price.cache_secs, default_price_cache_secs());
		assert_eq!(cfg.price.url, default_price_url());
		assert_eq!(cfg.bitcoind.wallet, "bullnym");
	}

	fn json<T: Serialize>(v: &T) -> String {
		serde_json::to_string(v).unwrap()
	}
}
