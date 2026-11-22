//! End-to-end tests of the service.
//!
//! The service runs in the test process against a real Ark server, a barkd
//! that forwards Lightning receives, lightningd nodes and bitcoind. The
//! owner's wallet registers the nym and the store, then goes offline for
//! every payment.
//!
//! Lee Salminen's posd tests are the first four, adapted: one invoice for
//! every payer, the cashier link instead of paired terminals, and the
//! history and CSV behind the owner login.

mod common;

use std::str::FromStr;
use std::time::Duration;

use ark::lightning::Bolt11InvoiceExt;
use lightning_invoice::Bolt11Invoice;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use bark::Wallet;
use bark::payment_request::PaymentRequest;

use ark_testing::{TestContext, sat};
use bullnym::error::UNAVAILABLE_MESSAGE;
use common::{SALE_TIMEOUT, Service, Store, owner_restores, rpc, store_setup, store_setup_with};


/// A payer that pays a payment request the default way. Returns the kind
/// of payment it chose.
async fn pay(payer: &Wallet, payment_request: &str) -> String {
	let req: PaymentRequest = payer.parse_payment_request(payment_request).await.unwrap();
	let option = req.default_option().expect("no usable payment option").clone();
	let kind = option.method.type_str().to_string();
	payer.send_payment(&option.method, None, None::<String>, true).await.unwrap();
	kind
}

fn invoice_hint(invoice: &str) -> Option<String> {
	Bolt11Invoice::from_str(invoice).unwrap().ark_address().map(|a| a.to_string())
}

fn warnings(sale: &Value) -> Vec<String> {
	sale["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_string()).collect()
}

async fn owner_csv(store: &Store) -> String {
	reqwest::Client::new().get(format!("{}/api/v1/owner/sales.csv", store.svc.url))
		.bearer_auth(&store.owner.token).send().await.unwrap().text().await.unwrap()
}

#[tokio::test]
async fn sale_paid_by_lightning() {
	let ctx = TestContext::new("bullnym/sale_paid_by_lightning").await;
	let store = store_setup(&ctx).await;

	// 12,500 CRC at the test price is 20,000 sats, below the on-chain minimum.
	let sale = store.new_sale(json!({
		"fiat_amount": "12500", "memo": "2 coffees", "cashier_name": "Front till",
	})).await;
	assert_eq!(sale["state"], "open");
	assert_eq!(sale["amount_sat"], 20_000);
	assert_eq!(sale["fiat_amount"], "12500.00");
	assert_eq!(sale["onchain_address"], Value::Null);
	let pr = sale["payment_request"].as_str().unwrap();
	assert!(pr.starts_with("bitcoin:?lightning="), "{}", pr);
	assert!(!pr.contains("ark-lightning"), "one invoice only: {}", pr);
	// The one invoice names the owner's registered address as its Ark hint
	// (S1). Without S1, barkd leaves the hint out.
	let invoice = sale["lightning_invoice"].as_str().unwrap();
	if common::s1_s4() {
		assert_eq!(invoice_hint(invoice), Some(store.owner.address.to_string()));
	} else {
		assert_eq!(invoice_hint(invoice), None);
	}

	store.lightning.external.pay_bolt11(invoice).await;
	let id = sale["id"].as_str().unwrap();
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "lightning");
	assert_eq!(paid["final"], true, "a delivered invoice is final");

	owner_restores(&ctx, &store, sat(15_000)).await;

	// The cashier link cannot list sales or export them.
	let (status, _) = store.svc.call(Method::GET, "/api/v1/sales", None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = store.svc.call(Method::GET, "/api/v1/owner/sales.csv", None, None).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);
	let csv = owner_csv(&store).await;
	assert!(csv.contains(id) && csv.contains("2 coffees") && csv.contains(",paid,lightning,"), "{}", csv);
	assert!(csv.contains(",2 coffees,Front till,"), "{}", csv);
}

#[tokio::test]
async fn sale_paid_by_ark() {
	require_s1_s4!("sale_paid_by_ark");
	let ctx = TestContext::new("bullnym/sale_paid_by_ark").await;
	let store = store_setup(&ctx).await;
	let payer = ctx.bark_sdk("payer", &store.srv).boarded(sat(200_000))
		.cfg(|cfg| cfg.lightning_ark_address_hints = true)
		.create().await;

	let sale = store.new_sale(json!({ "amount_sat": 30_000 })).await;
	let id = sale["id"].as_str().unwrap();
	let kind = pay(&payer, sale["payment_request"].as_str().unwrap()).await;
	// Paid only once the Ark server reports the coins delivered (S4).
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "ark", "payer chose {}", kind);
	assert_eq!(paid["received_sat"], 30_000);

	// The invoice is settled: the sale cannot be paid twice.
	let again = payer.parse_payment_request(sale["payment_request"].as_str().unwrap()).await.unwrap();
	let option = again.default_option().expect("an option").clone();
	assert!(payer.send_payment(&option.method, None, None::<String>, true).await.is_err(),
		"the server must refuse to pay a settled invoice twice");

	owner_restores(&ctx, &store, sat(30_000)).await;
}

