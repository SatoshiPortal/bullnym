//! Watches the payment rails of recent sales and the invoices of the
//! Lightning Addresses, and moves them to their next state.
//!
//! A sale is paid by the first rail that delivers its full amount. The
//! watcher keeps checking the other rails until they expire, so a second
//! payment is recorded as excess instead of being missed.

use std::str::FromStr;
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{Address, Amount};
use tokio_postgres::GenericClient;

use crate::App;
use crate::database::{Rail, SaleState, WatchedPayment};
use crate::rails::forward::ForwardStatus;


/// How long an invoice payment is watched after its sale or invoice
/// expires. A payment made just before expiry can take a little while to
/// deliver.
const DELIVERY_GRACE_SECS: f64 = 600.0;

/// On-chain addresses are checked every this many ticks.
const ONCHAIN_EVERY_TICKS: u64 = 5;

/// What a check found for one payment rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
	/// Delivered to the owner. For on-chain, confirmed.
	pub received: Amount,
	/// Seen but not delivered. Only on-chain reports this.
	pub pending: Amount,
	/// Whether the rail is done: delivered, or certain never to pay.
	pub settled: bool,
	/// An Ark transfer paid the invoice and is not delivered yet.
	pub awaiting_delivery: bool,
	/// The invoice was paid by an Ark transfer rather than over Lightning.
	pub via_ark: bool,
	pub txids: Option<Vec<String>>,
}

impl Observation {
	pub fn invoice(received: Amount, settled: bool, awaiting_delivery: bool, via_ark: bool) -> Observation {
		Observation {
			received, pending: Amount::ZERO, settled, awaiting_delivery, via_ark, txids: None,
		}
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
		if p.rail == Rail::Onchain && !check_onchain {
			continue;
		}
		let obs = match observe(app, &p).await {
			Ok(Some(obs)) => obs,
			Ok(None) => continue,
			Err(e) => {
				tracing::warn!("check of sale {} ({}) failed: {:#}", p.sale_public_id, p.rail.as_str(), e);
				continue;
			},
		};
		changed |= record(app, &p, &obs).await?;
	}
	changed |= expire_sales(app).await?;
	if let Err(e) = watch_lnurl_invoices(app).await {
		tracing::warn!("Lightning Address invoice pass failed: {:#}", e);
	}
	if changed {
		app.notify_sale_change();
	}
	Ok(())
}

