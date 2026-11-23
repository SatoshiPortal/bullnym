use std::process::{Command, Output};

use ark_testing::TestContext;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use url::Url;

use crate::common::{Service, ark_server, signed_nym_request};

fn cli(svc: &Service) -> Command {
	let db = &svc.app.cfg.postgres;
	let mut url = Url::parse("postgresql://localhost").unwrap();
	url.set_host(Some(&db.host)).unwrap();
	url.set_port(Some(db.port)).unwrap();
	url.set_username(db.user.as_deref().unwrap_or("")).unwrap();
	url.set_password(db.password.as_deref()).unwrap();
	url.set_path(&db.name);
	let mut command = Command::new(env!("CARGO_BIN_EXE_bullnym"));
	command.arg("codes").env("DATABASE_URL", url.as_str());
	command
}

fn command(svc: &Service, args: &[&str]) -> Output {
	cli(svc).args(args).output().unwrap()
}

fn grants(svc: &Service, args: &[&str]) -> Vec<Value> {
	let output = command(svc, args);
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	String::from_utf8(output.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

async fn submit(svc: &Service, signed: &Value, code: Option<&str>) -> (StatusCode, Value) {
	let mut body = signed.clone();
	if let Some(code) = code { body["claim_code"] = json!(code); }
	svc.call(Method::POST, "/api/v1/nym", None, Some(body)).await
}

fn denied(response: (StatusCode, Value), code: &str) {
	assert_eq!(response.0, StatusCode::FORBIDDEN);
	assert_eq!(response.1["code"], code);
}

/// Wait until the CLI and then the HTTP writer are queued in this test's database.
async fn wait_for_claim_writers(svc: &Service, count: i64) {
	let conn = svc.app.public.db.conn().await.unwrap();
	tokio::time::timeout(std::time::Duration::from_secs(15), async {
		loop {
			let waiting: i64 = conn.query_one("SELECT count(*) FROM pg_stat_activity
				WHERE datname = current_database() AND wait_event_type = 'Lock'
					AND (query LIKE '%UPDATE claim_code SET%' OR query LIKE '%bullnym:nym-cap%')",
				&[]).await.unwrap().get(0);
			if waiting >= count { return; }
			tokio::time::sleep(std::time::Duration::from_millis(20)).await;
		}
	}).await.expect("expected claim-code writers to queue");
}

#[tokio::test]
async fn open_registration_preserves_codes_and_honors_name_locks() {
	let ctx = TestContext::new("bullnym/open_registration_claim_codes").await;
	let (srv, _ln, forwarder) = ark_server(&ctx).await;
	let svc = Service::start_with(&ctx, &srv, &forwarder, |cfg| {
		cfg.nyms.reserved.push("private-name".into());
	}).await;
	let wallet = ctx.bark_sdk("owner", &srv).create().await;
	let generic = grants(&svc, &["mint", "--count", "2", "--label", "launch"]);
	assert_eq!(generic.len(), 2);
	let code = generic[0]["claim_code"].as_str().unwrap();
	let (_, open) = signed_nym_request(&wallet, "open").await;
	assert_eq!(submit(&svc, &open, Some(code)).await.0, StatusCode::OK);
	assert_eq!(submit(&svc, &open, Some(code)).await.0, StatusCode::CONFLICT);
	assert_eq!(grants(&svc, &["list", "--unused"]).len(), 2);
	let (_, free) = signed_nym_request(&wallet, "free").await;
	assert_eq!(submit(&svc, &free, None).await.0, StatusCode::OK);
	assert!(!command(&svc, &["lock", "--nym", "open"]).status.success());
	assert!(!command(&svc, &["lock", "--nym", "api"]).status.success());
	let config = ctx.datadir.join("reserved-names.toml");
	std::fs::write(&config, "[nyms]\nreserved = [\"private-name\"]\n").unwrap();
	let denied_lock = command(&svc, &["lock", "--config", config.to_str().unwrap(), "--nym", "private-name"]);
	assert!(!denied_lock.status.success());
	assert!(String::from_utf8_lossy(&denied_lock.stderr).contains("this nym is reserved"));
	let (_, private) = signed_nym_request(&wallet, "private-name").await;
	assert_eq!(submit(&svc, &private, Some(code)).await.0, StatusCode::BAD_REQUEST);

	let old = grants(&svc, &["lock", "--nym", "locked", "--expires-days", "1"]).remove(0);
	assert!(!command(&svc, &["lock", "--nym", "locked"]).status.success());
	let conn = svc.app.public.db.conn().await.unwrap();
	conn.execute("UPDATE claim_code SET expires_at = clock_timestamp() - interval '1 second'
		WHERE id::TEXT = $1", &[&old["id"].as_str().unwrap()]).await.unwrap();
	drop(conn);
	let live = grants(&svc, &["lock", "--nym", "locked"]).remove(0);
	let (_, outside) = signed_nym_request(&wallet, "outside").await;
	assert_eq!(submit(&svc, &outside, live["claim_code"].as_str()).await.0, StatusCode::OK);
	let (_, locked) = signed_nym_request(&wallet, "locked").await;
	denied(submit(&svc, &locked, None).await, "ClaimCodeRequired");
	for wrong in [code, old["claim_code"].as_str().unwrap()] {
		denied(submit(&svc, &locked, Some(wrong)).await, "ClaimCodeInvalid");
	}
	let displayed = live["claim_code"].as_str().unwrap().to_lowercase().replace('-', " ");
	assert_eq!(submit(&svc, &locked, Some(&displayed)).await.0, StatusCode::OK);
	let used = grants(&svc, &["list", "--used"]);
	assert_eq!(used.len(), 1);
	assert_eq!(used[0]["used_nym"], "locked");
	assert_eq!(used[0]["used_by_ark_address"], locked["registration"]["ark_address"]);
	assert!(used[0].get("code_hash").is_none() && used[0].get("claim_code").is_none());
	assert_eq!(grants(&svc, &["list", "--locked"]).len(), 2);

	// Queue the revoke first and the matching registration second. After release,
	// this name is open and the former code cannot prevent registration.
	let grant = grants(&svc, &["lock", "--nym", "release"]).remove(0);
	let (_, release) = signed_nym_request(&wallet, "release").await;
	let id = grant["id"].as_str().unwrap();
	let mut blocker = svc.app.public.db.conn().await.unwrap();
	let row_lock = blocker.transaction().await.unwrap();
	row_lock.query_one("SELECT id::TEXT FROM claim_code WHERE id::TEXT = $1 FOR UPDATE", &[&id]).await.unwrap();
	let mut revoke = cli(&svc);
	revoke.args(["revoke", id]);
	let revocation = tokio::task::spawn_blocking(move || revoke.output().unwrap());
	wait_for_claim_writers(&svc, 1).await;
	let (registration, ()) = tokio::join!(
		submit(&svc, &release, grant["claim_code"].as_str()), async {
			wait_for_claim_writers(&svc, 2).await;
			row_lock.commit().await.unwrap();
		},
	);
	assert_eq!(registration.0, StatusCode::OK);
	assert!(revocation.await.unwrap().status.success());
}

#[tokio::test]
async fn required_codes_are_redeemed_once_with_the_registered_nym() {
	let ctx = TestContext::new("bullnym/required_registration_claim_codes").await;
	let (srv, _ln, forwarder) = ark_server(&ctx).await;
	let svc = Service::start_with(&ctx, &srv, &forwarder, |cfg| cfg.registration.require_claim_code = true).await;
	let wallet = ctx.bark_sdk("owner", &srv).create().await;
	let (_, a) = signed_nym_request(&wallet, "alpha").await;
	let (_, b) = signed_nym_request(&wallet, "bravo").await;
	denied(submit(&svc, &a, None).await, "ClaimCodeRequired");
	denied(submit(&svc, &a, Some("unknown")).await, "ClaimCodeInvalid");
	let locked = grants(&svc, &["lock", "--nym", "reserved-for-owner"]).remove(0);
	denied(submit(&svc, &a, locked["claim_code"].as_str()).await, "ClaimCodeInvalid");
	let issued = grants(&svc, &["mint", "--count", "4"]);
	let (_, reserved) = signed_nym_request(&wallet, "reserved-for-owner").await;
	denied(submit(&svc, &reserved, None).await, "ClaimCodeRequired");
	denied(submit(&svc, &reserved, issued[0]["claim_code"].as_str()).await, "ClaimCodeInvalid");
	let mut invalid_signature = a.clone();
	invalid_signature["signature"] = b["signature"].clone();
	assert_eq!(submit(&svc, &invalid_signature, issued[0]["claim_code"].as_str()).await.0, StatusCode::BAD_REQUEST);
	assert!(command(&svc, &["revoke", issued[1]["id"].as_str().unwrap()]).status.success());
	denied(submit(&svc, &a, issued[1]["claim_code"].as_str()).await, "ClaimCodeInvalid");
	let conn = svc.app.public.db.conn().await.unwrap();
	conn.execute("UPDATE claim_code SET expires_at = clock_timestamp() - interval '1 second'
		WHERE id::TEXT = $1", &[&issued[2]["id"].as_str().unwrap()]).await.unwrap();
	drop(conn);
	denied(submit(&svc, &a, issued[2]["claim_code"].as_str()).await, "ClaimCodeInvalid");

	let code = issued[0]["claim_code"].as_str();
	let (one, two) = tokio::join!(submit(&svc, &a, code), submit(&svc, &b, code));
	let (winner, loser, rejected) = if one.0 == StatusCode::OK {
		(&a, &b, two)
	} else {
		assert_eq!(two.0, StatusCode::OK);
		(&b, &a, one)
	};
	denied(rejected, "ClaimCodeInvalid");
	let spare = issued[3]["claim_code"].as_str();
	assert_eq!(submit(&svc, winner, spare).await.0, StatusCode::CONFLICT);
	denied(submit(&svc, loser, code).await, "ClaimCodeInvalid");
	assert_eq!(submit(&svc, loser, spare).await.0, StatusCode::OK);
	assert_eq!(submit(&svc, &reserved, locked["claim_code"].as_str()).await.0, StatusCode::OK);
	assert_eq!(grants(&svc, &["list", "--used"]).len(), 3);
}
