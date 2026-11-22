//! Runs the service inside the test process, against a real Ark server,
//! barkd, bitcoind and lightningd from the Scuttle testing crate.
//!
//! The daemons are the binaries in `CAPTAIND_EXEC`, `BARKD_EXEC`,
//! `BITCOIND_EXEC` and `LIGHTNINGD_EXEC`. Tests that need a Scuttle release
//! with S1 and S4 run only with `BULLNYM_TEST_S1_S4=1`. Without it, barkd
//! ignores the hint request, and the service runs with the test override
//! that accepts store invoices without the Ark hint (see
//! `bullnym::TestOverrides`), which no config can set.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use bitcoin::bech32::{Bech32m, ByteIterExt, Fe32, Fe32IterExt, Hrp};
use bitcoin::bip32::{Xpriv, Xpub};
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use bitcoin::{Amount, Network};
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use ark_testing::context::LightningPaymentSetup;
use ark_testing::util::poll_interval;
use ark_testing::{Barkd, Bitcoind, Captaind, TestContext, btc};
use bullnym::TestOverrides;
use bullnym::config::{self, Config};
use bullnym::rails::onchain::BitcoindConfig;
use bullnym::registration::{NymRegistration, StoreRegistration};


/// The bitcoin price in tests: 62,500,000.00 per bitcoin in every currency,
/// so 12,500.00 is 20,000 sats.
pub const TEST_BTC_PRICE: &str = "62500000.00";

pub const SALE_TIMEOUT: Duration = Duration::from_secs(90);

/// Whether the daemons have S1 and S4 (`BULLNYM_TEST_S1_S4=1`).
pub fn s1_s4() -> bool {
	std::env::var("BULLNYM_TEST_S1_S4").is_ok_and(|v| v == "1")
}

/// Skips a test that needs S1 and S4 when the daemons lack them.
#[macro_export]
macro_rules! require_s1_s4 {
	($name:expr) => {
		if !common::s1_s4() {
			eprintln!("SKIPPED {}: needs a Scuttle release with S1 and S4 (BULLNYM_TEST_S1_S4=1)", $name);
			return;
		}
	};
}

fn postgres_config(host: &str, port: u16, name: &str, user: Option<String>, password: Option<String>) -> config::Postgres {
	config::Postgres {
		host: host.to_string(),
		port,
		name: name.to_string(),
		user,
		password,
		max_connections: 5,
	}
}

/// A test Silent Payment address for regtest.
pub fn silent_payment_address() -> String {
	let secp = Secp256k1::new();
	let scan = SecretKey::from_slice(&[11u8; 32]).unwrap().public_key(&secp);
	let spend = SecretKey::from_slice(&[12u8; 32]).unwrap().public_key(&secp);
	let payload = [scan.serialize(), spend.serialize()].concat();
	payload.iter().copied().bytes_to_fes()
		.with_checksum::<Bech32m>(&Hrp::parse("tsp").unwrap())
		.with_witness_version(Fe32::Q)
		.chars()
		.collect()
}

/// The public descriptor of the on-chain account the wallet chose.
pub fn store_descriptor(seed: u8) -> String {
	let xpriv = Xpriv::new_master(Network::Regtest, &[seed; 32]).unwrap();
	format!("tr({}/0/*)", Xpub::from_priv(&Secp256k1::new(), &xpriv))
}