async fn watched_payments(app: &App) -> anyhow::Result<Vec<WatchedPayment>> {
	let conn = app.db.conn().await?;
	let rows = conn.query("
		SELECT p.id AS payment_id, s.id AS sale_id, s.public_id, s.state, s.amount_sat,
			s.expires_at, p.rail, p.payment_hash, p.onchain_address, p.received_sat,
			p.pending_sat, p.settled_at
		FROM sale_payment p
		JOIN sale s ON s.id = p.sale_id
		WHERE p.settled_at IS NULL AND (
			(p.rail <> 'onchain' AND s.state <> 'canceled' AND (
				s.expires_at > NOW() - make_interval(secs => $1) OR s.state = 'in-progress'
			))
			OR (p.rail = 'onchain' AND s.created_at > NOW() - make_interval(secs => $2))
		)
		ORDER BY p.id
	", &[&DELIVERY_GRACE_SECS, &(app.cfg.sales.onchain_watch_secs as f64)]).await?;
	Ok(rows.iter().map(WatchedPayment::from_row).collect())
}

/// Checks one rail. `None` means nothing new.
async fn observe(app: &App, p: &WatchedPayment) -> anyhow::Result<Option<Observation>> {
	match p.rail {
		Rail::Lightning | Rail::Ark => {
			let hash = p.payment_hash.as_deref().context("invoice payment without hash")?;
			Ok(match app.forwarder.status(hash).await? {
				ForwardStatus::Unpaid | ForwardStatus::Arriving | ForwardStatus::Unknown => None,
				// Settled by the Ark server, not yet in the owner's mailbox.
				ForwardStatus::AwaitingDelivery if p.sale_state == SaleState::InProgress => None,
				ForwardStatus::AwaitingDelivery => {
					Some(Observation::invoice(Amount::ZERO, false, true, true))
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
		Rail::Onchain => {
			let address = p.onchain_address.as_deref().context("on-chain payment without address")?;
			let address = Address::from_str(address)?.require_network(app.cfg.network)?;
			let funds = app.chain.funds(&address).await?;
			let obs = Observation {
				received: funds.confirmed,
				pending: funds.pending,
				// An address stays watched for late or extra payments until
				// its watch window ends.
				settled: false,
				awaiting_delivery: false,
				via_ark: false,
				txids: Some(funds.txids),
			};
			let unchanged = obs.received.to_sat() as i64 == p.received_sat
				&& obs.pending.to_sat() as i64 == p.pending_sat;
			Ok(if unchanged { None } else { Some(obs) })
		},
	}
}

/// The state a sale moves to after an observation, if it changes.
pub fn next_state(
	state: SaleState,
	amount_sat: i64,
	rail: Rail,
	obs: &Observation,
	expired: bool,
) -> Option<SaleState> {
	if !state.is_live() {
		return None;
	}
	let received = obs.received.to_sat() as i64;
	let pending = obs.pending.to_sat() as i64;
	let delivered = match rail {
		Rail::Onchain => received >= amount_sat,
		Rail::Lightning | Rail::Ark => obs.settled && received >= amount_sat,
	};
	if delivered {
		return Some(SaleState::Paid);
	}
	// An Ark payment the server settled but has not delivered: the payer
	// cannot take it back, but it is not the owner's yet.
	if obs.awaiting_delivery && state == SaleState::Open {
		return Some(SaleState::InProgress);
	}
	if rail == Rail::Onchain {
		if received + pending >= amount_sat {
			if state == SaleState::Open {
				return Some(SaleState::InProgress);
			}
		} else if state == SaleState::InProgress {
			// The mempool payment went away.
			return Some(if expired { SaleState::Expired } else { SaleState::Open });
		}
	}
	None
}

/// Stores an observation and moves the sale on. Returns whether a sale
/// changed state.
async fn record(app: &App, p: &WatchedPayment, obs: &Observation) -> anyhow::Result<bool> {
	let mut conn = app.db.conn().await?;
	let tx = conn.transaction().await?;

	// Lock the sale, so two payments cannot both mark it paid.
	let row = tx.query_one(
		"SELECT state, amount_sat, expires_at < NOW() AS expired FROM sale WHERE id = $1 FOR UPDATE",
		&[&p.sale_id],
	).await?;
	let state: SaleState = row.get("state");
	let amount_sat: i64 = row.get("amount_sat");
	let expired: bool = row.get("expired");

	tx.execute("
		UPDATE sale_payment SET received_sat = $2, pending_sat = $3,
			settled_at = CASE WHEN $4 THEN NOW() END,
			txids = coalesce($5, txids)
		WHERE id = $1
	", &[
		&p.payment_id, &(obs.received.to_sat() as i64), &(obs.pending.to_sat() as i64),
		&obs.settled, &obs.txids,
	]).await?;
	tx.execute("
		UPDATE sale SET received_sat = (
			SELECT coalesce(sum(received_sat), 0) FROM sale_payment WHERE sale_id = $1
		), updated_at = NOW() WHERE id = $1
	", &[&p.sale_id]).await?;

	let next = next_state(state, amount_sat, p.rail, obs, expired);
	if let Some(next) = next {
		let paid = next == SaleState::Paid;
		tx.execute("
			UPDATE sale SET state = $2,
				paid_rail = CASE WHEN $3 THEN $4 ELSE paid_rail END,
				paid_at = CASE WHEN $3 THEN NOW() ELSE paid_at END
			WHERE id = $1
		", &[&p.sale_id, &next, &paid, &obs.paid_rail(p.rail)]).await?;
	}
	tx.commit().await?;

	if next == Some(SaleState::Paid) {
		tracing::info!("sale {} paid by {}", p.sale_public_id, obs.paid_rail(p.rail).as_str());
		if p.rail == Rail::Onchain {
			sale_ended(app, p.sale_id).await;
		}
	}
	Ok(next.is_some())
}

/// The payment hash of a sale's invoice, while it is not settled.
pub async fn invoice_hash(app: &App, sale_id: i64) -> anyhow::Result<Option<String>> {
	let conn = app.db.conn().await?;
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
pub async fn cancel_invoice(app: &App, hash: &str) -> Result<(), CancelError> {
	match app.forwarder.status(hash).await.map_err(CancelError::Unavailable)? {
		ForwardStatus::Gone => Ok(()),
		ForwardStatus::Unpaid => match app.forwarder.cancel(hash).await {
			Ok(()) => Ok(()),
			// barkd refuses once it revealed the preimage. Ask again what
			// it holds.
			Err(e) => match app.forwarder.status(hash).await {
				Ok(ForwardStatus::Gone) => Ok(()),
				Ok(ForwardStatus::Unpaid) | Err(_) => Err(CancelError::Unavailable(e)),
				Ok(_) => Err(CancelError::PaymentArriving),
			},
		},
		ForwardStatus::Arriving | ForwardStatus::AwaitingDelivery | ForwardStatus::Delivered(_)
			| ForwardStatus::Unknown => Err(CancelError::PaymentArriving),
	}
}

/// Ends the open invoice of a sale that another rail paid, or that was
/// canceled or expired. It is best effort: a payment already in flight
/// still completes and counts as excess.
pub async fn sale_ended(app: &App, sale_id: i64) {
	match invoice_hash(app, sale_id).await {
		Ok(Some(hash)) => {
			if let Err(e) = cancel_invoice(app, &hash).await {
				tracing::info!("invoice {} stays open: {}", hash, e);
			}
		},
		Ok(None) => {},
		Err(e) => tracing::warn!("failed to load the invoice of sale {}: {:#}", sale_id, e),
	}
}

/// Expires open sales past their expiry, and cancels their invoices in
/// barkd. A sale whose invoice payment is arriving stays open for it, up
/// to the delivery grace. An `in-progress` sale is never expired here: it
/// stays open for its payment.
async fn expire_sales(app: &App) -> anyhow::Result<bool> {
	let rows = {
		let conn = app.db.conn().await?;
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
			match cancel_invoice(app, &hash).await {
				Ok(()) => {},
				Err(CancelError::PaymentArriving) if !past_grace => continue,
				// The invoice expires at the Ark server at the same time, so
				// no new payment can start.
				Err(e) => tracing::info!("expiring sale {} with its invoice open: {}", sale_id, e),
			}
		}
		let conn = app.db.conn().await?;
		changed |= expire_sale(&*conn, sale_id).await? > 0;
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
		let conn = app.db.conn().await?;
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
		let status = match app.forwarder.status(&hash).await {
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
		let conn = app.db.conn().await?;
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
				match cancel_invoice(app, &hash).await {
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

	fn obs(received: u64, pending: u64, settled: bool) -> Observation {
		Observation {
			received: Amount::from_sat(received),
			pending: Amount::from_sat(pending),
			settled,
			awaiting_delivery: false,
			via_ark: false,
			txids: None,
		}
	}

	#[test]
	fn state_transitions() {
		use SaleState::*;
		// The invoice pays on delivery of the full amount.
		assert_eq!(next_state(Open, 1000, Rail::Lightning, &obs(1000, 0, true), false), Some(Paid));
		assert_eq!(next_state(Open, 1000, Rail::Lightning, &obs(1200, 0, true), false), Some(Paid));
		assert_eq!(next_state(Open, 1000, Rail::Lightning, &obs(0, 0, true), false), None);
		// An Ark payment settled by the server but not delivered is in
		// progress, never paid, and stays so past expiry.
		let awaiting = Observation::invoice(Amount::ZERO, false, true, true);
		assert_eq!(next_state(Open, 1000, Rail::Lightning, &awaiting, false), Some(InProgress));
		assert_eq!(next_state(InProgress, 1000, Rail::Lightning, &awaiting, true), None);
		let delivered = Observation::invoice(Amount::from_sat(1000), true, false, true);
		assert_eq!(next_state(InProgress, 1000, Rail::Lightning, &delivered, true), Some(Paid));
		assert_eq!(delivered.paid_rail(Rail::Lightning), Rail::Ark);
		assert_eq!(obs(1000, 0, true).paid_rail(Rail::Lightning), Rail::Lightning);
		// A payment after the sale ended does not change it: it counts as
		// excess.
		assert_eq!(next_state(Expired, 1000, Rail::Lightning, &obs(1000, 0, true), true), None);
		assert_eq!(next_state(Paid, 1000, Rail::Lightning, &obs(1000, 0, true), false), None);

		// On-chain: mempool, then 1 confirmation.
		assert_eq!(next_state(Open, 1000, Rail::Onchain, &obs(0, 1000, false), false), Some(InProgress));
		assert_eq!(next_state(InProgress, 1000, Rail::Onchain, &obs(0, 1000, false), true), None);
		assert_eq!(next_state(InProgress, 1000, Rail::Onchain, &obs(1000, 0, false), true), Some(Paid));
		// Underpaid on-chain stays open.
		assert_eq!(next_state(Open, 1000, Rail::Onchain, &obs(0, 500, false), false), None);
		// The mempool payment disappears.
		assert_eq!(next_state(InProgress, 1000, Rail::Onchain, &obs(0, 0, false), false), Some(Open));
		assert_eq!(next_state(InProgress, 1000, Rail::Onchain, &obs(0, 0, false), true), Some(Expired));
	}
}
