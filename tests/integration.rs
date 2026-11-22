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

use std::collections::HashMap;
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
use bullnym::TestOverrides;
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
		"fiat_amount": "12500", "memo": "2 coffees",
	})).await;
	assert_eq!(sale["state"], "open");
	assert_eq!(sale["amount_sat"], 20_000);
	assert_eq!(sale["fiat_amount"], "12500.00");
	// The sale has its Lightning QR only: no Bitcoin unless the merchant
	// asks, and no QR with both.
	assert_eq!((sale["onchain_address"].clone(), sale["onchain_request"].clone()), (Value::Null, Value::Null));
	assert!(sale.get("payment_request").is_none());
	let invoice = sale["lightning_invoice"].as_str().unwrap();
	assert_eq!(sale["lightning_request"], format!("lightning:{}", invoice));
	// The one invoice names the owner's registered address as its Ark hint
	// (S1). Without S1, barkd leaves the hint out.
	if common::s1_s4() {
		assert_eq!(invoice_hint(invoice), Some(store.owner.address.to_string()));
	} else {
		assert_eq!(invoice_hint(invoice), None);
	}

	store.lightning.external.pay_bolt11(invoice).await;
	let id = sale["id"].as_str().unwrap();
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "lightning");
	assert!(paid.get("final").is_none(), "paid is final: no separate finality");

	owner_restores(&ctx, &store, sat(15_000)).await;

	// The cashier link cannot list sales or export them.
	let (status, _) = store.svc.call(Method::GET, "/api/v1/sales", None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = store.svc.call(Method::GET, "/api/v1/owner/sales.csv", None, None).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);
	let csv = owner_csv(&store).await;
	assert!(csv.contains(id) && csv.contains("2 coffees") && csv.contains(",paid,lightning,"), "{}", csv);
	assert!(!csv.contains("cashier"), "no cashier name: {}", csv);
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
	// The Scuttle wallet pays the Lightning QR text over Ark.
	let qr = sale["lightning_request"].as_str().unwrap();
	assert!(qr.starts_with("lightning:"), "{}", qr);
	let kind = pay(&payer, qr).await;
	// Paid once barkd settles the receive (S4).
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "ark", "payer chose {}", kind);
	assert_eq!(paid["received_sat"], 30_000);

	// The invoice is settled: the sale cannot be paid twice.
	let again = payer.parse_payment_request(qr).await.unwrap();
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
	pay(&payer, sale["lightning_request"].as_str().unwrap()).await;
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "lightning");
}