/// The service, running in the test process.
pub struct Service {
	pub app: Arc<bullnym::App>,
	/// The service's own bitcoind, so its watch-only wallet does not disturb
	/// the wallet the tests fund from.
	pub bitcoind: Bitcoind,
	pub url: String,
	http: reqwest::Client,
	tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Service {
	fn drop(&mut self) {
		for t in &self.tasks {
			t.abort();
		}
	}
}

/// The test overrides of the daemons in use: a Scuttle release without S1
/// returns store invoices without the Ark hint.
pub fn default_overrides() -> TestOverrides {
	TestOverrides { accept_unhinted_invoices: !s1_s4() }
}

/// The config of a test service. Rate limits are loose: the tests of the
/// limits set their own.
pub async fn test_config(
	ctx: &TestContext,
	srv: &Captaind,
	forwarder: &Barkd,
	bitcoind: &Bitcoind,
	port: u16,
	db_name: &str,
) -> Config {
	let url = format!("http://127.0.0.1:{}", port);
	let pg = ctx.new_postgres(db_name).await;
	let token = std::fs::read_to_string(forwarder.datadir().join("auth_token"))
		.expect("barkd writes its auth token");
	let mut cfg = Config {
		network: Network::Regtest,
		http: config::Http { listen: format!("127.0.0.1:{}", port).parse().unwrap(), public_url: url },
		postgres: postgres_config(
			&pg.host, pg.port, &pg.name, pg.user.clone(), pg.password.as_ref().map(|p| p.leak_ref().clone()),
		),
		ark_server_url: srv.ark_url(),
		barkd: config::Barkd { url: forwarder.base_url(), token: Some(token.trim().to_string()) },
		bitcoind: BitcoindConfig {
			url: bitcoind.rpc_url(),
			cookie: Some(bitcoind.rpc_cookie()),
			rpc_user: None,
			rpc_password: None,
			wallet: "bullnym".into(),
		},
		price: config::Price { url: String::new(), fixed_btc_price: Some(TEST_BTC_PRICE.into()), cache_secs: 60 },
		nyms: Default::default(),
		sales: config::Sales { poll_interval_ms: 200, ..Default::default() },
		limits: Default::default(),
		certification: Default::default(),
		watcher: config::Watcher { max_connections: 4, health_interval_secs: 1 },
	};
	let l = &mut cfg.limits;
	l.register_per_minute = 1000;
	l.register_identities_per_hour = 1000;
	l.lookups_per_minute = 10_000;
	l.lookup_names_per_hour = 10_000;
	l.callbacks_per_minute = 1000;
	l.callbacks_per_nym_per_minute = 1000;
	l.sales_per_minute = 1000;
	l.status_reads_per_minute = 100_000;
	l.owner_per_minute = 1000;
	l.pin_checks_per_minute = 1000;
	cfg
}

impl Service {
	pub async fn start(ctx: &TestContext, srv: &Captaind, forwarder: &Barkd) -> Service {
		Self::start_with(ctx, srv, forwarder, |_| {}).await
	}

	pub async fn start_with(
		ctx: &TestContext,
		srv: &Captaind,
		forwarder: &Barkd,
		tweak: impl FnOnce(&mut Config),
	) -> Service {
		Self::try_start_with(ctx, srv, forwarder, tweak).await
			.map_err(|e| format!("{:#}", e)).expect("the service failed to start")
	}

	pub async fn try_start_with(
		ctx: &TestContext,
		srv: &Captaind,
		forwarder: &Barkd,
		tweak: impl FnOnce(&mut Config),
	) -> anyhow::Result<Service> {
		Self::try_start_overridden(ctx, srv, forwarder, tweak, default_overrides()).await
	}

	pub async fn try_start_overridden(
		ctx: &TestContext,
		srv: &Captaind,
		forwarder: &Barkd,
		tweak: impl FnOnce(&mut Config),
		overrides: TestOverrides,
	) -> anyhow::Result<Service> {
		// A test may start more than one service: each has its own node and
		// database.
		static STARTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
		let n = STARTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
		let suffix = if n == 0 { String::new() } else { n.to_string() };
		let port = ark_testing::ports::pick_port();
		let node = format!("service_bitcoind{}", suffix);
		let mut btc_cfg = ctx.bitcoind_default_cfg(&node);
		btc_cfg.wallet = true;
		let bitcoind = ctx.new_bitcoind_with_cfg(&node, btc_cfg).await;
		let db_name = format!("{}_svc{}", ctx.test_name.replace('/', "_"), suffix);
		let mut cfg = test_config(ctx, srv, forwarder, &bitcoind, port, &db_name).await;
		tweak(&mut cfg);
		let listen = cfg.http.listen;
		let url = cfg.http.public_url.clone();
		let app = bullnym::App::new_with_overrides(cfg, overrides).await?;
		let mut tasks = bullnym::spawn_background(app.clone());
		let listener = tokio::net::TcpListener::bind(listen).await?;
		let served = app.clone();
		tasks.push(tokio::spawn(async move {
			bullnym::serve(listener, served).await.unwrap();
		}));
		Ok(Service { app, bitcoind, url, http: reqwest::Client::new(), tasks })
	}