#[tokio::test]
async fn payer_with_hints_off_pays_lightning() {
	let ctx = TestContext::new("bullnym/payer_with_hints_off_pays_lightning").await;
	let store = store_setup(&ctx).await;
	// A wallet with address hints off pays over Lightning, as upstream does.
	let payer = ctx.bark_sdk("payer", &store.srv).boarded(sat(200_000))
		.cfg(|cfg| cfg.lightning_ark_address_hints = false)
		.create().await;

	let sale = store.new_sale(json!({ "amount_sat": 25_000 })).await;
	let id = sale["id"].as_str().unwrap();
	pay(&payer, sale["payment_request"].as_str().unwrap()).await;
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "lightning");
}

#[tokio::test]
async fn sale_paid_onchain_through_its_stages() {
	let ctx = TestContext::new("bullnym/sale_paid_onchain_through_its_stages").await;
	let store = store_setup(&ctx).await;

	// At or above the 50,000-sat minimum, the sale offers an address.
	let sale = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	let id = sale["id"].as_str().unwrap();
	let address = sale["onchain_address"].as_str().expect("an on-chain address").to_string();
	assert!(sale["payment_request"].as_str().unwrap()
		.starts_with(&format!("bitcoin:{}?amount=0.0006&lightning=", address)));
	let second = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	assert_ne!(second["onchain_address"].as_str().unwrap(), address, "each open sale has its own address");

	// Seen in the mempool: in progress, not guaranteed, and nothing counted.
	ctx.bitcoind().fund_addr(&address, sat(60_000)).await;
	let seen = store.svc.wait_sale_state(id, "in-progress", SALE_TIMEOUT).await;
	assert_eq!(seen["progress"], "onchain");
	assert!(warnings(&seen).contains(&"not-guaranteed".to_string()), "{}", seen);
	assert_eq!(seen["received_sat"], 0);
	assert_eq!(seen["seen_sat"], 60_000);
	assert_eq!(seen["paid_at"], Value::Null);
	assert_eq!(seen["paid_rail"], Value::Null);
	// A sale in progress cannot be canceled.
	let (status, _) = store.svc.call(Method::POST, &format!("/api/v1/sales/{}/cancel", id), None, None).await;
	assert_eq!(status, StatusCode::CONFLICT);

	// Paid at 1 confirmation, final at 3.
	ctx.generate_blocks(1).await;
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "onchain");
	assert_eq!(paid["received_sat"], 60_000);
	assert_eq!(paid["final"], false);
	assert!(warnings(&paid).is_empty(), "{}", paid);
	ctx.generate_blocks(2).await;
	let fin = store.svc.wait_sale(id, SALE_TIMEOUT, |s| s["final"] == true, "final").await;
	assert_eq!(fin["state"], "paid");

	let csv = owner_csv(&store).await;
	assert!(csv.contains(id) && csv.contains(",paid,onchain,60000,60000,"), "{}", csv);
}

