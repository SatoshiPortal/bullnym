//! Watches the payment rails of recent sales and the invoices of the
//! Lightning Addresses, and moves them to their next state.
//!
//! A sale is paid by the first rail that delivers its full amount. The
//! watcher keeps checking its invoice until barkd says how it ended, so a
//! second payment over it is recorded as excess instead of being missed.
//! A payment to a sale that ended unpaid is a late payment: the owner sees
//! it on that sale, and it never pays another one.
//!
//! On-chain payments go through the stages of [crate::stages]: seen in the
//! mempool (in progress, not guaranteed), then paid at 1 confirmation,
//! which is final as in posd. As in posd, an address stays watched for 7
//! days after its sale was created: a payment beyond the amount of a paid
//! sale counts in its received amount, and never changes its state.
//!
//! barkd is the one source of truth for an invoice: a sale is paid when
//! barkd reports the forwarded receive settled, and an Ark payment barkd
//! shows awaiting delivery keeps its sale in progress, with no deadline.
//! The Ark server only labels a settled invoice's rail: Lightning if it
//! granted HTLCs, Ark if not. The label never holds back the paid state;
//! when the server cannot be read, the rail stays unknown.

use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;
use std::sync::Arc;

use bitcoin::{Address, Amount};

use crate::{App, Backends};
use crate::database::{Rail, SaleState, WatchedPayment};
use crate::rails::forward::ForwardStatus;
use crate::stages::{self, FeePolicy, TxRecord, TxStatus};


/// How long an open sale waits past its expiry when a payment may be
/// arriving, or barkd does not answer, before it expires. Its invoice stays
/// watched after that until barkd gives its outcome.
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
	/// Whether an Ark transfer rather than a Lightning payment paid the
	/// invoice. `None` when the Ark server could not say.
	pub via_ark: Option<bool>,
}

impl Observation {
	pub fn invoice(received: Amount, settled: bool, awaiting_delivery: bool, via_ark: Option<bool>) -> Observation {
		Observation { received, settled, awaiting_delivery, via_ark }
	}