	/// Calls the API. Returns the status and the JSON body (a string if it
	/// is not JSON).
	pub async fn call_with(
		&self,
		method: Method,
		path: &str,
		token: Option<&str>,
		body: Option<Value>,
		headers: HeaderMap,
	) -> (StatusCode, Value) {
		let mut req = self.http.request(method, format!("{}{}", self.url, path)).headers(headers);
		if let Some(token) = token {
			req = req.bearer_auth(token);
		}
		if let Some(body) = body {
			req = req.json(&body);
		}
		let resp = req.send().await.expect("request to the service failed");
		let status = resp.status();
		let text = resp.text().await.unwrap();
		let json = serde_json::from_str(&text).unwrap_or(Value::String(text));
		(status, json)
	}

	pub async fn call(&self, method: Method, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
		self.call_with(method, path, token, body, HeaderMap::new()).await
	}

	/// Calls the API and expects a 2xx answer.
	pub async fn ok(&self, method: Method, path: &str, token: Option<&str>, body: Option<Value>) -> Value {
		let (status, json) = self.call(method, path, token, body).await;
		assert!(status.is_success(), "{} failed with {}: {}", path, status, json);
		json
	}

	/// Waits until the sale reaches `state`, or panics after `timeout`.
	pub async fn wait_sale_state(&self, id: &str, state: &str, timeout: Duration) -> Value {
		self.wait_sale(id, timeout, |s| s["state"] == state, state).await
	}

	pub async fn wait_sale(
		&self,
		id: &str,
		timeout: Duration,
		done: impl Fn(&Value) -> bool,
		what: &str,
	) -> Value {
		let deadline = tokio::time::Instant::now() + timeout;
		loop {
			let sale = self.ok(Method::GET, &format!("/api/v1/sales/{}", id), None, None).await;
			if done(&sale) {
				return sale;
			}
			if tokio::time::Instant::now() > deadline {
				panic!("sale {} is not {} after {:?}: {}", id, what, timeout, sale);
			}
			tokio::time::sleep(poll_interval()).await;
		}
	}

	/// Runs a SQL statement on the service's database.
	pub async fn sql(&self, statement: &str) -> Result<u64, tokio_postgres::Error> {
		let conn = self.app.public.db.conn().await.unwrap();
		conn.execute(statement, &[]).await
	}
}

/// The owner's wallet, which registers the nym and the store and then goes
/// offline.
pub struct Owner {
	pub seed: bip39::Mnemonic,
	pub address: ark::Address,
	pub token: String,
}

/// An Ark server with a forwarding barkd, the service, and a registered
/// nym and store.
pub struct Store {
	pub srv: Arc<Captaind>,
	pub lightning: LightningPaymentSetup,
	pub svc: Service,
	pub owner: Owner,
	pub nym: String,
	pub forwarder: Barkd,
}

/// Starts an Ark server in expiry payout mode with Ark address hints, its
/// Lightning nodes and a forwarding barkd.
pub async fn ark_server(ctx: &TestContext) -> (Arc<Captaind>, LightningPaymentSetup, Barkd) {
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10))
		.cfg(|cfg| {
			cfg.expiry_payouts = true;
			cfg.ln_receive_ark_address_hints = true;
		}).create().await;
	// Lightning receives draw their VTXOs from the pool. Confirm the pool
	// transaction, so a node that starts later knows the VTXO anchor.
	srv.wait_for_vtxopool(ctx).await;
	ctx.generate_blocks(1).await;
	let forwarder = ctx.barkd("forwarder", &*srv).create().await;
	(srv, lightning, forwarder)
}

pub async fn sign(wallet: &bark::Wallet, message: &str, address: &ark::Address) -> String {
	wallet.sign_message(message.as_bytes(), address).await.unwrap()
		.expect("the wallet holds the key").to_string()
}

/// Registers `nym` for a new address of `wallet`.
pub async fn register_nym(svc: &Service, wallet: &bark::Wallet, nym: &str) -> (ark::Address, (StatusCode, Value)) {
	let address = wallet.new_address().await.unwrap();
	let reg = NymRegistration {
		nym: nym.into(),
		ark_address: address.to_string(),
		silent_payment_address: silent_payment_address(),
		timestamp: chrono::Utc::now().timestamp() as u64,
	};
	let sig = sign(wallet, &reg.message(), &address).await;
	let res = svc.call(Method::POST, "/api/v1/nym", None, Some(json!({
		"registration": reg, "signature": sig,
	}))).await;
	(address, res)
}