#[tokio::test]
async fn lightning_address_and_lud22() {
	let ctx = TestContext::new("bullnym/lightning_address_and_lud22").await;
	let store = store_setup(&ctx).await;
	let svc = &store.svc;

	let pay_request = svc.ok(Method::GET, "/.well-known/lnurlp/cafe", None, None).await;
	assert_eq!(pay_request["tag"], "payRequest");
	assert_eq!(pay_request["payment_methods"], json!(["ARK", "SP"]));
	assert!(pay_request["metadata"].as_str().unwrap().contains("cafe@127.0.0.1"));
	let callback = pay_request["callback"].as_str().unwrap();
	let path = callback.strip_prefix(&svc.url).unwrap().to_string();

	// The default callback: a forwarding invoice without a hint, to the
	// registered address. Any Lightning wallet pays it.
	let reply = svc.ok(Method::GET, &format!("{}?amount=21000000", path), None, None).await;
	let invoice = reply["pr"].as_str().unwrap();
	assert_eq!(invoice_hint(invoice), None, "the Lightning Address invoice has no hint");
	store.lightning.external.pay_bolt11(invoice).await;
	let deadline = tokio::time::Instant::now() + SALE_TIMEOUT;
	loop {
		let payments = svc.ok(Method::GET, "/api/v1/owner/nym/payments", Some(&store.owner.token), None).await;
		let p = &payments["payments"][0];
		assert_eq!(p["amount_sat"], 21_000);
		if p["state"] == "paid" {
			break;
		}
		assert!(tokio::time::Instant::now() < deadline, "Lightning Address payment not recorded: {}", p);
		tokio::time::sleep(Duration::from_millis(200)).await;
	}

	// LUD-22: each method answers with only that method's static address,
	// and creates no invoice.
	let open_before = svc.ok(Method::GET, "/api/v1/owner/nym/payments", Some(&store.owner.token), None).await
		["payments"].as_array().unwrap().len();
	let ark = svc.ok(Method::GET, &format!("{}?amount=5000000&payment_method=ARK", path), None, None).await;
	assert_eq!(ark, json!({ "ARK": { "address": store.owner.address.to_string() } }));
	let sp = svc.ok(Method::GET, &format!("{}?amount=5000000&payment_method=SP", path), None, None).await;
	assert_eq!(sp, json!({ "SP": { "address": common::silent_payment_address() } }));
	let open_after = svc.ok(Method::GET, "/api/v1/owner/nym/payments", Some(&store.owner.token), None).await
		["payments"].as_array().unwrap().len();
	assert_eq!(open_before, open_after, "a LUD-22 method creates no invoice");

	// Errors use the LNURL shape.
	let (status, err) = svc.call(Method::GET, &format!("{}?amount=5000000&payment_method=L-BTC", path), None, None).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert_eq!(err["status"], "ERROR");
	let (status, _) = svc.call(Method::GET, &format!("{}?amount=1500", path), None, None).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "not whole sats");
	let (status, _) = svc.call(Method::GET, "/.well-known/lnurlp/nobody", None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	// Lookup.
	let found = svc.ok(Method::GET, "/api/v1/nym/cafe", None, None).await;
	assert_eq!((found["registered"].clone(), found["has_store"].clone()), (json!(true), json!(true)));
	let reserved = svc.ok(Method::GET, "/api/v1/nym/api", None, None).await;
	assert_eq!(reserved["available"], false);
	let free = svc.ok(Method::GET, "/api/v1/nym/someone-else", None, None).await;
	assert_eq!(free["available"], true);
}