#[tokio::test]
async fn sale_paid_onchain_through_its_stages() {
	let ctx = TestContext::new("bullnym/sale_paid_onchain_through_its_stages").await;
	let store = store_setup(&ctx).await;

	// At or above the 50,000-sat minimum, the merchant can add Bitcoin. The
	// sale has none until then.
	let sale = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	let id = sale["id"].as_str().unwrap();
	assert_eq!(sale["onchain_address"], Value::Null);
	let with_btc = store.add_onchain(&sale).await;
	let address = with_btc["onchain_address"].as_str().expect("an on-chain address").to_string();
	// Two QRs, never one with both.
	assert_eq!(with_btc["onchain_request"], format!("bitcoin:{}?amount=0.0006", address));
	assert_eq!(with_btc["lightning_request"], sale["lightning_request"], "the invoice stays");
	// Adding it again gives the same address: a sale takes one index.
	assert_eq!(store.add_onchain(&sale).await["onchain_address"], address);
	let owner = Some(store.owner.token.as_str());
	assert_eq!(store.svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await["next_onchain_index"], 1);
	let second = store.add_onchain(&store.new_sale(json!({ "amount_sat": 60_000 })).await).await;
	assert_ne!(second["onchain_address"].as_str().unwrap(), address, "each sale has its own address");

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

	// Paid at 1 confirmation, for good, on the cashier's view and the
	// owner's.
	ctx.generate_blocks(1).await;
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	assert_eq!(paid["paid_rail"], "onchain");
	assert_eq!(paid["received_sat"], 60_000);
	assert!(paid.get("final").is_none() && paid.get("final_at").is_none(), "{}", paid);
	assert!(warnings(&paid).is_empty(), "{}", paid);
	let history = store.svc.ok(Method::GET, "/api/v1/owner/sales", Some(&store.owner.token), None).await;
	let row = history["sales"].as_array().unwrap().iter().find(|s| s["id"] == id).unwrap().clone();
	assert_eq!((row["state"].clone(), row["received_sat"].clone()), (json!("paid"), json!(60_000)));

	// A paid sale stays paid: the block with the payment going away does
	// not change its state.
	let node = &store.svc.bitcoind;
	let tip = rpc(node, None, "getbestblockhash", json!([])).await;
	rpc(node, None, "invalidateblock", json!([tip])).await;
	tokio::time::sleep(Duration::from_secs(6)).await;
	let after = store.svc.ok(Method::GET, &format!("/api/v1/sales/{}", id), None, None).await;
	assert_eq!(after["state"], "paid", "{}", after);
	rpc(node, None, "reconsiderblock", json!([tip])).await;
	store.svc.wait_sale(id, SALE_TIMEOUT, |s| s["received_sat"] == 60_000, "counted again").await;

	// As in posd, the address stays watched for 7 days: an extra payment
	// counts on this sale, as an overpayment, and the sale stays paid.
	ctx.bitcoind().fund_addr(&address, sat(10_000)).await;
	ctx.generate_blocks(1).await;
	let over = store.svc.wait_sale(id, SALE_TIMEOUT, |s| s["received_sat"] == 70_000, "overpaid").await;
	assert_eq!((over["state"].clone(), over["paid_rail"].clone()), (json!("paid"), json!("onchain")));
	let history = store.svc.ok(Method::GET, "/api/v1/owner/sales", Some(&store.owner.token), None).await;
	let row = history["sales"].as_array().unwrap().iter().find(|s| s["id"] == id).unwrap().clone();
	assert_eq!(row["received_sat"], 70_000);

	let csv = owner_csv(&store).await;
	assert!(csv.contains(id) && csv.contains(",paid,onchain,60000,70000,"), "{}", csv);
	assert!(!csv.contains("final"), "{}", csv);
}