pub async fn register_store(
	svc: &Service,
	wallet: &bark::Wallet,
	address: &ark::Address,
	nym: &str,
	descriptor: String,
) -> (StatusCode, Value) {
	let reg = StoreRegistration {
		name: "Café Uvita".into(),
		ark_address: address.to_string(),
		mailbox_id: wallet.mailbox_identifier().to_string(),
		onchain_descriptor: descriptor,
		currency: "CRC".into(),
		timestamp: chrono::Utc::now().timestamp() as u64,
		nym: nym.into(),
	};
	let sig = sign(wallet, &reg.message(), address).await;
	svc.call(Method::POST, "/api/v1/stores", None, Some(json!({
		"registration": reg, "signature": sig,
	}))).await
}

pub async fn login(svc: &Service, wallet: &bark::Wallet, address: &ark::Address) -> String {
	let challenge = svc.ok(Method::POST, "/api/v1/owner/challenge", None, Some(json!({
		"ark_address": address.to_string(),
	}))).await["challenge"].as_str().unwrap().to_string();
	let sig = sign(wallet, &challenge, address).await;
	svc.ok(Method::POST, "/api/v1/owner/login", None, Some(json!({
		"ark_address": address.to_string(), "challenge": challenge, "signature": sig,
	}))).await["token"].as_str().unwrap().to_string()
}

/// A registered nym and store whose owner is offline.
pub async fn store_setup(ctx: &TestContext) -> Store {
	store_setup_with(ctx, |_| {}).await
}

pub async fn store_setup_with(ctx: &TestContext, tweak: impl FnOnce(&mut Config)) -> Store {
	let (srv, lightning, forwarder) = ark_server(ctx).await;
	let seed = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("owner", &srv).mnemonic(seed.clone()).create().await;
	let svc = Service::start_with(ctx, &srv, &forwarder, tweak).await;
	let nym = "cafe".to_string();
	let (address, (status, body)) = register_nym(&svc, &wallet, &nym).await;
	assert!(status.is_success(), "nym registration failed: {} {}", status, body);
	let (status, body) = register_store(&svc, &wallet, &address, &nym, store_descriptor(42)).await;
	assert!(status.is_success(), "store registration failed: {} {}", status, body);
	let token = login(&svc, &wallet, &address).await;
	drop(wallet);
	Store { srv, lightning, svc, owner: Owner { seed, address, token }, nym, forwarder }
}

impl Store {
	/// Creates a sale through the store's public checkout.
	pub async fn new_sale(&self, body: Value) -> Value {
		self.svc.ok(Method::POST, &format!("/api/v1/checkout/{}/sales", self.nym), None, Some(body)).await
	}
}

/// Calls bitcoind's JSON-RPC.
pub async fn rpc(bitcoind: &Bitcoind, wallet: Option<&str>, method: &str, params: Value) -> Value {
	let cookie = std::fs::read_to_string(bitcoind.rpc_cookie()).unwrap();
	let (user, pass) = cookie.trim().split_once(':').unwrap();
	let url = match wallet {
		Some(w) => format!("{}/wallet/{}", bitcoind.rpc_url(), w),
		None => bitcoind.rpc_url(),
	};
	let resp: Value = reqwest::Client::new().post(url).basic_auth(user, Some(pass))
		.json(&json!({ "jsonrpc": "1.0", "id": "test", "method": method, "params": params }))
		.send().await.unwrap().json().await.unwrap();
	assert!(resp["error"].is_null(), "bitcoind {} failed: {}", method, resp["error"]);
	resp["result"].clone()
}

/// The owner restores the wallet from the seed and sees the funds.
pub async fn owner_restores(ctx: &TestContext, store: &Store, at_least: Amount) {
	let restored = ctx.bark_sdk("owner-restored", &store.srv)
		.mnemonic(store.owner.seed.clone()).create().await;
	let deadline = tokio::time::Instant::now() + SALE_TIMEOUT;
	loop {
		restored.sync().await;
		let balance = restored.balance().await.unwrap();
		if balance.spendable >= at_least {
			return;
		}
		assert!(tokio::time::Instant::now() < deadline,
			"owner has {} spendable, expected at least {}", balance.spendable, at_least);
		tokio::time::sleep(Duration::from_millis(500)).await;
	}
}