#[tokio::test]
async fn cashier_link() {
	let ctx = TestContext::new("bullnym/cashier_link").await;
	let store = store_setup(&ctx).await;
	let svc = &store.svc;
	let owner = Some(store.owner.token.as_str());

	// The store has one link, /<nym>. Viewing it allocates nothing.
	let view = svc.ok(Method::GET, "/api/v1/checkout/cafe", None, None).await;
	assert_eq!(view["store"]["name"], "Café Uvita");
	assert!(view["store"]["lightning_address"].as_str().unwrap().starts_with("cafe@"));
	let (status, manifest) = svc.call(Method::GET, "/cafe/manifest.webmanifest", None, None).await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(manifest["start_url"], "/cafe");
	let owner_view = svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await;
	assert_eq!(owner_view["next_onchain_index"], 0, "viewing the checkout takes no address");
	assert!(owner_view["checkout_url"].as_str().unwrap().ends_with("/cafe"));
	let (status, _) = svc.call(Method::GET, "/api/v1/checkout/nobody", None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	// The link creates, follows and cancels its sale, without a token.
	let sale = store.new_sale(json!({ "amount_sat": 60_000, "cashier_name": "Back till" })).await;
	let id = sale["id"].as_str().unwrap();
	assert_eq!(sale["cashier_name"], "Back till");
	svc.ok(Method::GET, &format!("/api/v1/sales/{}", id), None, None).await;
	let canceled = svc.ok(Method::POST, &format!("/api/v1/sales/{}/cancel", id), None, None).await;
	assert_eq!(canceled["state"], "canceled");
	let (status, _) = svc.call(Method::POST, &format!("/api/v1/sales/{}/cancel", id), None, None).await;
	assert_eq!(status, StatusCode::CONFLICT);
	let (status, _) = svc.call(Method::GET, "/api/v1/sales/0123456789abcdef0123456789abcdef", None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	store.new_sale(json!({ "amount_sat": 1000 })).await;

	// The link is always on: the owner has no switch for it, and no other
	// link exists.
	let unchanged = svc.ok(Method::PATCH, "/api/v1/owner/store", owner,
		Some(json!({ "public_checkout": false }))).await;
	assert!(unchanged.get("public_checkout").is_none());
	svc.ok(Method::GET, "/api/v1/checkout/cafe", None, None).await;
	let (status, _) = svc.call(Method::POST, "/api/v1/owner/store/unlisted-link", owner, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	// History is the owner's only.
	let (status, _) = svc.call(Method::GET, "/api/v1/owner/sales", Some("not-a-token"), None).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);
	let history = svc.ok(Method::GET, "/api/v1/owner/sales", owner, None).await;
	let sales = history["sales"].as_array().unwrap();
	assert_eq!(sales.len(), 2);
	assert_eq!(sales[1]["state"], "canceled");
}

#[tokio::test]
async fn registration_rules() {
	let ctx = TestContext::new("bullnym/registration_rules").await;
	let (srv, _lightning, forwarder) = common::ark_server(&ctx).await;
	let svc = Service::start(&ctx, &srv, &forwarder).await;
	let alice = ctx.bark_sdk("alice", &srv).create().await;
	let bob = ctx.bark_sdk("bob", &srv).create().await;

	// A store needs the Lightning Address first.
	let early = alice.new_address().await.unwrap();
	let (status, _) = common::register_store(&svc, &alice, &early, "cafe", common::store_descriptor(1)).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	let (a1, (status, _)) = common::register_nym(&svc, &alice, "cafe").await;
	assert!(status.is_success());
	// The nym is taken for good, and the address has its nym.
	let (_, (status, _)) = common::register_nym(&svc, &bob, "cafe").await;
	assert_eq!(status, StatusCode::CONFLICT);
	let reg = bullnym::registration::NymRegistration {
		nym: "cafe2".into(), ark_address: a1.to_string(),
		silent_payment_address: common::silent_payment_address(),
		timestamp: chrono::Utc::now().timestamp() as u64,
	};
	let sig = common::sign(&alice, &reg.message(), &a1).await;
	let (status, _) = svc.call(Method::POST, "/api/v1/nym", None,
		Some(json!({ "registration": reg, "signature": sig }))).await;
	assert_eq!(status, StatusCode::CONFLICT, "one nym per Ark address");

	// Refused registrations.
	for (nym, why) in [("api", "reserved"), ("Cafe", "uppercase"), ("-x", "form")] {
		let (_, (status, _)) = common::register_nym(&svc, &bob, nym).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{}", why);
	}
	let b1 = bob.new_address().await.unwrap();
	let mut reg = bullnym::registration::NymRegistration {
		nym: "bob".into(), ark_address: b1.to_string(),
		silent_payment_address: common::silent_payment_address(),
		timestamp: chrono::Utc::now().timestamp() as u64 - 3600,
	};
	let sig = common::sign(&bob, &reg.message(), &b1).await;
	let (status, _) = svc.call(Method::POST, "/api/v1/nym", None,
		Some(json!({ "registration": reg.clone(), "signature": sig }))).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "an old timestamp");
	reg.timestamp = chrono::Utc::now().timestamp() as u64;
	reg.silent_payment_address = "sp1qqgste7k9hx0qftg6qmwlkqtwuy6cycyavzmzj85c6qdfhjdpdjtdgqjuexzk6murw56suy3e0rd2cgqvycxttddwsvgxe2usfpxumr70xc9pkqwv".into();
	let sig = common::sign(&bob, &reg.message(), &b1).await;
	let (status, _) = svc.call(Method::POST, "/api/v1/nym", None,
		Some(json!({ "registration": reg.clone(), "signature": sig }))).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "a mainnet Silent Payment address");
	reg.silent_payment_address = common::silent_payment_address();
	let sig = common::sign(&alice, &reg.message(), &a1).await;
	let (status, _) = svc.call(Method::POST, "/api/v1/nym", None,
		Some(json!({ "registration": reg, "signature": sig }))).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "signed by another key");

	// The store must use the nym's address, and there is one per nym.
	let a2 = alice.new_address().await.unwrap();
	let (status, body) = common::register_store(&svc, &alice, &a2, "cafe", common::store_descriptor(1)).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
	let (status, body) = common::register_store(&svc, &alice, &a1, "cafe", common::store_descriptor(1)).await;
	assert!(status.is_success(), "{}", body);
	assert!(body["store"]["lightning_address"].as_str().unwrap().starts_with("cafe@"));
	let (status, _) = common::register_store(&svc, &alice, &a1, "cafe", common::store_descriptor(2)).await;
	assert_eq!(status, StatusCode::CONFLICT, "one store per nym");
	// The service does not judge the account the wallet chose: another
	// store may register the same descriptor.
	let (b2, (status, _)) = common::register_nym(&svc, &bob, "bob").await;
	assert!(status.is_success());
	let (status, body) = common::register_store(&svc, &bob, &b2, "bob", common::store_descriptor(1)).await;
	assert!(status.is_success(), "{}", body);

	// Nothing can change a nym's name or destinations, or delete it.
	assert!(svc.sql("UPDATE nym SET ark_address = 'x' WHERE name = 'cafe'").await.is_err());
	assert!(svc.sql("UPDATE nym SET name = 'other' WHERE name = 'cafe'").await.is_err());
	assert!(svc.sql("UPDATE nym SET silent_payment_address = 'x' WHERE name = 'cafe'").await.is_err());
	assert!(svc.sql("DELETE FROM nym WHERE name = 'bob'").await.is_err());
	assert!(svc.sql("UPDATE store SET onchain_descriptor = 'x'").await.is_err());
	assert!(svc.sql("DELETE FROM store").await.is_err());
	assert_eq!(svc.sql("UPDATE nym SET display_name = 'Café' WHERE name = 'cafe'").await.unwrap(), 1);
}

