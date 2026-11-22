//! Watches the payment rails of recent sales and the invoices of the
//! Lightning Addresses, and moves them to their next state.
//!
//! A sale is paid by the first rail that delivers its full amount. The
//! watcher keeps checking the other rails until they expire, so a second
//! payment is recorded as excess instead of being missed. A payment to a
//! sale that ended unpaid is a late payment: the owner sees it on that
//! sale, and it never pays another one.
//!
//! On-chain payments go through the stages of [crate::stages]: seen in the
//! mempool (in progress, not guaranteed), paid at 1 confirmation, final at
//! 3, and a payment issue if a reorg takes a counted payment away.

use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{Address, Amount};
use tokio_postgres::GenericClient;

use crate::{App, Backends};
use crate::database::{Rail, SaleState, WatchedPayment};
use crate::rails::forward::ForwardStatus;
use crate::stages::{self, FeePolicy, TxRecord, TxStatus};


/// How long an invoice payment is watched after its sale or invoice
/// expires. A payment made just before expiry can take a little while to
/// deliver.
const DELIVERY_GRACE_SECS: f64 = 600.0;

/// On-chain addresses are checked every this many ticks.
const ONCHAIN_EVERY_TICKS: u64 = 5;

/// What a check found for a sale's invoice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
	/// Delivered to the owner.
	pub received: Amount,
	/// Whether the invoice is done: delivered, or certain never to pay.
	pub settled: bool,
	/// An Ark transfer paid the invoice and is not delivered yet.
	pub awaiting_delivery: bool,
	/// The invoice was paid by an Ark transfer rather than over Lightning.
	pub via_ark: bool,
}

impl Observation {
	pub fn invoice(received: Amount, settled: bool, awaiting_delivery: bool, via_ark: bool) -> Observation {
		Observation { received, settled, awaiting_delivery, via_ark }
	}

	/// The rail that paid, for a sale this observation pays.
	pub fn paid_rail(&self, rail: Rail) -> Rail {
		match rail {
			Rail::Onchain => Rail::Onchain,
			_ if self.via_ark => Rail::Ark,
			_ => Rail::Lightning,
		}
	}
}

pub async fn run(app: Arc<App>) {
	let mut tick = 0u64;
	loop {
		if let Err(e) = run_once(&app, tick % ONCHAIN_EVERY_TICKS == 0).await {
			tracing::warn!("watcher pass failed: {:#}", e);
		}
		tick = tick.wrapping_add(1);
		tokio::time::sleep(app.cfg.sales.poll_interval()).await;
	}
}

/// One pass over all watched payments, then expiry.
pub async fn run_once(app: &App, check_onchain: bool) -> anyhow::Result<()> {
	let payments = watched_payments(app).await?;
	let mut changed = false;
	for p in payments {
		let obs = match observe(app, &p).await {
			Ok(Some(obs)) => obs,
			Ok(None) => continue,
			Err(e) => {
				tracing::warn!("check of sale {} ({}) failed: {:#}", p.sale_public_id, p.rail.as_str(), e);
				continue;
			},
		};
		match record(app, &p, &obs).await {
			Ok(c) => changed |= c,
			Err(e) => tracing::warn!("recording sale {} failed: {:#}", p.sale_public_id, e),
		}
	}
	if check_onchain {
		match watch_onchain(app).await {
			Ok(c) => changed |= c,
			Err(e) => tracing::warn!("on-chain pass failed: {:#}", e),
		}
	}
	match expire_sales(app).await {
		Ok(c) => changed |= c,
		Err(e) => tracing::warn!("expiry pass failed: {:#}", e),
	}
	if let Err(e) = watch_lnurl_invoices(app).await {
		tracing::warn!("Lightning Address invoice pass failed: {:#}", e);
	}
	if changed {
		app.notify_sale_change();
	}
	Ok(())
}