	/// The rail that paid, for a sale this observation pays. `None` when it
	/// is not known.
	pub fn paid_rail(&self, rail: Rail) -> Option<Rail> {
		match rail {
			Rail::Onchain => Some(Rail::Onchain),
			_ => self.via_ark.map(|ark| if ark { Rail::Ark } else { Rail::Lightning }),
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
	let mut checked_at_expiry = Vec::new();
	for p in payments {
		let Some(hash) = &p.payment_hash else { continue };
		let status = match app.watch.forwarder.status(hash).await {
			Ok(status) => status,
			Err(e) => {
				tracing::warn!("check of sale {} failed: {:#}", p.sale_public_id, e);
				continue;
			},
		};
		if let Err(e) = record_invoice_status(app, p.sale_id, p.rail, status).await {
			tracing::warn!("recording sale {} failed: {:#}", p.sale_public_id, e);
		}
	}
	if check_onchain {
		match watch_onchain(app).await {
			Ok(checked) => checked_at_expiry = checked,
			Err(e) => tracing::warn!("on-chain pass failed: {:#}", e),
		}
	}
	if let Err(e) = expire_sales(app, &checked_at_expiry).await {
		tracing::warn!("expiry pass failed: {:#}", e);
	}
	if let Err(e) = watch_lnurl_invoices(app).await {
		tracing::warn!("Lightning Address invoice pass failed: {:#}", e);
	}
	Ok(())
}

/// The sale invoices whose outcome barkd has not given yet. An invoice
/// stays watched, whatever its age, until barkd says it was delivered or
/// ended unpaid: a read error never ends the watch. A canceled sale's
/// invoice was canceled in barkd first.
pub async fn watched_payments(app: &App) -> anyhow::Result<Vec<WatchedPayment>> {
	let conn = app.watch.db.conn().await?;
	let rows = conn.query("
		SELECT p.id AS payment_id, s.id AS sale_id, s.public_id, s.state, s.amount_sat,
			s.expires_at, p.rail, p.payment_hash, p.onchain_address,
			p.received_sat, p.pending_sat, p.awaiting_delivery_at, p.settled_at
		FROM sale_payment p
		JOIN sale s ON s.id = p.sale_id
		WHERE p.settled_at IS NULL AND p.rail <> 'onchain' AND p.payment_hash IS NOT NULL
			AND s.state <> 'canceled'
		ORDER BY p.id
	", &[]).await?;
	Ok(rows.iter().map(WatchedPayment::from_row).collect())
}

/// What an invoice that barkd shows awaiting delivery means for its sale:
/// in progress, until barkd settles it. The pending Ark payment is recorded
/// on the invoice, also when the sale is already in progress for an
/// on-chain payment, so the sale stays in progress for it if that one goes
/// away. `known`: it is recorded already.
pub fn awaiting_delivery(state: SaleState, known: bool) -> Option<Observation> {
	if known && state != SaleState::Open {
		return None;
	}
	Some(Observation::invoice(Amount::ZERO, false, true, Some(true)))
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

/// Applies the strongest observation already returned by barkd. Polling, expiry
/// and cashier cancellation all use the same transaction.
pub(crate) async fn record_invoice_status(app: &App, sale_id: i64, rail: Rail, status: ForwardStatus) -> anyhow::Result<()> {
	if matches!(status, ForwardStatus::Unpaid | ForwardStatus::Unknown) {
		return Ok(());
	}
	let mut conn = app.watch.db.conn().await?;
	let tx = conn.transaction().await?;
	let row = tx.query_one("
		SELECT s.state, s.amount_sat, s.public_id, p.id, p.awaiting_delivery_at IS NOT NULL AS awaiting
		FROM sale s JOIN sale_payment p ON p.sale_id = s.id
		WHERE s.id = $1 AND p.rail = $2 FOR UPDATE OF s
	", &[&sale_id, &rail]).await?;
	let state: SaleState = row.get("state");
	let payment_id: i64 = row.get("id");
	if status == ForwardStatus::Arriving {
		tx.execute("UPDATE sale_payment SET arriving_at = NOW() WHERE id = $1 AND settled_at IS NULL AND arriving_at IS NULL", &[&payment_id]).await?;
		tx.commit().await?;
		return Ok(());
	}
	let amount_sat: i64 = row.get("amount_sat");
	let obs = match status {
		ForwardStatus::AwaitingDelivery => match awaiting_delivery(state, row.get("awaiting")) {
			Some(obs) => obs,
			None => return Ok(()),
		},
		ForwardStatus::Delivered(amount) => Observation::invoice(amount, true, false, None),
		ForwardStatus::Gone => Observation::invoice(Amount::ZERO, true, false, None),
		_ => return Ok(()),
	};
	let recorded = tx.execute("
		UPDATE sale_payment SET received_sat = $2, settled_at = CASE WHEN $3 THEN NOW() END,
			awaiting_delivery_at = CASE WHEN $4 THEN coalesce(awaiting_delivery_at, NOW()) ELSE awaiting_delivery_at END
		WHERE id = $1 AND settled_at IS NULL
	", &[&payment_id, &(obs.received.to_sat() as i64), &obs.settled, &obs.awaiting_delivery]).await?;
	if recorded == 0 {
		return Ok(());
	}
	tx.execute("
		UPDATE sale SET received_sat = (
			SELECT coalesce(sum(received_sat), 0) FROM sale_payment WHERE sale_id = $1
		), updated_at = GREATEST(clock_timestamp(), updated_at + INTERVAL '1 microsecond') WHERE id = $1
	", &[&sale_id]).await?;

	let next = next_state(state, amount_sat, &obs);
	if let Some(next) = next {
		// An Ark payment that is not delivered yet puts the sale in
		// progress.
		let paid = next == SaleState::Paid;
		let progress = (next == SaleState::InProgress).then_some("ark");
		tx.execute("
			UPDATE sale SET state = $2,
				paid_rail = CASE WHEN $3 THEN $4 ELSE paid_rail END,
				paid_at = CASE WHEN $3 THEN NOW() ELSE paid_at END,
				progress = $5,
				notice = NULL
			WHERE id = $1
		", &[&sale_id, &next, &paid, &obs.paid_rail(rail), &progress]).await?;
	}
	tx.commit().await?;

	app.notify_sale_change(sale_id);
	if next == Some(SaleState::Paid) {
		tracing::info!("sale {} paid by its invoice", row.get::<_, String>("public_id"));
	}
	Ok(())
}

/// Labels the rail of a sale paid by its invoice, from the Ark server: only
/// a Lightning payment gets HTLCs, so an invoice without them was paid by
/// an Ark transfer. The sale is paid already; the label never holds it
/// back.
async fn label_rail(app: &App, sale_id: i64, payment_hash: &str) -> anyhow::Result<()> {
	let rail = if app.ark.has_htlcs(payment_hash).await? { Rail::Lightning } else { Rail::Ark };
	let conn = app.watch.db.conn().await?;
	conn.execute("
		UPDATE sale SET paid_rail = $2, updated_at = GREATEST(clock_timestamp(), updated_at + INTERVAL '1 microsecond') WHERE id = $1 AND paid_rail IS NULL
	", &[&sale_id, &rail]).await?;
	Ok(())
}

/// Labels the sales paid by their invoice whose rail the Ark server could
/// not give when they were paid, for 7 days.
async fn label_rails(app: &App) -> anyhow::Result<()> {
	let rows = {
		let conn = app.watch.db.conn().await?;
		conn.query("
			SELECT s.id, s.public_id, p.payment_hash FROM sale s
			JOIN sale_payment p ON p.sale_id = s.id AND p.rail = 'lightning'
			WHERE s.state = 'paid' AND s.paid_rail IS NULL AND s.paid_at > NOW() - INTERVAL '7 days'
				AND p.settled_at IS NOT NULL AND p.received_sat > 0 AND p.payment_hash IS NOT NULL
		", &[]).await?
	};
	for row in rows {
		let hash: String = row.get("payment_hash");
		match label_rail(app, row.get("id"), &hash).await {
			Ok(()) => app.notify_sale_change(row.get("id")),
			Err(e) => tracing::debug!("sale {} has no rail label yet: {:#}", row.get::<_, String>("public_id"), e),
		}
	}
	Ok(())
}

/// Method labels can arrive after payment, even when their server is slow.
pub async fn run_labels(app: Arc<App>) {
	loop {
		if let Err(e) = label_rails(&app).await {
			tracing::warn!("rail label pass failed: {:#}", e);
		}
		tokio::time::sleep(app.cfg.sales.poll_interval()).await;
	}
}

/// An on-chain payment the watcher follows.
struct WatchedAddress {
	payment_id: i64,
	sale_id: i64,
	sale_public_id: String,
	address: String,
	received_sat: i64,
	pending_sat: i64,
	warnings: Vec<String>,
	past_expiry: bool,
}

/// The on-chain payments to follow, as in posd: every sale address for 7
/// days after its sale was created, whatever the sale's state, so a late
/// payment to an ended sale and an extra payment to a paid one are recorded
/// on that sale; and the address of a sale still in progress until its
/// payment confirms or goes. An overdue open sale also needs a successful
/// address check before expiry if the watcher was offline. A paid sale stays
/// paid whatever its address shows.
async fn watched_addresses(app: &App) -> anyhow::Result<Vec<WatchedAddress>> {
	let conn = app.watch.db.conn().await?;
	let rows = conn.query("
		SELECT p.id AS payment_id, s.id AS sale_id, s.public_id, p.onchain_address,
			p.received_sat, p.pending_sat, p.warnings, s.state = 'open' AND s.expires_at < NOW() AS past_expiry
		FROM sale_payment p
		JOIN sale s ON s.id = p.sale_id
		WHERE p.rail = 'onchain' AND (
			s.created_at > NOW() - make_interval(secs => $1)
			OR (s.state = 'in-progress' AND s.progress = 'onchain')
			OR (s.state = 'open' AND s.expires_at < NOW())
		)
		ORDER BY p.id
	", &[&(app.cfg.sales.onchain_watch_secs as f64)]).await?;
	Ok(rows.iter().map(|r| WatchedAddress {
		payment_id: r.get("payment_id"),
		sale_id: r.get("sale_id"),
		sale_public_id: r.get("public_id"),
		address: r.get("onchain_address"),
		received_sat: r.get("received_sat"),
		pending_sat: r.get("pending_sat"),
		warnings: r.get("warnings"),
		past_expiry: r.get("past_expiry"),
	}).collect())
}

/// One pass over the on-chain payments. Without an answer from bitcoind
/// nothing changes: a payment never counts as gone for lack of an answer.
async fn watch_onchain(app: &App) -> anyhow::Result<Vec<i64>> {
	let watched = watched_addresses(app).await?;
	if watched.is_empty() {
		return Ok(Vec::new());
	}
	let mut checked = Vec::new();
	let ready = watched.iter().any(|w| w.past_expiry) && app.watch.chain.ready_for_absence().await.unwrap_or(false);
	let received = app.watch.chain.received_by_address().await?;
	let fees = FeePolicy {
		estimate_sat_vb: app.watch.chain.fee_estimate(app.cfg.sales.fee_estimate_blocks).await,
		ratio: app.cfg.sales.low_fee_ratio,
		floor_sat_vb: app.cfg.sales.low_fee_floor_sat_vb,
	};
	for w in watched {
		let known = known_txs(app, w.payment_id).await?;
		let mut txids = received.get(&w.address).cloned().unwrap_or_default().into_iter()
			.collect::<BTreeSet<_>>();
		txids.extend(known.keys().cloned());
		if txids.is_empty() {
			if w.past_expiry && ready { checked.push(w.sale_id); }
			continue;
		}
		let address = Address::from_str(&w.address)?.require_network(app.cfg.network)?;
		let all = txids.iter().cloned().collect::<Vec<_>>();
		let mut records = Vec::with_capacity(all.len());
		let mut complete = ready && w.past_expiry;
		for txid in &all {
			match app.watch.chain.observe_tx(txid, &address, &all).await {
				Ok(obs) => {
					if obs.confirmations == 0 && obs.mempool == stages::Mempool::Unknown { complete = false; }
					records.extend(stages::classify(known.get(txid), &obs, &fees));
				},
				Err(e) => {
					complete = false;
					tracing::warn!("check of transaction {} failed: {:#}", txid, e);
					if let Some(k) = known.get(txid) {
						records.push(k.clone());
					}
				},
			}
		}
		match record_onchain(app, &w, &records).await {
			Ok(()) if complete => checked.push(w.sale_id),
			Ok(()) => {},
			Err(e) => tracing::warn!("recording sale {} failed: {:#}", w.sale_public_id, e),
		}
	}
	Ok(checked)
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
async fn record_onchain(app: &App, w: &WatchedAddress, records: &[TxRecord]) -> anyhow::Result<()> {
	let summary = stages::summarize(records);
	let warnings = summary.warnings.iter().map(|w| w.as_str().to_string()).collect::<Vec<_>>();

	let mut conn = app.watch.db.conn().await?;
	let tx = conn.transaction().await?;
	// Lock the sale, so its rails cannot both mark it paid.
	let row = tx.query_one("
		SELECT state, amount_sat, expires_at < NOW() AS expired, progress
		FROM sale WHERE id = $1 FOR UPDATE
	", &[&w.sale_id]).await?;
	let state: SaleState = row.get("state");
	let amount = Amount::from_sat(row.get::<_, i64>("amount_sat") as u64);
	let expired: bool = row.get("expired");
	let progress: Option<String> = row.get("progress");
	// An Ark payment barkd showed awaiting delivery and has not settled.
	let ark_pending: bool = tx.query_one("
		SELECT EXISTS (SELECT 1 FROM sale_payment WHERE sale_id = $1 AND rail = 'lightning'
			AND awaiting_delivery_at IS NOT NULL AND settled_at IS NULL)
	", &[&w.sale_id]).await?.get(0);

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
	let confirmed = summary.confirmed.to_sat() as i64;
	let seen = summary.seen.to_sat() as i64;
	let payment_changed = confirmed != w.received_sat || seen != w.pending_sat || warnings != w.warnings;
	let t = stages::next_onchain_state(
		state, amount, progress.as_deref() == Some("onchain"), ark_pending, expired, &summary,
	);
	if !payment_changed && t.state.is_none() {
		tx.commit().await?;
		return Ok(());
	}
	tx.execute("
		UPDATE sale_payment SET received_sat = $2, pending_sat = $3, warnings = $4 WHERE id = $1
	", &[&w.payment_id, &confirmed, &seen, &warnings]).await?;
	tx.execute("
		UPDATE sale SET received_sat = (
			SELECT coalesce(sum(received_sat), 0) FROM sale_payment WHERE sale_id = $1
		), updated_at = GREATEST(clock_timestamp(), updated_at + INTERVAL '1 microsecond') WHERE id = $1
	", &[&w.sale_id]).await?;

	match t.state {
		Some(SaleState::InProgress) if t.ark_progress => {
			let notice = t.notice.map(|n| n.as_str());
			tx.execute("
				UPDATE sale SET state = 'in-progress', progress = 'ark', notice = $2 WHERE id = $1
			", &[&w.sale_id, &notice]).await?;
		},
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
		Some(back) => {
			let notice = t.notice.map(|n| n.as_str());
			tx.execute("
				UPDATE sale SET state = $2, progress = NULL, notice = $3 WHERE id = $1
			", &[&w.sale_id, &back, &notice]).await?;
		},
		None => {},
	}
	tx.commit().await?;

	app.notify_sale_change(w.sale_id);
	if t.counts_as_paid {
		drop(conn);
		tracing::info!("sale {} paid on-chain", w.sale_public_id);
		if let Err(e) = close_other_invoice(app, w.sale_id).await {
			tracing::debug!("could not close invoice on paid sale {}: {:#}", w.sale_public_id, e);
		}
	}
	Ok(())
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
	PaymentArriving(ForwardStatus),
	/// barkd reports a state the service does not know.
	#[error("barkd reports a state this service does not know")]
	UnknownState,
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
				Ok(ForwardStatus::Unknown) => Err(CancelError::UnknownState),
				Ok(status) => Err(CancelError::PaymentArriving(status)),
			},
		},
		status @ (ForwardStatus::Arriving | ForwardStatus::AwaitingDelivery | ForwardStatus::Delivered(_)) => {
			Err(CancelError::PaymentArriving(status))
		},
		ForwardStatus::Unknown => Err(CancelError::UnknownState),
	}
}

/// An invoice already in flight when Bitcoin paid the sale still counts on that receipt.
async fn close_other_invoice(app: &App, sale_id: i64) -> anyhow::Result<()> {
	let Some(hash) = invoice_hash(&app.watch, sale_id).await? else { return Ok(()) };
	let status = match cancel_invoice(&app.watch, &hash).await {
		Ok(()) => ForwardStatus::Gone,
		Err(CancelError::PaymentArriving(status)) => status,
		Err(e) => return Err(e.into()),
	};
	record_invoice_status(app, sale_id, Rail::Lightning, status).await
}

/// Expires open sales past their expiry, and cancels their invoices in
/// barkd. A sale whose invoice payment is arriving stays open for it, up
/// to the delivery grace. An `in-progress` sale is never expired here: it
/// stays open for its payment.
async fn expire_sales(app: &App, checked_addresses: &[i64]) -> anyhow::Result<()> {
	let rows = {
		let mut conn = app.watch.db.conn().await?;
		let expired = conn.query("
			SELECT id FROM sale WHERE issued_at IS NULL AND expires_at < NOW() - make_interval(secs => $1)
		", &[&DELIVERY_GRACE_SECS]).await?;
		for row in expired {
			crate::sales::discard_reservation(&mut conn, row.get("id")).await?;
		}
		conn.query("
			SELECT s.id, p.payment_hash,
				s.expires_at < NOW() - make_interval(secs => $1) AS past_grace,
				(p.awaiting_delivery_at IS NOT NULL OR p.arriving_at IS NOT NULL) AS payment_seen
			FROM sale s
			LEFT JOIN sale_payment p ON p.sale_id = s.id AND p.rail = 'lightning' AND p.settled_at IS NULL
			WHERE s.state = 'open' AND s.issued_at IS NOT NULL AND s.expires_at < NOW()
				AND (s.id = ANY($2) OR NOT EXISTS (SELECT 1 FROM sale_payment oc WHERE oc.sale_id = s.id AND oc.rail = 'onchain'))
		", &[&DELIVERY_GRACE_SECS, &checked_addresses]).await?
	};
	for row in rows {
		let sale_id: i64 = row.get("id");
		let past_grace: bool = row.get("past_grace");
		let mut ended_unpaid = false;
		if let Some(hash) = row.get::<_, Option<String>>("payment_hash") {
			let canceled = cancel_invoice(&app.watch, &hash).await;
			let seen = row.get::<_, Option<bool>>("payment_seen").unwrap_or(false);
			ended_unpaid = canceled.is_ok();
			if let Err(CancelError::PaymentArriving(status)) = &canceled {
				// Remembered, so a later failed read or a restart never
				// expires the sale.
				record_invoice_status(app, sale_id, Rail::Lightning, *status).await?;
			}
			if !expires_now(&canceled, past_grace, seen) {
				continue;
			}
			if let Err(e) = canceled {
				// The invoice stays watched until barkd says how it ended; a
				// payment that still arrives shows as a late payment.
				tracing::info!("expiring sale {} with its invoice open: {}", sale_id, e);
			}
		}
		if expire_sale(app, sale_id, ended_unpaid, checked_addresses).await? {
			app.notify_sale_change(sale_id);
		}
	}
	Ok(())
}

/// Whether an open sale past its expiry expires now, given how canceling
/// its invoice went. barkd showing a payment arriving, awaiting delivery
/// or delivered is positive evidence: the sale waits for it, however long.
/// `seen`: barkd showed such a payment before; then only barkd's answer
/// that the invoice ended unpaid expires the sale. Otherwise, without an
/// answer, or with a state the service does not know, it waits up to the
/// delivery grace.
pub fn expires_now(canceled: &Result<(), CancelError>, past_grace: bool, seen: bool) -> bool {
	match canceled {
		Ok(()) => true,
		Err(CancelError::PaymentArriving(_)) => false,
		Err(CancelError::UnknownState | CancelError::Unavailable(_)) => past_grace && !seen,
	}
}

async fn expire_sale(app: &App, sale_id: i64, ended_unpaid: bool, checked: &[i64]) -> anyhow::Result<bool> {
	let mut conn = app.watch.db.conn().await?;
	let tx = conn.transaction().await?;
	// Recheck both rails after any concurrent payment observer releases the sale.
	tx.query_opt("SELECT id FROM sale WHERE id = $1 FOR UPDATE", &[&sale_id]).await?;
	let count = tx.execute("
		UPDATE sale SET state = 'expired', updated_at = GREATEST(clock_timestamp(), updated_at + INTERVAL '1 microsecond')
		WHERE id = $1 AND state = 'open' AND expires_at < NOW()
			AND (id = ANY($3) OR NOT EXISTS (SELECT 1 FROM sale_payment WHERE sale_id = $1 AND rail = 'onchain'))
			AND ($2 OR NOT EXISTS (SELECT 1 FROM sale_payment WHERE sale_id = $1 AND settled_at IS NULL
				AND (awaiting_delivery_at IS NOT NULL OR arriving_at IS NOT NULL)))
	", &[&sale_id, &ended_unpaid, &checked]).await?;
	tx.commit().await?;
	Ok(count > 0)
}

/// What to do with an open Lightning Address invoice after asking barkd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LnurlStep {
	/// Delivered to the owner: record it.
	Paid(Amount),
	/// Canceled or expired unpaid, as barkd says.
	Expired,
	/// Unpaid past its expiry: cancel it in barkd.
	Cancel,
	/// Nothing known for sure: keep watching.
	Keep,
}

/// The next step for an open Lightning Address invoice. `status` is
/// barkd's answer, `None` when barkd did not answer. Only barkd's explicit
/// answer ends the watch: a read error never does, however old the invoice.
pub fn lnurl_step(status: Option<ForwardStatus>, expired: bool) -> LnurlStep {
	match status {
		Some(ForwardStatus::Delivered(amount)) => LnurlStep::Paid(amount),
		Some(ForwardStatus::Gone) => LnurlStep::Expired,
		Some(ForwardStatus::Unpaid) if expired => LnurlStep::Cancel,
		Some(_) | None => LnurlStep::Keep,
	}
}

/// Records the Lightning payments to the Lightning Addresses, and cancels
/// unpaid invoices at expiry. An invoice stays watched until barkd says how
/// it ended.
async fn watch_lnurl_invoices(app: &App) -> anyhow::Result<()> {
	let rows = {
		let conn = app.watch.db.conn().await?;
		// A reservation that never got its invoice, because the process
		// stopped in between, was never shown to a payer.
		conn.execute("
			DELETE FROM lnurl_invoice
			WHERE payment_hash IS NULL AND expires_at < NOW() - make_interval(secs => $1)
		", &[&DELIVERY_GRACE_SECS]).await?;
		conn.query("
			SELECT id, payment_hash, expires_at < NOW() AS expired
			FROM lnurl_invoice WHERE state = 'open' AND payment_hash IS NOT NULL ORDER BY id
		", &[]).await?
	};
	for row in rows {
		let id: i64 = row.get("id");
		let hash: String = row.get("payment_hash");
		let expired: bool = row.get("expired");
		let status = match app.watch.forwarder.status(&hash).await {
			Ok(s) => Some(s),
			Err(e) => {
				tracing::warn!("check of Lightning Address invoice {} failed: {:#}", hash, e);
				None
			},
		};
		let mut step = lnurl_step(status, expired);
		if step == LnurlStep::Cancel {
			step = match cancel_invoice(&app.watch, &hash).await {
				Ok(()) => LnurlStep::Expired,
				Err(CancelError::PaymentArriving(observed)) => lnurl_step(Some(observed), expired),
				Err(e) => {
					tracing::info!("Lightning Address invoice {} stays open: {}", hash, e);
					LnurlStep::Keep
				},
			};
		}
		match step {
			LnurlStep::Paid(amount) => {
				app.watch.db.conn().await?.execute("
					UPDATE lnurl_invoice SET state = 'paid', received_sat = $2, settled_at = NOW()
					WHERE id = $1 AND state = 'open'
				", &[&id, &(amount.to_sat() as i64)]).await?;
			},
			LnurlStep::Expired => {
				app.watch.db.conn().await?.execute("
					UPDATE lnurl_invoice SET state = 'expired', settled_at = NOW() WHERE id = $1 AND state = 'open'
				", &[&id]).await?;
			},
			LnurlStep::Keep | LnurlStep::Cancel => {},
		}
	}
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	fn obs(received: u64, settled: bool) -> Observation {
		Observation::invoice(Amount::from_sat(received), settled, false, Some(false))
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
		let awaiting = Observation::invoice(Amount::ZERO, false, true, Some(true));
		assert_eq!(next_state(Open, 1000, &awaiting), Some(InProgress));
		assert_eq!(next_state(InProgress, 1000, &awaiting), None);
		let delivered = Observation::invoice(Amount::from_sat(1000), true, false, Some(true));
		assert_eq!(next_state(InProgress, 1000, &delivered), Some(Paid));
		assert_eq!(delivered.paid_rail(Rail::Lightning), Some(Rail::Ark));
		assert_eq!(obs(1000, true).paid_rail(Rail::Lightning), Some(Rail::Lightning));
		// A payment after the sale ended does not change it: it is a late
		// payment.
		assert_eq!(next_state(Expired, 1000, &obs(1000, true)), None);
		assert_eq!(next_state(Canceled, 1000, &obs(1000, true)), None);
		assert_eq!(next_state(Paid, 1000, &obs(1000, true)), None);
	}

	#[test]
	fn barkd_settles_and_the_server_only_labels_the_rail() {
		use SaleState::*;
		// The server could not be read: paid all the same, the rail unknown,
		// never taken from the sale's state.
		let unknown = Observation::invoice(Amount::from_sat(1000), true, false, None);
		for state in [Open, InProgress] {
			assert_eq!(next_state(state, 1000, &unknown), Some(Paid));
		}
		assert_eq!(unknown.paid_rail(Rail::Lightning), None);
	}

	#[test]
	fn read_errors_never_end_a_lightning_address_invoice() {
		let a = Amount::from_sat(21);
		// No answer from barkd: keep watching, even long after expiry.
		assert_eq!(lnurl_step(None, false), LnurlStep::Keep);
		assert_eq!(lnurl_step(None, true), LnurlStep::Keep);
		// Only explicit answers end it.
		assert_eq!(lnurl_step(Some(ForwardStatus::Delivered(a)), true), LnurlStep::Paid(a));
		assert_eq!(lnurl_step(Some(ForwardStatus::Gone), false), LnurlStep::Expired);
		assert_eq!(lnurl_step(Some(ForwardStatus::Unpaid), true), LnurlStep::Cancel);
		assert_eq!(lnurl_step(Some(ForwardStatus::Unpaid), false), LnurlStep::Keep);
		for arriving in [ForwardStatus::Arriving, ForwardStatus::AwaitingDelivery, ForwardStatus::Unknown] {
			assert_eq!(lnurl_step(Some(arriving), true), LnurlStep::Keep);
		}
	}

	#[test]
	fn ark_payment_awaiting_delivery() {
		use SaleState::*;
		// barkd shows it awaiting delivery: in progress, recorded on the
		// invoice, with nothing new until barkd settles it, however long.
		let o = awaiting_delivery(Open, false).unwrap();
		assert!(!o.settled && o.awaiting_delivery);
		assert_eq!(next_state(Open, 1000, &o), Some(InProgress));
		assert_eq!(awaiting_delivery(InProgress, true), None);
		// In progress for an on-chain payment: the Ark payment is still
		// recorded, without changing the sale.
		let o = awaiting_delivery(InProgress, false).unwrap();
		assert!(o.awaiting_delivery);
		assert_eq!(next_state(InProgress, 1000, &o), None);
	}

	/// A synthetic sequence: an unconfirmed Bitcoin payment puts the sale in
	/// progress, barkd then shows an Ark payment awaiting delivery, and the
	/// Bitcoin payment goes away. The sale stays in progress for the Ark
	/// payment, and barkd's settlement pays it.
	#[test]
	fn a_pending_ark_payment_survives_a_lost_bitcoin_payment() {
		use SaleState::*;
		use crate::stages::{Summary, next_onchain_state};
		let a = Amount::from_sat(1000);
		let seen = Summary { seen: a, ..Summary::default() };
		let t = next_onchain_state(Open, a, false, false, false, &seen);
		assert_eq!(t.state, Some(InProgress));
		// barkd: awaiting delivery. Recorded; the sale stays as it is.
		let o = awaiting_delivery(InProgress, false).unwrap();
		assert_eq!(next_state(InProgress, 1000, &o), None);
		// The Bitcoin payment is replaced, after the sale's expiry.
		let gone = Summary { replaced: true, ..Summary::default() };
		let t = next_onchain_state(InProgress, a, true, true, true, &gone);
		assert_eq!((t.state, t.ark_progress), (Some(InProgress), true));
		// barkd settles: paid.
		let settled = Observation::invoice(a, true, false, Some(true));
		assert_eq!(next_state(InProgress, 1000, &settled), Some(Paid));
	}

	/// An Ark-only sale past its expiry: the first barkd read fails, the
	/// second, while canceling the invoice, shows the payment awaiting
	/// delivery. That positive answer never expires the sale, even past
	/// the grace; only no answer, or an unknown state, does after it.
	#[test]
	fn a_positive_pending_answer_never_expires_a_sale() {
		let unavailable = || Err(CancelError::Unavailable(anyhow::anyhow!("barkd is down")));
		let arriving = || Err(CancelError::PaymentArriving(ForwardStatus::AwaitingDelivery));
		for seen in [false, true] {
			assert!(!expires_now(&arriving(), false, seen));
			assert!(!expires_now(&arriving(), true, seen));
			assert!(!expires_now(&unavailable(), false, seen));
			assert!(!expires_now(&Err(CancelError::UnknownState), false, seen));
			assert!(expires_now(&Ok(()), false, seen), "barkd says it ended unpaid");
		}
		assert!(expires_now(&unavailable(), true, false));
		assert!(expires_now(&Err(CancelError::UnknownState), true, false));
		// A payment seen before, then a failed read or a restart: the sale
		// never expires on that.
		assert!(!expires_now(&unavailable(), true, true));
		assert!(!expires_now(&Err(CancelError::UnknownState), true, true));
		// The next pass reads barkd's answer: in progress, and recorded.
		let o = awaiting_delivery(SaleState::Open, false).unwrap();
		assert_eq!(next_state(SaleState::Open, 1000, &o), Some(SaleState::InProgress));
	}
}