#[tokio::test]
async fn unconfirmed_payment_warnings_and_safeguards() {
	let ctx = TestContext::new("bullnym/unconfirmed_payment_warnings_and_safeguards").await;
	// Every mempool payment is below this floor, so it gets the low-fee
	// warning. Sales expire quickly.
	let store = store_setup_with(&ctx, |cfg| {
		cfg.sales.low_fee_floor_sat_vb = 1000.0;
		cfg.sales.expiry_secs = 20;
	}).await;
	let sale = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	let id = sale["id"].as_str().unwrap();
	let address = sale["onchain_address"].as_str().unwrap().to_string();

	// The payment spends an unconfirmed output.
	let central = ctx.bitcoind();
	let own = rpc(central, Some("central"), "getnewaddress", json!([])).await;
	let parent = rpc(central, Some("central"), "sendtoaddress", json!([own, 0.01])).await;
	let parent_tx = rpc(central, Some("central"), "gettransaction", json!([parent, true, true])).await;
	let vout = parent_tx["decoded"]["vout"].as_array().unwrap().iter()
		.find(|o| o["scriptPubKey"]["address"] == own).unwrap()["n"].clone();
	rpc(central, Some("central"), "send", json!([
		[{ address.clone(): 0.0006 }], null, "unset", null,
		{ "inputs": [{ "txid": parent, "vout": vout }], "add_inputs": false },
	])).await;

	let seen = store.svc.wait_sale(id, SALE_TIMEOUT, |s| s["state"] == "in-progress"
		&& warnings(s).contains(&"unconfirmed-parents".to_string()), "in progress with warnings").await;
	let w = warnings(&seen);
	assert!(w.contains(&"not-guaranteed".to_string()) && w.contains(&"low-fee".to_string()), "{:?}", w);
	// The owner's history shows the same stage and warnings.
	let history = store.svc.ok(Method::GET, "/api/v1/owner/sales", Some(&store.owner.token), None).await;
	assert_eq!(history["sales"][0]["state"], "in-progress");
	assert_eq!(warnings(&history["sales"][0]), w);

	// No cancel, and the sale outlives its expiry.
	let (status, _) = store.svc.call(Method::POST, &format!("/api/v1/sales/{}/cancel", id), None, None).await;
	assert_eq!(status, StatusCode::CONFLICT);
	tokio::time::sleep(Duration::from_secs(25)).await;
	let later = store.svc.ok(Method::GET, &format!("/api/v1/sales/{}", id), None, None).await;
	assert_eq!(later["state"], "in-progress", "{}", later);
	assert_eq!(later["received_sat"], 0, "an unconfirmed payment never counts");

	ctx.generate_blocks(1).await;
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "onchain");
}

#[tokio::test]
async fn replaced_payment_returns_the_sale_to_open() {
	let ctx = TestContext::new("bullnym/replaced_payment_returns_the_sale_to_open").await;
	let store = store_setup(&ctx).await;
	let sale = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	let id = sale["id"].as_str().unwrap();
	let address = sale["onchain_address"].as_str().unwrap().to_string();

	let central = ctx.bitcoind();
	let txid = rpc(central, Some("central"), "sendtoaddress", json!([address, 0.0006])).await;
	store.svc.wait_sale_state(id, "in-progress", SALE_TIMEOUT).await;

	// The payer replaces it with a transaction that pays someone else.
	let tx = rpc(central, Some("central"), "gettransaction", json!([txid, true, true])).await;
	let inputs = tx["decoded"]["vin"].as_array().unwrap().iter()
		.map(|i| json!({ "txid": i["txid"], "vout": i["vout"] })).collect::<Vec<_>>();
	let elsewhere = rpc(central, Some("central"), "getnewaddress", json!([])).await;
	rpc(central, Some("central"), "send", json!([
		[{ elsewhere.as_str().unwrap(): 0.0001 }], null, "unset", 50,
		{ "inputs": inputs, "add_inputs": false },
	])).await;

	let back = store.svc.wait_sale_state(id, "open", SALE_TIMEOUT).await;
	assert_eq!(back["notice"], "payment-replaced", "{}", back);
	assert_eq!(back["received_sat"], 0);
	assert_eq!(back["seen_sat"], 0);
}