/// The invoices of recent sales.
async fn watched_payments(app: &App) -> anyhow::Result<Vec<WatchedPayment>> {
	let conn = app.watch.db.conn().await?;
	let rows = conn.query("
		SELECT p.id AS payment_id, s.id AS sale_id, s.public_id, s.state, s.amount_sat,
			s.expires_at, p.rail, p.payment_hash, p.onchain_address, p.onchain_address_id,
			p.received_sat, p.pending_sat, p.settled_at
		FROM sale_payment p
		JOIN sale s ON s.id = p.sale_id
		WHERE p.settled_at IS NULL AND p.rail <> 'onchain' AND s.state <> 'canceled' AND (
			s.expires_at > NOW() - make_interval(secs => $1) OR s.state = 'in-progress'
		)
		ORDER BY p.id
	", &[&DELIVERY_GRACE_SECS]).await?;
	Ok(rows.iter().map(WatchedPayment::from_row).collect())
}

/// Checks one rail. `None` means nothing new.
async fn observe(app: &App, p: &WatchedPayment) -> anyhow::Result<Option<Observation>> {
	match p.rail {
		Rail::Lightning | Rail::Ark => {
			let hash = p.payment_hash.as_deref().context("invoice payment without hash")?;
			Ok(match app.watch.forwarder.status(hash).await? {
				ForwardStatus::Unpaid | ForwardStatus::Arriving | ForwardStatus::Unknown => None,
				// Settled by the Ark server, not yet in the owner's mailbox.
				// barkd records the delivery on its own schedule; the Ark
				// server's report of it (S4) is the same guarantee, so the
				// service asks the server too.
				ForwardStatus::AwaitingDelivery => {
					let delivered = match app.ark.receive(hash).await {
						Ok(r) => r.delivered.filter(|d| *d).map(|_| r.amount),
						Err(e) => {
							tracing::debug!("delivery check of invoice {} failed: {:#}", hash, e);
							None
						},
					};
					awaiting_delivery(delivered, p.sale_state)
				},
				ForwardStatus::Delivered(amount) => {
					// The Ark server granted HTLCs only to a Lightning payment.
					let via_ark = match app.ark.receive(hash).await {
						Ok(r) => !r.has_htlcs,
						Err(e) => {
							tracing::warn!("could not tell how invoice {} was paid: {:#}", hash, e);
							p.sale_state == SaleState::InProgress
						},
					};
					Some(Observation::invoice(amount, true, false, via_ark))
				},
				// Canceled or expired unpaid: stop checking it.
				ForwardStatus::Gone => Some(Observation::invoice(Amount::ZERO, true, false, false)),
			})
		},
		Rail::Onchain => Ok(None),
	}
}

/// What an invoice that barkd shows awaiting delivery means for its sale:
/// paid once the Ark server reports the transfer delivered, in progress
/// before. `delivered` is the delivered amount, when the server reports
/// it.
pub fn awaiting_delivery(delivered: Option<Amount>, state: SaleState) -> Option<Observation> {
	match delivered {
		Some(amount) => Some(Observation::invoice(amount, true, false, true)),
		None if state == SaleState::InProgress => None,
		None => Some(Observation::invoice(Amount::ZERO, false, true, true)),
	}
}

/// The state a sale moves to after an observation of its invoice, if it
/// changes. On-chain payments follow [stages::next_onchain_state].
pub fn next_state(state: SaleState, amount_sat: i64, obs: &Observation) -> Option<SaleState> {
	if !state.is_live() {
		return None;
	}
	if obs.settled && obs.received.to_sat() as i64 >= amount_sat {
		return Some(SaleState::Paid);
	}
	// An Ark payment the server settled but has not delivered: the payer
	// cannot take it back, but it is not the owner's yet.
	if obs.awaiting_delivery && state == SaleState::Open {
		return Some(SaleState::InProgress);
	}
	None
}

/// Stores an observation and moves the sale on. Returns whether a sale
/// changed state.
async fn record(app: &App, p: &WatchedPayment, obs: &Observation) -> anyhow::Result<bool> {
	let mut conn = app.watch.db.conn().await?;
	let tx = conn.transaction().await?;

	// Lock the sale, so two payments cannot both mark it paid.
	let row = tx.query_one(
		"SELECT state, amount_sat FROM sale WHERE id = $1 FOR UPDATE", &[&p.sale_id],
	).await?;
	let state: SaleState = row.get("state");
	let amount_sat: i64 = row.get("amount_sat");

	tx.execute("
		UPDATE sale_payment SET received_sat = $2, settled_at = CASE WHEN $3 THEN NOW() END
		WHERE id = $1
	", &[&p.payment_id, &(obs.received.to_sat() as i64), &obs.settled]).await?;
	tx.execute("
		UPDATE sale SET received_sat = (
			SELECT coalesce(sum(received_sat), 0) FROM sale_payment WHERE sale_id = $1
		), updated_at = NOW() WHERE id = $1
	", &[&p.sale_id]).await?;

	let next = next_state(state, amount_sat, obs);
	if let Some(next) = next {
		// A sale paid by its invoice is final at once. An Ark payment that
		// is not delivered yet puts the sale in progress.
		let paid = next == SaleState::Paid;
		let progress = (next == SaleState::InProgress).then_some("ark");
		tx.execute("
			UPDATE sale SET state = $2,
				paid_rail = CASE WHEN $3 THEN $4 ELSE paid_rail END,
				paid_at = CASE WHEN $3 THEN NOW() ELSE paid_at END,
				final_at = CASE WHEN $3 THEN NOW() ELSE final_at END,
				progress = $5,
				notice = NULL
			WHERE id = $1
		", &[&p.sale_id, &next, &paid, &obs.paid_rail(p.rail), &progress]).await?;
	}
	tx.commit().await?;

	if next == Some(SaleState::Paid) {
		tracing::info!("sale {} paid by {}", p.sale_public_id, obs.paid_rail(p.rail).as_str());
		sale_ended(&app.watch, p.sale_id).await;
	}
	Ok(next.is_some())
}

/// An on-chain payment the watcher follows.
struct WatchedAddress {
	payment_id: i64,
	sale_id: i64,
	sale_public_id: String,
	address: String,
	address_id: Option<i64>,
	received_sat: i64,
	pending_sat: i64,
	warnings: Vec<String>,
}

/// The on-chain payments to follow: addresses in their late-payment
/// window, sales in progress or with a payment issue, and sales paid
/// on-chain until they are final.
async fn watched_addresses(app: &App) -> anyhow::Result<Vec<WatchedAddress>> {
	let conn = app.watch.db.conn().await?;
	let rows = conn.query("
		SELECT p.id AS payment_id, s.id AS sale_id, s.public_id, p.onchain_address,
			p.onchain_address_id, p.received_sat, p.pending_sat, p.warnings
		FROM sale_payment p
		JOIN sale s ON s.id = p.sale_id
		WHERE p.rail = 'onchain' AND (
			s.created_at > NOW() - make_interval(secs => $1)
			OR s.state IN ('in-progress', 'payment-issue')
			OR (s.state = 'paid' AND s.paid_rail = 'onchain' AND s.final_at IS NULL)
		)
		ORDER BY p.id
	", &[&(app.cfg.sales.onchain_watch_secs as f64)]).await?;
	Ok(rows.iter().map(|r| WatchedAddress {
		payment_id: r.get("payment_id"),
		sale_id: r.get("sale_id"),
		sale_public_id: r.get("public_id"),
		address: r.get("onchain_address"),
		address_id: r.get("onchain_address_id"),
		received_sat: r.get("received_sat"),
		pending_sat: r.get("pending_sat"),
		warnings: r.get("warnings"),
	}).collect())
}

/// One pass over the on-chain payments. Without an answer from bitcoind
/// nothing changes: a payment never counts as gone for lack of an answer.
async fn watch_onchain(app: &App) -> anyhow::Result<bool> {
	let watched = watched_addresses(app).await?;
	if watched.is_empty() {
		return Ok(false);
	}
	let received = app.watch.chain.received_by_address().await?;
	let fees = FeePolicy {
		estimate_sat_vb: app.watch.chain.fee_estimate(app.cfg.sales.fee_estimate_blocks).await,
		ratio: app.cfg.sales.low_fee_ratio,
		floor_sat_vb: app.cfg.sales.low_fee_floor_sat_vb,
	};
	let mut changed = false;
	for w in watched {
		let known = known_txs(app, w.payment_id).await?;
		let mut txids = received.get(&w.address).cloned().unwrap_or_default().into_iter()
			.collect::<BTreeSet<_>>();
		txids.extend(known.keys().cloned());
		if txids.is_empty() {
			continue;
		}
		let address = Address::from_str(&w.address)?.require_network(app.cfg.network)?;
		let all = txids.iter().cloned().collect::<Vec<_>>();
		let mut records = Vec::with_capacity(all.len());
		for txid in &all {
			match app.watch.chain.observe_tx(txid, &address, &all).await {
				Ok(obs) => records.push(stages::classify(known.get(txid), &obs, &fees)),
				Err(e) => {
					tracing::warn!("check of transaction {} failed: {:#}", txid, e);
					if let Some(k) = known.get(txid) {
						records.push(k.clone());
					}
				},
			}
		}
		match record_onchain(app, &w, &records).await {
			Ok(c) => changed |= c,
			Err(e) => tracing::warn!("recording sale {} failed: {:#}", w.sale_public_id, e),
		}
	}
	Ok(changed)
}

async fn known_txs(app: &App, payment_id: i64) -> anyhow::Result<HashMap<String, TxRecord>> {
	let conn = app.watch.db.conn().await?;
	let rows = conn.query("
		SELECT txid, amount_sat, status, confirmations, low_fee, unconfirmed_parents, replaced,
			absent_count
		FROM onchain_tx WHERE sale_payment_id = $1
	", &[&payment_id]).await?;
	Ok(rows.iter().map(|r| {
		let rec = TxRecord {
			txid: r.get("txid"),
			amount: Amount::from_sat(r.get::<_, i64>("amount_sat") as u64),
			status: TxStatus::parse(r.get("status")),
			confirmations: r.get("confirmations"),
			low_fee: r.get("low_fee"),
			unconfirmed_parents: r.get("unconfirmed_parents"),
			replaced: r.get("replaced"),
			absent_count: r.get("absent_count"),
		};
		(rec.txid.clone(), rec)
	}).collect())
}

/// Stores what the watcher found for an address, and moves its sale on.
/// Returns whether the sale changed.
async fn record_onchain(app: &App, w: &WatchedAddress, records: &[TxRecord]) -> anyhow::Result<bool> {
	let summary = stages::summarize(records);
	let warnings = summary.warnings.iter().map(|w| w.as_str().to_string()).collect::<Vec<_>>();

	let mut conn = app.watch.db.conn().await?;
	let tx = conn.transaction().await?;
	// Lock the sale, so its rails cannot both mark it paid.
	let row = tx.query_one("
		SELECT state, amount_sat, expires_at < NOW() AS expired, paid_rail, progress, final_at
		FROM sale WHERE id = $1 FOR UPDATE
	", &[&w.sale_id]).await?;
	let state: SaleState = row.get("state");
	let amount = Amount::from_sat(row.get::<_, i64>("amount_sat") as u64);
	let expired: bool = row.get("expired");
	let paid_rail: Option<Rail> = row.get("paid_rail");
	let progress: Option<String> = row.get("progress");
	let has_final: bool = row.get::<_, Option<chrono::DateTime<chrono::Utc>>>("final_at").is_some();

	for r in records {
		tx.execute("
			INSERT INTO onchain_tx (sale_payment_id, txid, amount_sat, status, confirmations, low_fee,
				unconfirmed_parents, replaced, absent_count)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
			ON CONFLICT (sale_payment_id, txid) DO UPDATE SET
				amount_sat = $3, status = $4, confirmations = $5, low_fee = $6,
				unconfirmed_parents = $7, replaced = $8, absent_count = $9, updated_at = NOW()
		", &[
			&w.payment_id, &r.txid, &(r.amount.to_sat() as i64), &r.status.as_str(),
			&r.confirmations, &r.low_fee, &r.unconfirmed_parents, &r.replaced, &r.absent_count,
		]).await?;
	}
	if summary.ever_received {
		if let Some(id) = w.address_id {
			tx.execute("UPDATE onchain_address SET ever_received = TRUE WHERE id = $1", &[&id]).await?;
		}
	}
	let confirmed = summary.confirmed.to_sat() as i64;
	let seen = summary.seen.to_sat() as i64;
	let payment_changed = confirmed != w.received_sat || seen != w.pending_sat || warnings != w.warnings;
	tx.execute("
		UPDATE sale_payment SET received_sat = $2, pending_sat = $3, warnings = $4 WHERE id = $1
	", &[&w.payment_id, &confirmed, &seen, &warnings]).await?;
	tx.execute("
		UPDATE sale SET received_sat = (
			SELECT coalesce(sum(received_sat), 0) FROM sale_payment WHERE sale_id = $1
		), updated_at = NOW() WHERE id = $1
	", &[&w.sale_id]).await?;

	let t = stages::next_onchain_state(
		state, amount, paid_rail == Some(Rail::Onchain), progress.as_deref() == Some("onchain"),
		expired, &summary, app.cfg.sales.final_confirmations as i64,
	);
	match t.state {
		Some(SaleState::InProgress) => {
			tx.execute("
				UPDATE sale SET state = 'in-progress', progress = 'onchain', notice = NULL WHERE id = $1
			", &[&w.sale_id]).await?;
		},
		Some(SaleState::Paid) if t.counts_as_paid => {
			tx.execute("
				UPDATE sale SET state = 'paid', paid_rail = 'onchain', paid_at = NOW(), progress = NULL,
					notice = NULL
				WHERE id = $1
			", &[&w.sale_id]).await?;
		},
		Some(SaleState::Paid) => {
			tx.execute("UPDATE sale SET state = 'paid' WHERE id = $1", &[&w.sale_id]).await?;
		},
		Some(SaleState::PaymentIssue) => {
			tracing::warn!("sale {}: a counted on-chain payment went away", w.sale_public_id);
			tx.execute("
				UPDATE sale SET state = 'payment-issue', final_at = NULL WHERE id = $1
			", &[&w.sale_id]).await?;
		},
		Some(back) => {
			let notice = t.notice.map(|n| n.as_str());
			tx.execute("
				UPDATE sale SET state = $2, progress = NULL, notice = $3 WHERE id = $1
			", &[&w.sale_id, &back, &notice]).await?;
		},
		None => {},
	}
	let became_final = t.is_final && !has_final;
	if became_final {
		tx.execute("UPDATE sale SET final_at = NOW() WHERE id = $1", &[&w.sale_id]).await?;
	}
	tx.commit().await?;

	if t.counts_as_paid {
		tracing::info!("sale {} paid on-chain", w.sale_public_id);
		sale_ended(&app.watch, w.sale_id).await;
	}
	if matches!(t.state, Some(SaleState::Expired)) {
		release_address(&app.watch, w.sale_id).await;
	}
	Ok(t.state.is_some() || became_final || payment_changed)
}

/// The payment hash of a sale's invoice, while it is not settled.
pub async fn invoice_hash(b: &Backends, sale_id: i64) -> anyhow::Result<Option<String>> {
	let conn = b.db.conn().await?;
	let row = conn.query_opt(
		"SELECT payment_hash FROM sale_payment WHERE sale_id = $1 AND rail = 'lightning' AND settled_at IS NULL",
		&[&sale_id],
	).await?;
	Ok(row.and_then(|r| r.get("payment_hash")))
}

/// Why an invoice could not be canceled.
#[derive(Debug, thiserror::Error)]
pub enum CancelError {
	/// barkd reports a payment arriving, settled by Ark, or delivered.
	#[error("a payment is arriving")]
	PaymentArriving,
	/// barkd did not answer.
	#[error("barkd is unavailable: {0:#}")]
	Unavailable(anyhow::Error),
}

/// Cancels an invoice in barkd, unless a payment is arriving for it.
pub async fn cancel_invoice(b: &Backends, hash: &str) -> Result<(), CancelError> {
	match b.forwarder.status(hash).await.map_err(CancelError::Unavailable)? {
		ForwardStatus::Gone => Ok(()),
		ForwardStatus::Unpaid => match b.forwarder.cancel(hash).await {
			Ok(()) => Ok(()),
			// barkd refuses once it revealed the preimage. Ask again what
			// it holds.
			Err(e) => match b.forwarder.status(hash).await {
				Ok(ForwardStatus::Gone) => Ok(()),
				Ok(ForwardStatus::Unpaid) | Err(_) => Err(CancelError::Unavailable(e)),
				Ok(_) => Err(CancelError::PaymentArriving),
			},
		},
		ForwardStatus::Arriving | ForwardStatus::AwaitingDelivery | ForwardStatus::Delivered(_)
			| ForwardStatus::Unknown => Err(CancelError::PaymentArriving),
	}
}

/// Ends what a sale holds once it is paid, canceled or expired: its open
/// invoice is canceled, and its on-chain address is released, to be
/// reused after the grace period if it never received anything. It is
/// best effort: a payment already in flight still completes and counts as
/// excess or late.
pub async fn sale_ended(b: &Backends, sale_id: i64) {
	match invoice_hash(b, sale_id).await {
		Ok(Some(hash)) => {
			if let Err(e) = cancel_invoice(b, &hash).await {
				tracing::debug!("invoice {} stays open: {}", hash, e);
			}
		},
		Ok(None) => {},
		Err(e) => tracing::warn!("failed to load the invoice of sale {}: {:#}", sale_id, e),
	}
	release_address(b, sale_id).await;
}

/// Releases the on-chain address of a sale that ended.
async fn release_address(b: &Backends, sale_id: i64) {
	let res = async {
		let conn = b.db.conn().await?;
		conn.execute("
			UPDATE onchain_address SET sale_id = NULL, released_at = NOW() WHERE sale_id = $1
		", &[&sale_id]).await?;
		anyhow::Ok(())
	}.await;
	if let Err(e) = res {
		tracing::warn!("failed to release the address of sale {}: {:#}", sale_id, e);
	}
}

/// Expires open sales past their expiry, and cancels their invoices in
/// barkd. A sale whose invoice payment is arriving stays open for it, up
/// to the delivery grace. An `in-progress` sale is never expired here: it
/// stays open for its payment.
async fn expire_sales(app: &App) -> anyhow::Result<bool> {
	let rows = {
		let conn = app.watch.db.conn().await?;
		conn.query("
			SELECT s.id, s.public_id, p.payment_hash,
				s.expires_at < NOW() - make_interval(secs => $1) AS past_grace
			FROM sale s
			LEFT JOIN sale_payment p ON p.sale_id = s.id AND p.rail = 'lightning' AND p.settled_at IS NULL
			WHERE s.state = 'open' AND s.expires_at < NOW()
		", &[&DELIVERY_GRACE_SECS]).await?
	};
	let mut changed = false;
	for row in rows {
		let sale_id: i64 = row.get("id");
		let past_grace: bool = row.get("past_grace");
		if let Some(hash) = row.get::<_, Option<String>>("payment_hash") {
			match cancel_invoice(&app.watch, &hash).await {
				Ok(()) => {},
				Err(CancelError::PaymentArriving) if !past_grace => continue,
				// The invoice expires at the Ark server at the same time, so
				// no new payment can start.
				Err(e) => tracing::info!("expiring sale {} with its invoice open: {}", sale_id, e),
			}
		}
		let n = {
			let conn = app.watch.db.conn().await?;
			expire_sale(&*conn, sale_id).await?
		};
		if n > 0 {
			release_address(&app.watch, sale_id).await;
			changed = true;
		}
	}
	Ok(changed)
}

pub async fn expire_sale(conn: &impl GenericClient, sale_id: i64) -> anyhow::Result<u64> {
	Ok(conn.execute("
		UPDATE sale SET state = 'expired', updated_at = NOW()
		WHERE id = $1 AND state = 'open' AND expires_at < NOW()
	", &[&sale_id]).await?)
}

/// Records the Lightning payments to the Lightning Addresses, and cancels
/// unpaid invoices at expiry.
async fn watch_lnurl_invoices(app: &App) -> anyhow::Result<()> {
	let rows = {
		let conn = app.watch.db.conn().await?;
		conn.query("
			SELECT id, payment_hash, expires_at < NOW() AS expired,
				expires_at < NOW() - make_interval(secs => $1) AS past_grace
			FROM lnurl_invoice WHERE state = 'open' ORDER BY id
		", &[&DELIVERY_GRACE_SECS]).await?
	};
	for row in rows {
		let id: i64 = row.get("id");
		let hash: String = row.get("payment_hash");
		let expired: bool = row.get("expired");
		let past_grace: bool = row.get("past_grace");
		let status = match app.watch.forwarder.status(&hash).await {
			Ok(s) => s,
			Err(e) if past_grace => {
				tracing::info!("closing Lightning Address invoice {} unanswered: {:#}", hash, e);
				ForwardStatus::Gone
			},
			Err(e) => {
				tracing::warn!("check of Lightning Address invoice {} failed: {:#}", hash, e);
				continue;
			},
		};
		let conn = app.watch.db.conn().await?;
		match status {
			ForwardStatus::Delivered(amount) => {
				conn.execute("
					UPDATE lnurl_invoice SET state = 'paid', received_sat = $2, settled_at = NOW()
					WHERE id = $1
				", &[&id, &(amount.to_sat() as i64)]).await?;
			},
			ForwardStatus::Gone => {
				conn.execute("
					UPDATE lnurl_invoice SET state = 'expired', settled_at = NOW() WHERE id = $1
				", &[&id]).await?;
			},
			ForwardStatus::Unpaid if expired => {
				match cancel_invoice(&app.watch, &hash).await {
					Ok(()) => {
						conn.execute("
							UPDATE lnurl_invoice SET state = 'expired', settled_at = NOW() WHERE id = $1
						", &[&id]).await?;
					},
					Err(e) => tracing::info!("Lightning Address invoice {} stays open: {}", hash, e),
				}
			},
			_ => {},
		}
	}
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	fn obs(received: u64, settled: bool) -> Observation {
		Observation::invoice(Amount::from_sat(received), settled, false, false)
	}

	#[test]
	fn invoice_transitions() {
		use SaleState::*;
		// The invoice pays on delivery of the full amount.
		assert_eq!(next_state(Open, 1000, &obs(1000, true)), Some(Paid));
		assert_eq!(next_state(Open, 1000, &obs(1200, true)), Some(Paid));
		assert_eq!(next_state(Open, 1000, &obs(0, true)), None, "canceled unpaid");
		// An Ark payment settled by the server but not delivered is in
		// progress, never paid.
		let awaiting = Observation::invoice(Amount::ZERO, false, true, true);
		assert_eq!(next_state(Open, 1000, &awaiting), Some(InProgress));
		assert_eq!(next_state(InProgress, 1000, &awaiting), None);
		let delivered = Observation::invoice(Amount::from_sat(1000), true, false, true);
		assert_eq!(next_state(InProgress, 1000, &delivered), Some(Paid));
		assert_eq!(delivered.paid_rail(Rail::Lightning), Rail::Ark);
		assert_eq!(obs(1000, true).paid_rail(Rail::Lightning), Rail::Lightning);
		// A payment after the sale ended does not change it: it is a late
		// payment.
		assert_eq!(next_state(Expired, 1000, &obs(1000, true)), None);
		assert_eq!(next_state(Canceled, 1000, &obs(1000, true)), None);
		assert_eq!(next_state(Paid, 1000, &obs(1000, true)), None);
		assert_eq!(next_state(PaymentIssue, 1000, &obs(1000, true)), None);
	}

	#[test]
	fn ark_payment_awaiting_delivery() {
		use SaleState::*;
		// Not delivered yet: in progress, and nothing new once it is.
		let o = awaiting_delivery(None, Open).unwrap();
		assert_eq!(next_state(Open, 1000, &o), Some(InProgress));
		assert_eq!(awaiting_delivery(None, InProgress), None);
		// The Ark server reports it delivered: paid by Ark.
		let o = awaiting_delivery(Some(Amount::from_sat(1000)), InProgress).unwrap();
		assert_eq!(next_state(InProgress, 1000, &o), Some(Paid));
		assert_eq!(o.paid_rail(Rail::Lightning), Rail::Ark);
	}
}