#[tokio::test]
async fn lightning_address_and_lud22() {
	let ctx = TestContext::new("bullnym/lightning_address_and_lud22").await;
	let store = store_setup(&ctx).await;
	let svc = &store.svc;

	let pay_request = svc.ok(Method::GET, "/.well-known/lnurlp/cafe", None, None).await;
	assert_eq!(pay_request["tag"], "payRequest");
	assert_eq!(pay_request["payment_methods"], json!(["SCUTTLE", "SP"]));
	assert!(pay_request["metadata"].as_str().unwrap().contains("cafe@127.0.0.1"));
	let callback = pay_request["callback"].as_str().unwrap();
	let path = callback.strip_prefix(&svc.url).unwrap().to_string();

	// The default callback: a forwarding invoice without a hint, to the
	// registered address. Any Lightning wallet pays it.
	let reply = svc.ok(Method::GET, &format!("{}?amount=21000000", path), None, None).await;
	let invoice = reply["pr"].as_str().unwrap();
	assert_eq!(invoice_hint(invoice), None, "the Lightning Address invoice has no hint");
	store.lightning.external.pay_bolt11(invoice).await;
	// The service records the payment, which reaches the owner through
	// barkd. There is no API listing them.
	let deadline = tokio::time::Instant::now() + SALE_TIMEOUT;
	loop {
		let (state, amount) = lnurl_invoices(svc).await.into_iter().next().expect("an invoice");
		assert_eq!(amount, 21_000);
		if state == "paid" {
			break;
		}
		assert!(tokio::time::Instant::now() < deadline, "Lightning Address payment not recorded: {}", state);
		tokio::time::sleep(Duration::from_millis(200)).await;
	}
	owner_restores(&ctx, &store, sat(15_000)).await;
	let (status, _) = svc.call(Method::GET, "/api/v1/owner/nym/payments", Some(&store.owner.token), None).await;
	assert_eq!(status, StatusCode::NOT_FOUND, "no payments list");

	// LUD-22: each method answers with only the requested methods' static
	// addresses, and creates no invoice.
	let open_before = lnurl_invoices(svc).await.len();
	let ark = json!({ "address": store.owner.address.to_string() });
	let sp = json!({ "address": common::silent_payment_address() });
	let with = |m: &str| format!("{}?amount=5000000&payment_method={}", path, m);
	assert_eq!(svc.ok(Method::GET, &with("SCUTTLE"), None, None).await, json!({ "SCUTTLE": ark.clone() }));
	assert_eq!(svc.ok(Method::GET, &with("SP"), None, None).await, json!({ "SP": sp.clone() }));
	assert_eq!(svc.ok(Method::GET, &with("SCUTTLE,SP"), None, None).await,
		json!({ "SCUTTLE": ark.clone(), "SP": sp.clone() }));
	assert_eq!(svc.ok(Method::GET, &with("SP,SCUTTLE"), None, None).await, json!({ "SCUTTLE": ark, "SP": sp }));
	assert_eq!(open_before, lnurl_invoices(svc).await.len(), "a LUD-22 method creates no invoice");

	// An unknown method is refused in the LNURL error shape. ARK is not an
	// alias of SCUTTLE.
	for unknown in ["ARK", "L-BTC", "SCUTTLE,L-BTC"] {
		let (status, err) = svc.call(Method::GET,
			&format!("{}?amount=5000000&payment_method={}", path, unknown), None, None).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{}", unknown);
		assert_eq!(err["status"], "ERROR");
		assert!(err["reason"].as_str().unwrap().starts_with("unsupported payment method"), "{}", err);
	}
	let (status, _) = svc.call(Method::GET, &format!("{}?amount=1500", path), None, None).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "not whole sats");
	let (status, _) = svc.call(Method::GET, "/.well-known/lnurlp/nobody", None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	// There is no public nym lookup.
	let (status, _) = svc.call(Method::GET, "/api/v1/nym/cafe", None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The Lightning Address invoices the service recorded, newest first: their
/// state and amount.
async fn lnurl_invoices(svc: &Service) -> Vec<(String, i64)> {
	let conn = svc.app.public.db.conn().await.unwrap();
	conn.query("
		SELECT state, amount_sat FROM lnurl_invoice WHERE payment_hash IS NOT NULL ORDER BY id DESC
	", &[]).await.unwrap().iter().map(|r| (r.get(0), r.get(1))).collect()
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

	// The link creates, follows and cancels its sale, without a token. A
	// sale takes no address unless Bitcoin is added to it.
	let sale = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	let id = sale["id"].as_str().unwrap();
	assert!(sale.get("cashier_name").is_none(), "sales carry no cashier name: {}", sale);
	assert_eq!(svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await["next_onchain_index"], 0);
	svc.ok(Method::GET, &format!("/api/v1/sales/{}", id), None, None).await;
	let onchain = |id: &str| format!("/api/v1/sales/{}/onchain", id);
	let canceled = svc.ok(Method::POST, &format!("/api/v1/sales/{}/cancel", id), None, None).await;
	assert_eq!(canceled["state"], "canceled");
	let (status, _) = svc.call(Method::POST, &format!("/api/v1/sales/{}/cancel", id), None, None).await;
	assert_eq!(status, StatusCode::CONFLICT);
	let (status, _) = svc.call(Method::POST, &onchain(id), None, None).await;
	assert_eq!(status, StatusCode::CONFLICT, "Bitcoin is added to an open sale only");
	let (status, _) = svc.call(Method::GET, "/api/v1/sales/0123456789abcdef0123456789abcdef", None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (status, _) = svc.call(Method::POST, &onchain("0123456789abcdef0123456789abcdef"), None, None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	// Below the store's on-chain minimum, Bitcoin is refused.
	let small = store.new_sale(json!({ "amount_sat": 1000 })).await;
	let (status, body) = svc.call(Method::POST, &onchain(small["id"].as_str().unwrap()), None, None).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
	assert_eq!(svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await["next_onchain_index"], 0);

	// Amounts past 21,000,000 BTC, or a fiat amount of 10^15 or more, are
	// refused before a sale, an invoice or an address is allocated.
	let before = svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await["next_onchain_index"].clone();
	for body in [
		json!({ "amount_sat": 2_100_000_000_000_001u64 }),
		json!({ "amount_sat": u64::MAX }),
		json!({ "fiat_amount": "1000000000000000" }),
	] {
		let (status, err) = svc.call(Method::POST, "/api/v1/checkout/cafe/sales", None, Some(body.clone())).await;
		assert_eq!((status, err["error"].clone()), (StatusCode::BAD_REQUEST, json!("the amount is too large")), "{}", body);
	}
	assert_eq!(svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await["next_onchain_index"], before);
	let (status, _) = svc.call(Method::PATCH, "/api/v1/owner/store", owner,
		Some(json!({ "onchain_min_sat": 2_100_000_000_000_001u64 }))).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);

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
async fn cashier_pin() {
	let ctx = TestContext::new("bullnym/cashier_pin").await;
	let store = store_setup(&ctx).await;
	let svc = &store.svc;
	let owner = Some(store.owner.token.as_str());
	let check = |pin: &str| svc.call(Method::POST, "/api/v1/checkout/cafe/pin", None, Some(json!({ "pin": pin })));

	// Before the owner sets a PIN, the list stays hidden: no PIN unlocks it.
	let view = svc.ok(Method::GET, "/api/v1/checkout/cafe", None, None).await;
	assert_eq!(view["store"]["cashier_pin_set"], false);
	let (status, _) = check("0000").await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	// Selling needs no PIN.
	store.new_sale(json!({ "amount_sat": 1000 })).await;

	// Only the owner sets it, 4 to 12 digits.
	let (status, _) = svc.call(Method::PUT, "/api/v1/owner/store/cashier-pin", None,
		Some(json!({ "pin": "2468" }))).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);
	for bad in ["123", "12a4", "1234567890123"] {
		let (status, _) = svc.call(Method::PUT, "/api/v1/owner/store/cashier-pin", owner,
			Some(json!({ "pin": bad }))).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "{}", bad);
	}
	let set = svc.ok(Method::PUT, "/api/v1/owner/store/cashier-pin", owner, Some(json!({ "pin": "1357" }))).await;
	assert_eq!(set["cashier_pin_set"], true);
	// The owner changes it.
	let set = svc.ok(Method::PUT, "/api/v1/owner/store/cashier-pin", owner, Some(json!({ "pin": "2468" }))).await;
	assert_eq!(set["cashier_pin_set"], true);
	assert!(!set.to_string().contains("argon2"), "the hash is never shown: {}", set);
	let view = svc.ok(Method::GET, "/api/v1/checkout/cafe", None, None).await;
	assert_eq!(view["store"]["cashier_pin_set"], true);
	assert!(!view.to_string().contains("argon2"));
	// Only a salted slow hash is kept.
	let conn = svc.app.public.db.conn().await.unwrap();
	let hash: String = conn.query_one("SELECT cashier_pin_hash FROM store", &[]).await.unwrap().get(0);
	drop(conn);
	assert!(hash.starts_with("$argon2id$") && !hash.contains("2468"), "{}", hash);

	// The service checks it and says only whether it matched.
	let (status, body) = check("2468").await;
	assert_eq!((status, body.clone()), (StatusCode::OK, json!({ "ok": true })));
	let (status, body) = check("1357").await;
	assert_eq!(status, StatusCode::UNAUTHORIZED, "the old PIN no longer unlocks: {}", body);
	assert_eq!(body, json!({ "error": "wrong PIN", "retry_after": 0 }));

	// Wrong PINs back off: 5 are free, then each locks further checks.
	for _ in 0..4 {
		let (status, body) = check("0000").await;
		assert_eq!((status, body["retry_after"].clone()), (StatusCode::UNAUTHORIZED, json!(0)));
	}
	let (status, body) = check("0000").await;
	assert_eq!((status, body["retry_after"].clone()), (StatusCode::UNAUTHORIZED, json!(2)), "{}", body);
	// While locked, even the right PIN is not checked.
	let (status, body) = check("2468").await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{}", body);
	assert!(body["retry_after"].as_i64().unwrap() > 0);
	tokio::time::sleep(Duration::from_secs(3)).await;
	let (status, _) = check("2468").await;
	assert_eq!(status, StatusCode::OK);
	let (status, body) = check("0000").await;
	assert_eq!((status, body["retry_after"].clone()), (StatusCode::UNAUTHORIZED, json!(4)), "{}", body);
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
	// posd's mailbox id is signed and must be one.
	let mut reg = bullnym::registration::StoreRegistration {
		name: "Café Uvita".into(), ark_address: a1.to_string(), mailbox_id: "02zz".into(),
		onchain_descriptor: common::store_descriptor(1), currency: "CRC".into(),
		timestamp: chrono::Utc::now().timestamp() as u64, nym: "cafe".into(),
	};
	let sig = common::sign(&alice, &reg.message(), &a1).await;
	let (status, body) = svc.call(Method::POST, "/api/v1/stores", None,
		Some(json!({ "registration": reg.clone(), "signature": sig }))).await;
	assert_eq!((status, body["error"].clone()), (StatusCode::BAD_REQUEST, json!("not a valid mailbox id")));
	reg.mailbox_id = alice.mailbox_identifier().to_string();
	let (status, _) = svc.call(Method::POST, "/api/v1/stores", None,
		Some(json!({ "registration": reg, "signature": sig }))).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "the mailbox id is part of the signed message");
	let (status, body) = common::register_store(&svc, &alice, &a1, "cafe", common::store_descriptor(1)).await;
	assert!(status.is_success(), "{}", body);
	assert!(body["store"]["lightning_address"].as_str().unwrap().starts_with("cafe@"));
	// The service keeps the mailbox id and shows it nowhere.
	let conn = svc.app.public.db.conn().await.unwrap();
	let kept: String = conn.query_one("SELECT mailbox_id FROM store", &[]).await.unwrap().get(0);
	drop(conn);
	assert_eq!(kept, alice.mailbox_identifier().to_string());
	assert!(!body.to_string().contains(&kept));
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
	assert!(svc.sql("UPDATE store SET mailbox_id = 'x'").await.is_err());
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
	let sale = store.add_onchain(&store.new_sale(json!({ "amount_sat": 60_000 })).await).await;
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
	let sale = store.add_onchain(&store.new_sale(json!({ "amount_sat": 60_000 })).await).await;
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
async fn late_payment_and_new_address_per_sale() {
	let ctx = TestContext::new("bullnym/late_payment_and_new_address_per_sale").await;
	let store = store_setup_with(&ctx, |cfg| {
		cfg.sales.expiry_secs = 5;
		cfg.sales.default_onchain_min_sat = 1000;
	}).await;
	let owner = Some(store.owner.token.as_str());
	assert_eq!(store.svc.ok(Method::GET, "/api/v1/owner/store", owner, None).await["next_onchain_index"], 0);

	// Each sale Bitcoin is added to takes the next address, as in posd.
	let a = store.add_onchain(&store.new_sale(json!({ "amount_sat": 2000 })).await).await;
	let b = store.add_onchain(&store.new_sale(json!({ "amount_sat": 2000 })).await).await;
	let addr_a = a["onchain_address"].as_str().unwrap().to_string();
	let addr_b = b["onchain_address"].as_str().unwrap().to_string();
	assert_ne!(addr_a, addr_b);
	// A sale without Bitcoin takes no address.
	let plain = store.new_sale(json!({ "amount_sat": 2000 })).await;
	assert_eq!(plain["onchain_address"], Value::Null);

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
	let c = store.add_onchain(&store.new_sale(json!({ "amount_sat": 2000 })).await).await;
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

	let open = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	store.forwarder.stop().await.unwrap();
	// The health probes close the invoice option after three failures.
	// A sale needs its invoice, so new sales are refused with one fixed
	// message.
	let deadline = tokio::time::Instant::now() + SALE_TIMEOUT;
	loop {
		let (status, body) = svc.call(Method::POST, "/api/v1/checkout/cafe/sales", None,
			Some(json!({ "amount_sat": 60_000 }))).await;
		if status == StatusCode::SERVICE_UNAVAILABLE {
			assert_eq!(body["error"], UNAVAILABLE_MESSAGE);
			break;
		}
		assert!(status.is_success(), "{} {}", status, body);
		assert!(tokio::time::Instant::now() < deadline, "the invoice option never closed");
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
	// The on-chain option has its own backend: Bitcoin can still be added
	// to an open sale.
	let added = store.add_onchain(&open).await;
	assert!(added["onchain_address"].is_string(), "{}", added);
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

	// Name lookups: the store behind a link, here one that does not exist.
	let lookup = "/api/v1/checkout/someone";
	for _ in 0..3 {
		let (status, _) = svc.call(Method::GET, lookup, None, None).await;
		assert_eq!(status, StatusCode::NOT_FOUND);
	}
	let (status, _) = svc.call(Method::GET, lookup, None, None).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
	// X-Forwarded-For is ignored without a trusted proxy.
	let mut forwarded = HeaderMap::new();
	forwarded.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.7"));
	let (status, _) = svc.call_with(Method::GET, lookup, None, None, forwarded).await;
	assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
	// The certification token skips the limits.
	let mut certified = HeaderMap::new();
	certified.insert("x-bullnym-certification-token", HeaderValue::from_static(token));
	let (status, _) = svc.call_with(Method::GET, lookup, None, None, certified).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

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
async fn startup_and_sale_hint_checks() {
	let ctx = TestContext::new("bullnym/startup_and_sale_hint_checks").await;
	let (srv, _lightning, forwarder) = common::ark_server(&ctx).await;

	// posd's startup check: a server that is not in expiry payout mode is
	// refused.
	let plain = ctx.captaind("plain").cfg(|cfg| cfg.expiry_payouts = false).create().await;
	let err = Service::try_start_with(&ctx, &plain, &forwarder, |_| {}).await
		.err().expect("the service must refuse a server without expiry payouts");
	let err = format!("{:#}", err);
	assert!(err.contains("does not run in expiry payout mode"), "{}", err);

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

	// In payout mode the service starts: nothing else is checked at
	// startup, and no override is set.
	let svc = Service::try_start_overridden(&ctx, &srv, &forwarder, |_| {}, TestOverrides::default()).await
		.map_err(|e| format!("{:#}", e)).expect("the service starts in payout mode");
	let (address, (status, body)) = common::register_nym(&svc, &owner, "cafe").await;
	assert!(status.is_success(), "{}", body);
	let (status, body) = common::register_store(&svc, &owner, &address, "cafe", common::store_descriptor(3)).await;
	assert!(status.is_success(), "{}", body);
	let token = common::login(&svc, &owner, &address).await;

	// Each sale checks its invoice, as posd did: without the store's Ark
	// hint the sale is refused with a message saying why, and nothing is
	// left of it.
	let (status, body) = svc.call(Method::POST, "/api/v1/checkout/cafe/sales", None,
		Some(json!({ "amount_sat": 60_000 }))).await;
	if common::s1_s4() {
		assert_eq!(status, StatusCode::OK, "{}", body);
		assert_eq!(invoice_hint(body["lightning_invoice"].as_str().unwrap()), Some(address.to_string()));
	} else {
		assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{}", body);
		let error = body["error"].as_str().unwrap();
		assert!(error.contains("the Ark server must allow Ark address hints"), "{}", error);
		let history = svc.ok(Method::GET, "/api/v1/owner/sales", Some(&token), None).await;
		assert!(history["sales"].as_array().unwrap().is_empty(), "{}", history);
		let view = svc.ok(Method::GET, "/api/v1/owner/store", Some(&token), None).await;
		assert_eq!(view["next_onchain_index"], 0, "the refused sale took no address");
	}
}

#[tokio::test]
async fn stores_sharing_a_descriptor_never_share_an_address() {
	let ctx = TestContext::new("bullnym/stores_sharing_a_descriptor_never_share_an_address").await;
	let (srv, _lightning, forwarder) = common::ark_server(&ctx).await;
	let svc = Service::start_with(&ctx, &srv, &forwarder, |cfg| cfg.sales.default_onchain_min_sat = 1000).await;
	// Two stores register the same descriptor, which the service accepts.
	let descriptor = common::store_descriptor(7);
	let mut tokens = HashMap::new();
	for nym in ["alpha", "beta"] {
		let wallet = ctx.bark_sdk(nym, &srv).create().await;
		let (address, (status, body)) = common::register_nym(&svc, &wallet, nym).await;
		assert!(status.is_success(), "{}", body);
		let (status, body) = common::register_store(&svc, &wallet, &address, nym, descriptor.clone()).await;
		assert!(status.is_success(), "{}", body);
		tokens.insert(nym, common::login(&svc, &wallet, &address).await);
	}

	// Ordinary checkouts at both stores, all at once, then Bitcoin added to
	// every sale, all at once.
	let http = reqwest::Client::new();
	let mut set = tokio::task::JoinSet::new();
	for i in 0..12 {
		let nym = if i % 2 == 0 { "alpha" } else { "beta" };
		let req = http.post(format!("{}/api/v1/checkout/{}/sales", svc.url, nym)).json(&json!({ "amount_sat": 2000 }));
		set.spawn(async move {
			let resp = req.send().await.unwrap();
			assert!(resp.status().is_success(), "{}", resp.status());
			(nym, resp.json::<Value>().await.unwrap())
		});
	}
	let mut created = Vec::new();
	while let Some(r) = set.join_next().await {
		created.push(r.unwrap());
	}
	let mut set = tokio::task::JoinSet::new();
	for (nym, sale) in created {
		let req = http.post(format!("{}/api/v1/sales/{}/onchain", svc.url, sale["id"].as_str().unwrap()));
		set.spawn(async move {
			let resp = req.send().await.unwrap();
			assert!(resp.status().is_success(), "{}", resp.status());
			(nym, resp.json::<Value>().await.unwrap())
		});
	}
	let mut sales = Vec::new();
	while let Some(r) = set.join_next().await {
		sales.push(r.unwrap());
	}

	// Every sale has its own address: the first 12 of the shared account,
	// each handed out once.
	let desc = bullnym::rails::onchain::StoreDescriptor::parse(&descriptor).unwrap();
	let index_of = (0..12u32).map(|i| (desc.address(i, bitcoin::Network::Regtest).unwrap().to_string(), i))
		.collect::<HashMap<_, _>>();
	let mut issued = HashMap::new();
	for (nym, sale) in &sales {
		let address = sale["onchain_address"].as_str().expect("every sale offers an address").to_string();
		let index = *index_of.get(&address).expect("an address of the first 12");
		assert!(issued.insert(index, *nym).is_none(), "address {} handed out twice", index);
	}
	assert_eq!(issued.len(), 12);
	// Each store's index moved past every address it handed out, and past
	// the ones it skipped because the other store had them.
	for nym in ["alpha", "beta"] {
		let view = svc.ok(Method::GET, "/api/v1/owner/store", Some(&tokens[nym]), None).await;
		let next = view["next_onchain_index"].as_u64().unwrap() as u32;
		let mine = issued.iter().filter(|(_, n)| **n == nym).map(|(i, _)| *i).collect::<Vec<_>>();
		assert_eq!(mine.len(), 6, "{} sales at {}", mine.len(), nym);
		assert!(mine.iter().all(|i| *i < next) && next <= 12, "{} next index {} for {:?}", nym, next, mine);
	}

	// A payment to one address counts for its sale only.
	let (nym, target) = &sales[0];
	let target_id = target["id"].as_str().unwrap();
	ctx.bitcoind().fund_addr(target["onchain_address"].as_str().unwrap(), sat(2000)).await;
	let seen = svc.wait_sale_state(target_id, "in-progress", SALE_TIMEOUT).await;
	assert_eq!(seen["seen_sat"], 2000);
	tokio::time::sleep(Duration::from_secs(3)).await;
	for (_, sale) in &sales[1..] {
		let other = svc.ok(Method::GET, &format!("/api/v1/sales/{}", sale["id"].as_str().unwrap()), None, None).await;
		assert_eq!((other["seen_sat"].clone(), other["state"].clone()), (json!(0), json!("open")), "{}", other);
	}
	let history = svc.ok(Method::GET, "/api/v1/owner/sales", Some(&tokens[nym]), None).await;
	let with_payment = history["sales"].as_array().unwrap().iter().filter(|s| s["seen_sat"] != 0).count();
	assert_eq!(with_payment, 1);
}

/// The two QR codes of a sale, Lightning and Bitcoin, and its invoice
/// alone, as a payer copies it, read by the Scuttle wallet library. The test
/// prints what the payer did with each (`LIGHTNING_QR_OUTCOME`,
/// `BITCOIN_QR_OUTCOME`, `COPIED_INVOICE_OUTCOME`).
#[tokio::test]
async fn qr_codes_and_copied_invoice() {
	let ctx = TestContext::new("bullnym/qr_codes_and_copied_invoice").await;
	let store = store_setup(&ctx).await;
	let payer = ctx.bark_sdk("payer", &store.srv).boarded(sat(300_000))
		.cfg(|cfg| cfg.lightning_ark_address_hints = true)
		.create().await;
	let rail = if common::s1_s4() { "ark" } else { "lightning" };

	// The Lightning QR: `lightning:<invoice>`. With S1 the Scuttle wallet
	// pays it over Ark.
	let sale = store.new_sale(json!({ "amount_sat": 60_000 })).await;
	let id = sale["id"].as_str().unwrap();
	let qr = sale["lightning_request"].as_str().unwrap();
	assert!(qr.starts_with("lightning:") && !qr.contains("bitcoin:"), "{}", qr);
	let kind = pay(&payer, qr).await;
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	eprintln!("LIGHTNING_QR_OUTCOME the payer chose {} and the sale was paid by {}", kind, paid["paid_rail"]);
	assert_eq!(paid["paid_rail"], rail);

	// The Bitcoin QR, once Bitcoin is added: `bitcoin:<address>?amount=`.
	let sale = store.add_onchain(&store.new_sale(json!({ "amount_sat": 60_000 })).await).await;
	let id = sale["id"].as_str().unwrap();
	let qr = sale["onchain_request"].as_str().unwrap();
	assert!(qr.starts_with("bitcoin:bcrt1") && qr.ends_with("?amount=0.0006"), "{}", qr);
	match payer.parse_payment_request(qr).await {
		Ok(req) => {
			let kinds = req.options.iter().map(|o| o.method.type_str().to_string()).collect::<Vec<_>>();
			eprintln!("BITCOIN_QR_OUTCOME parsed, with the options {:?}", kinds);
		},
		Err(e) => {
			let e = format!("{:#}", e);
			eprintln!("BITCOIN_QR_OUTCOME the payer refused the QR code: {}", e);
			assert!(e.contains("No valid payment option found"), "{}", e);
		},
	}

	// The invoice alone, copied from that sale, which still has it.
	let kind = pay(&payer, sale["lightning_invoice"].as_str().unwrap()).await;
	let paid = store.svc.wait_sale_state(id, "paid", SALE_TIMEOUT).await;
	eprintln!("COPIED_INVOICE_OUTCOME the payer chose {} and the sale was paid by {}", kind, paid["paid_rail"]);
	assert_eq!(paid["paid_rail"], rail);
}

/// A manual demo, not run by default. It starts a full stack, prints the
/// store's checkout link and the owner's token, and pays every sale created
/// on it from a Scuttle wallet, over its Lightning QR. Run it with
/// `cargo nextest run --test integration --run-ignored only demo`
/// and open the link in a browser. DEMO_SECS sets how long it runs, and
/// DEMO_PAY_DELAY_SECS how long a sale waits before it is paid (4 s).
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
	// The owner's token on this throwaway stack, to drive the owner API.
	eprintln!("DEMO_OWNER_TOKEN {}", store.owner.token);

	let secs = std::env::var("DEMO_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(900);
	let delay = std::env::var("DEMO_PAY_DELAY_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
	let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
	let mut paid = std::collections::HashSet::new();
	while tokio::time::Instant::now() < deadline {
		let sales = store.svc.ok(Method::GET, "/api/v1/owner/sales", Some(&store.owner.token), None).await;
		for sale in sales["sales"].as_array().unwrap() {
			let id = sale["id"].as_str().unwrap().to_string();
			if sale["state"] == "open" && paid.insert(id.clone()) {
				// Give the screen time to show the QR code.
				tokio::time::sleep(Duration::from_secs(delay)).await;
				let kind = pay(&payer, sale["lightning_request"].as_str().unwrap()).await;
				eprintln!("DEMO paid sale {} by {}", id, kind);
			}
		}
		tokio::time::sleep(Duration::from_millis(500)).await;
	}
}