#[tokio::test]
async fn reorg_after_paid_is_a_payment_issue() {
	let ctx = TestContext::new("bullnym/reorg_after_paid_is_a_payment_issue").await;
	let store = store_setup(&ctx).await;
	let sale = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	let id = sale["id"].as_str().unwrap();
	ctx.bitcoind().fund_addr(sale["onchain_address"].as_str().unwrap(), sat(60_000)).await;
	store.svc.wait_sale_state(id, "in-progress", SALE_TIMEOUT).await;
	ctx.generate_blocks(1).await;
	store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;

	// The service's node drops the block with the payment.
	let node = &store.svc.bitcoind;
	let tip = rpc(node, None, "getbestblockhash", json!([])).await;
	rpc(node, None, "invalidateblock", json!([tip])).await;
	let issue = store.svc.wait_sale_state(id, "payment-issue", SALE_TIMEOUT).await;
	assert_eq!(issue["final"], false);

	// The block comes back: paid again.
	rpc(node, None, "reconsiderblock", json!([tip])).await;
	store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
}

#[tokio::test]
async fn late_payment_and_new_address_per_sale() {
	let ctx = TestContext::new("bullnym/late_payment_and_new_address_per_sale").await;
	let store = store_setup_with(&ctx, |cfg| {
		cfg.sales.expiry_secs = 5;
		cfg.sales.default_onchain_min_sat = 1000;
	}).await;
	let owner = Some(store.owner.token.as_str());
	assert_eq!(store.svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await["next_onchain_index"], 0);

	// Each sale that offers Bitcoin takes the next address, as in posd.
	let a = store.new_sale(json!({ "amount_sat": 2000 })).await;
	let b = store.new_sale(json!({ "amount_sat": 2000 })).await;
	let addr_a = a["onchain_address"].as_str().unwrap().to_string();
	let addr_b = b["onchain_address"].as_str().unwrap().to_string();
	assert_ne!(addr_a, addr_b);
	// Below the on-chain minimum, a sale takes no address.
	let small = store.new_sale(json!({ "amount_sat": 500 })).await;
	assert_eq!(small["onchain_address"], Value::Null);

	let b_id = b["id"].as_str().unwrap();
	store.svc.wait_sale_state(b_id, "expired", SALE_TIMEOUT).await;
	store.svc.wait_sale_state(a["id"].as_str().unwrap(), "expired", SALE_TIMEOUT).await;

	// A payment to an expired sale is a late payment, shown on that sale.
	ctx.bitcoind().fund_addr(&addr_b, sat(2000)).await;
	let late = store.svc.wait_sale(b_id, SALE_TIMEOUT, |s| s["late_payment"] == true, "late").await;
	assert_eq!(late["state"], "expired");
	ctx.generate_blocks(1).await;
	let late = store.svc.wait_sale(b_id, SALE_TIMEOUT, |s| s["received_sat"] == 2000, "counted").await;
	assert_eq!(late["state"], "expired", "a late payment never revives the sale");
	let history = store.svc.ok(Method::GET, "/api/v1/owner/sales", owner, None).await;
	let row = history["sales"].as_array().unwrap().iter().find(|s| s["id"] == b_id).unwrap().clone();
	assert_eq!(row["late_payment"], true, "the owner sees the late payment");

	// An address is never handed out again, even after its sale ended.
	let c = store.new_sale(json!({ "amount_sat": 2000 })).await;
	let addr_c = c["onchain_address"].as_str().unwrap();
	assert!(addr_c != addr_a && addr_c != addr_b);
	// The owner's store view gives the next index, for the wallet's scan.
	assert_eq!(store.svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await["next_onchain_index"], 3);
}

#[tokio::test]
async fn options_close_while_their_backend_is_down() {
	let ctx = TestContext::new("bullnym/options_close_while_their_backend_is_down").await;
	let store = store_setup(&ctx).await;
	let svc = &store.svc;

	store.forwarder.stop().await.unwrap();
	// The health probes close the invoice option after three failures.
	let deadline = tokio::time::Instant::now() + SALE_TIMEOUT;
	loop {
		let sale = store.new_sale(json!({ "amount_sat": 60_000 })).await;
		if sale["lightning_invoice"].is_null() {
			assert!(sale["onchain_address"].is_string(), "the on-chain option stays: {}", sale);
			break;
		}
		assert!(tokio::time::Instant::now() < deadline, "the invoice option never closed");
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
	// Below the on-chain minimum no option is left: one fixed message.
	let (status, body) = svc.call(Method::POST, "/api/v1/checkout/cafe/sales", None,
		Some(json!({ "amount_sat": 1000 }))).await;
	assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
	assert_eq!(body["error"], UNAVAILABLE_MESSAGE);
	let (status, body) = svc.call(Method::GET, "/api/v1/lnurlp/cafe/callback?amount=1000000", None, None).await;
	assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
	assert_eq!((body["status"].clone(), body["reason"].clone()), (json!("ERROR"), json!(UNAVAILABLE_MESSAGE)));
	// The Silent Payment address needs no backend.
	svc.ok(Method::GET, "/api/v1/lnurlp/cafe/callback?amount=1000000&payment_method=SP", None, None).await;
	// A restarted barkd of the testing crate listens on another port, so
	// reopening is covered by the unit test of the health states.
}

#[tokio::test]
async fn unknown_invoice_outcomes_stay_watched() {
	let ctx = TestContext::new("bullnym/unknown_invoice_outcomes_stay_watched").await;
	let store = store_setup(&ctx).await;
	let svc = &store.svc;

	// barkd stops answering, while a Lightning Address invoice and a sale
	// invoice, long past their expiry and grace, have no known outcome.
	store.forwarder.stop().await.unwrap();
	svc.sql(&format!("
		INSERT INTO lnurl_invoice (nym_id, payment_hash, invoice, amount_sat, expires_at)
		SELECT id, '{}', 'lnbcrt1unknown', 1000, NOW() - INTERVAL '1 day' FROM nym WHERE name = 'cafe'
	", "aa".repeat(32))).await.unwrap();
	svc.sql("
		INSERT INTO sale (public_id, store_id, amount_sat, state, expires_at, created_at)
		SELECT 'unknown-outcome', id, 1000, 'expired', NOW() - INTERVAL '1 day', NOW() - INTERVAL '1 day'
		FROM store
	").await.unwrap();
	svc.sql(&format!("
		INSERT INTO sale_payment (sale_id, rail, payment_hash, invoice)
		SELECT id, 'lightning', '{}', 'lnbcrt1unknown' FROM sale WHERE public_id = 'unknown-outcome'
	", "bb".repeat(32))).await.unwrap();

	// Many watcher passes fail to read barkd. Neither invoice is closed.
	tokio::time::sleep(Duration::from_secs(10)).await;
	let open = svc.sql(&format!(
		"UPDATE lnurl_invoice SET state = state WHERE state = 'open' AND payment_hash = '{}'", "aa".repeat(32),
	)).await.unwrap();
	assert_eq!(open, 1, "a read error must not end the Lightning Address invoice");
	let watched = bullnym::watcher::watched_payments(&svc.app).await.unwrap();
	assert!(watched.iter().any(|p| p.sale_public_id == "unknown-outcome"),
		"the sale invoice stays watched past its grace");
}

/// Sends `n` requests at once and counts the ones that succeed.
async fn parallel_successes(n: usize, request: impl Fn() -> reqwest::RequestBuilder) -> (usize, Vec<StatusCode>) {
	let mut set = tokio::task::JoinSet::new();
	for _ in 0..n {
		let req = request();
		set.spawn(async move { req.send().await.unwrap().status() });
	}
	let mut statuses = Vec::new();
	while let Some(status) = set.join_next().await {
		statuses.push(status.unwrap());
	}
	(statuses.iter().filter(|s| s.is_success()).count(), statuses)
}

#[tokio::test]
async fn caps_hold_under_parallel_requests() {
	let ctx = TestContext::new("bullnym/caps_hold_under_parallel_requests").await;
	let store = store_setup_with(&ctx, |cfg| {
		cfg.limits.max_open_invoices_per_nym = 3;
		cfg.limits.max_open_sales_per_store = 4;
	}).await;
	let http = reqwest::Client::new();

	// Twelve Lightning Address callbacks at once: exactly the cap of open
	// invoices of the nym gets one.
	let url = format!("{}/api/v1/lnurlp/cafe/callback?amount=1000000", store.svc.url);
	let (ok, statuses) = parallel_successes(12, || http.get(&url)).await;
	assert_eq!(ok, 3, "open invoices per nym: {:?}", statuses);
	assert!(statuses.iter().all(|s| s.is_success() || *s == StatusCode::TOO_MANY_REQUESTS), "{:?}", statuses);

	// Twelve sales at once: exactly the cap of open sales of the store.
	let url = format!("{}/api/v1/checkout/cafe/sales", store.svc.url);
	let (ok, statuses) = parallel_successes(12, || http.post(&url).json(&json!({ "amount_sat": 1000 }))).await;
	assert_eq!(ok, 4, "open sales per store: {:?}", statuses);
	let history = store.svc.ok(Method::GET, "/api/v1/owner/sales", Some(&store.owner.token), None).await;
	assert_eq!(history["sales"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn rate_limits_and_bypass() {
	let ctx = TestContext::new("bullnym/rate_limits_and_bypass").await;
	let (srv, _lightning, forwarder) = common::ark_server(&ctx).await;
	let token = "certification-token-for-tests";
	let svc = Service::start_with(&ctx, &srv, &forwarder, |cfg| {
		cfg.limits.lookups_per_minute = 3;
		cfg.limits.register_per_minute = 2;
		cfg.certification.enabled = true;
		cfg.certification.token = Some(token.into());
		cfg.certification.allowed_sources = vec!["127.0.0.1".into()];
	}).await;

	for _ in 0..3 {
		svc.ok(Method::GET, "/api/v1/nym/someone", None, None).await;
	}
	let (status, _) = svc.call(Method::GET, "/api/v1/nym/someone", None, None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
	// X-Forwarded-For is ignored without a trusted proxy.
	let mut forwarded = HeaderMap::new();
	forwarded.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.7"));
	let (status, _) = svc.call_with(Method::GET, "/api/v1/nym/someone", None, None, forwarded).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
	// The certification token skips the limits.
	let mut certified = HeaderMap::new();
	certified.insert("x-bullnym-certification-token", HeaderValue::from_static(token));
	let (status, _) = svc.call_with(Method::GET, "/api/v1/nym/someone", None, None, certified).await;
	assert_eq!(status, StatusCode::OK);

	// Registrations are limited before any signature check.
	let body = json!({ "registration": {
		"nym": "x", "ark_address": "x", "silent_payment_address": "x", "timestamp": 0,
	}, "signature": "00" });
	for _ in 0..2 {
		let (status, _) = svc.call(Method::POST, "/api/v1/nym", None, Some(body.clone())).await;
		assert_eq!(status, StatusCode::BAD_REQUEST);
	}
	let (status, _) = svc.call(Method::POST, "/api/v1/nym", None, Some(body)).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn startup_checks_and_the_hint_field() {
	let ctx = TestContext::new("bullnym/startup_checks_and_the_hint_field").await;
	let (srv, _lightning, forwarder) = common::ark_server(&ctx).await;

	// barkd's invoice-for-address with the field S1 adds. A barkd without
	// S1 ignores the unknown field and returns an invoice without the hint.
	let owner = ctx.bark_sdk("owner", &srv).create().await;
	let address = owner.new_address().await.unwrap();
	let token = std::fs::read_to_string(forwarder.datadir().join("auth_token")).unwrap();
	let resp = reqwest::Client::new()
		.post(format!("{}/api/v1/lightning/receives/invoice/for-address", forwarder.base_url()))
		.bearer_auth(token.trim())
		.json(&json!({
			"amount_sat": 10_000, "address": address.to_string(), "expiry_secs": 60,
			"ark_address_hint": true,
		}))
		.send().await.unwrap();
	assert_eq!(resp.status(), StatusCode::OK, "barkd refused the request with the hint field");
	let invoice = resp.json::<Value>().await.unwrap()["invoice"].as_str().unwrap().to_string();
	if common::s1_s4() {
		assert_eq!(invoice_hint(&invoice), Some(address.to_string()));
	} else {
		assert_eq!(invoice_hint(&invoice), None);
	}

	// Without the override, the service starts only with S1 and S4.
	let res = Service::try_start_with(&ctx, &srv, &forwarder, |cfg| {
		cfg.startup.allow_missing_s1_s4 = false;
	}).await;
	if common::s1_s4() {
		assert!(res.is_ok(), "{:#}", res.err().unwrap());
	} else {
		let err = format!("{:#}", res.err().expect("the service must refuse to start"));
		assert!(err.contains("(S1)") && err.contains("(S4)"), "{}", err);
	}
}

/// A manual demo, not run by default. It starts a full stack, prints the
/// store's checkout link and pays every sale created on it from a Scuttle
/// wallet. Run it with
/// `cargo nextest run --test integration --run-ignored only demo`
/// and open the link in a browser. DEMO_SECS sets how long it runs.
#[tokio::test]
#[ignore]
async fn demo() {
	let ctx = TestContext::new("bullnym/demo").await;
	let store = store_setup(&ctx).await;
	let payer = ctx.bark_sdk("payer", &store.srv).boarded(sat(2_000_000))
		.cfg(|cfg| cfg.lightning_ark_address_hints = true)
		.create().await;
	println!("DEMO_CHECKOUT_URL {}/{}", store.svc.url, store.nym);
	eprintln!("DEMO_CHECKOUT_URL {}/{}", store.svc.url, store.nym);

	let secs = std::env::var("DEMO_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(900);
	let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
	let mut paid = std::collections::HashSet::new();
	while tokio::time::Instant::now() < deadline {
		let sales = store.svc.ok(Method::GET, "/api/v1/owner/sales", Some(&store.owner.token), None).await;
		for sale in sales["sales"].as_array().unwrap() {
			let id = sale["id"].as_str().unwrap().to_string();
			if sale["state"] == "open" && paid.insert(id.clone()) {
				// Give the screen time to show the QR code.
				tokio::time::sleep(Duration::from_secs(4)).await;
				let kind = pay(&payer, sale["payment_request"].as_str().unwrap()).await;
				eprintln!("DEMO paid sale {} by {}", id, kind);
			}
		}
		tokio::time::sleep(Duration::from_millis(500)).await;
	}
}
