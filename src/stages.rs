//! The stages of an on-chain payment, and the rules that keep a payment
//! the service has seen but cannot count from passing for paid.
//!
//! | Stage | When | Counts as paid |
//! |---|---|---|
//! | Seen | In the mempool | No: in progress, not guaranteed |
//! | Paid | 1 confirmation | Yes |
//! | Final | 3 confirmations | Yes |
//! | Payment issue | A counted payment disappears in a reorg | Held until resolved |
//!
//! Rules:
//! - An unconfirmed payment never counts: it never sets the received
//!   amount, the paid time or the rail. Only a confirmation does.
//! - A payment counts as gone only on an explicit signal from bitcoind: a
//!   conflict with a confirmed transaction, or two answers in a row that
//!   it is not in the mempool. A timeout, an error or a missing answer
//!   leaves it as it was.
//! - Every unconfirmed payment is treated as replaceable: since Bitcoin
//!   Core 28 nodes relay replacements of any unconfirmed transaction. So
//!   the "not guaranteed" warning never depends on the replaceability
//!   flag.

use bitcoin::Amount;
use serde::{Deserialize, Serialize};

use crate::database::SaleState;


/// What bitcoind says about one transaction paying a sale's address.
#[derive(Debug, Clone, PartialEq)]
pub struct TxObservation {
	pub txid: String,
	/// The amount it pays to the address.
	pub amount: Amount,
	/// Negative when it conflicts with a confirmed transaction.
	pub confirmations: i64,
	pub mempool: Mempool,
	/// Conflicting transactions that do not pay the sale's address.
	pub foreign_conflicts: bool,
}

/// What the mempool says about an unconfirmed transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Mempool {
	/// In the mempool, with its fee rate and whether it spends unconfirmed
	/// outputs.
	In { fee_rate_sat_vb: f64, unconfirmed_parents: bool },
	/// bitcoind answered that it is not in its mempool.
	NotIn,
	/// Not asked, or no answer.
	Unknown,
}

/// The status of a transaction, as the service keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TxStatus {
	/// In the mempool: in progress, not guaranteed.
	Seen,
	/// At least 1 confirmation: it counts.
	Confirmed,
	/// Replaced or dropped, on an explicit signal.
	Gone,
}

impl TxStatus {
	pub fn as_str(&self) -> &'static str {
		match self {
			TxStatus::Seen => "seen",
			TxStatus::Confirmed => "confirmed",
			TxStatus::Gone => "gone",
		}
	}

	pub fn parse(s: &str) -> TxStatus {
		match s {
			"confirmed" => TxStatus::Confirmed,
			"gone" => TxStatus::Gone,
			_ => TxStatus::Seen,
		}
	}
}

/// What the service keeps about a transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct TxRecord {
	pub txid: String,
	pub amount: Amount,
	pub status: TxStatus,
	pub confirmations: i64,
	pub low_fee: bool,
	pub unconfirmed_parents: bool,
	/// Gone because a transaction that does not pay the sale replaced it.
	pub replaced: bool,
	/// How many answers in a row said it is not in the mempool.
	pub absent_count: i32,
}

/// The fee rate below which a payment gets the low-fee warning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeePolicy {
	/// bitcoind's estimate for a confirmation within a few blocks, if it
	/// has one.
	pub estimate_sat_vb: Option<f64>,
	/// A payment below `ratio` times the estimate is low.
	pub ratio: f64,
	/// A payment below this rate is low, whatever the estimate.
	pub floor_sat_vb: f64,
}

impl FeePolicy {
	pub fn is_low(&self, fee_rate_sat_vb: f64) -> bool {
		fee_rate_sat_vb < self.floor_sat_vb
			|| self.estimate_sat_vb.is_some_and(|e| fee_rate_sat_vb < e * self.ratio)
	}
}

/// How many "not in the mempool" answers in a row make a payment gone.
pub const ABSENT_ANSWERS_FOR_GONE: i32 = 2;

/// Updates what the service keeps about a transaction from a new
/// observation.
pub fn classify(prev: Option<&TxRecord>, obs: &TxObservation, fees: &FeePolicy) -> TxRecord {
	let mut rec = prev.cloned().unwrap_or(TxRecord {
		txid: obs.txid.clone(),
		amount: obs.amount,
		status: TxStatus::Seen,
		confirmations: 0,
		low_fee: false,
		unconfirmed_parents: false,
		replaced: false,
		absent_count: 0,
	});
	rec.amount = obs.amount;
	rec.confirmations = obs.confirmations;
	if obs.confirmations >= 1 {
		rec.status = TxStatus::Confirmed;
		rec.absent_count = 0;
		rec.low_fee = false;
		rec.unconfirmed_parents = false;
		rec.replaced = false;
	} else if obs.confirmations < 0 {
		// It conflicts with a confirmed transaction: bitcoind is explicit.
		rec.status = TxStatus::Gone;
		rec.replaced = obs.foreign_conflicts || rec.replaced;
	} else {
		match &obs.mempool {
			Mempool::In { fee_rate_sat_vb, unconfirmed_parents } => {
				rec.status = TxStatus::Seen;
				rec.absent_count = 0;
				rec.low_fee = fees.is_low(*fee_rate_sat_vb);
				rec.unconfirmed_parents = *unconfirmed_parents;
			},
			Mempool::NotIn => {
				rec.absent_count = rec.absent_count.saturating_add(1);
				if rec.absent_count >= ABSENT_ANSWERS_FOR_GONE {
					rec.status = TxStatus::Gone;
					rec.replaced = obs.foreign_conflicts;
				} else if prev.is_none() {
					// First heard of, and already out of the mempool: it has
					// not been seen, so it does not show.
					rec.status = TxStatus::Gone;
					rec.replaced = obs.foreign_conflicts;
				}
				// Otherwise it keeps its status until the next answer.
			},
			// No answer: nothing changes.
			Mempool::Unknown => {
				if prev.is_none() {
					rec.status = TxStatus::Seen;
				}
			},
		}
		// A transaction that had a confirmation and has none now went
		// through a reorg. It no longer counts.
		if rec.status == TxStatus::Confirmed {
			rec.status = TxStatus::Seen;
		}
	}
	rec
}

/// A warning about an in-progress payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Warning {
	/// Always, while a payment is unconfirmed: it is not guaranteed.
	NotGuaranteed,
	/// The fee rate is well below bitcoind's estimate: it may take hours or
	/// drop out of mempools.
	LowFee,
	/// It spends unconfirmed outputs: if a parent is replaced, it goes too.
	UnconfirmedParents,
	/// A transaction that does not pay the sale replaced it.
	Replaced,
}

impl Warning {
	pub fn as_str(&self) -> &'static str {
		match self {
			Warning::NotGuaranteed => "not-guaranteed",
			Warning::LowFee => "low-fee",
			Warning::UnconfirmedParents => "unconfirmed-parents",
			Warning::Replaced => "replaced",
		}
	}
}

/// The on-chain payments of one sale address, summed up.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Summary {
	/// Confirmed: this counts.
	pub confirmed: Amount,
	/// In the mempool: shown, never counted.
	pub seen: Amount,
	/// The fewest confirmations of a confirmed payment.
	pub min_confirmations: Option<i64>,
	pub warnings: Vec<Warning>,
	/// Whether a payment went away because it was replaced.
	pub replaced: bool,
}

pub fn summarize(records: &[TxRecord]) -> Summary {
	let mut s = Summary::default();
	let mut low_fee = false;
	let mut parents = false;
	for r in records {
		match r.status {
			TxStatus::Confirmed => {
				s.confirmed += r.amount;
				s.min_confirmations = Some(s.min_confirmations.map_or(r.confirmations, |m| m.min(r.confirmations)));
			},
			TxStatus::Seen => {
				s.seen += r.amount;
				low_fee |= r.low_fee;
				parents |= r.unconfirmed_parents;
			},
			TxStatus::Gone => s.replaced |= r.replaced,
		}
	}
	if s.seen > Amount::ZERO {
		s.warnings.push(Warning::NotGuaranteed);
		if low_fee {
			s.warnings.push(Warning::LowFee);
		}
		if parents {
			s.warnings.push(Warning::UnconfirmedParents);
		}
	}
	if s.replaced {
		s.warnings.push(Warning::Replaced);
	}
	s
}

/// A notice shown on a sale whose payment went away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Notice {
	/// "Payment replaced": a transaction that does not pay the sale
	/// replaced it.
	PaymentReplaced,
	/// It dropped out of the mempool.
	PaymentDropped,
}

impl Notice {
	pub fn as_str(&self) -> &'static str {
		match self {
			Notice::PaymentReplaced => "payment-replaced",
			Notice::PaymentDropped => "payment-dropped",
		}
	}

	pub fn parse(s: &str) -> Option<Notice> {
		match s {
			"payment-replaced" => Some(Notice::PaymentReplaced),
			"payment-dropped" => Some(Notice::PaymentDropped),
			_ => None,
		}
	}
}

/// What an on-chain summary does to a sale.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transition {
	pub state: Option<SaleState>,
	/// Set the paid time and rail: the first confirmation.
	pub counts_as_paid: bool,
	/// The sale is final: every counted payment has enough confirmations.
	pub is_final: bool,
	pub notice: Option<Notice>,
}

/// The rules of an on-chain payment for a sale.
///
/// - `paid_onchain`: the sale was paid by its address, so a reorg of that
///   payment is a payment issue. A sale paid by its invoice is never
///   affected by its address.
/// - `onchain_progress`: the sale is in progress because of an on-chain
///   payment, not an Ark payment awaiting delivery.
///
/// The total in sight only drops when a transaction is gone, which needs an
/// explicit signal from bitcoind (see [classify]).
pub fn next_onchain_state(
	state: SaleState,
	amount: Amount,
	paid_onchain: bool,
	onchain_progress: bool,
	expired: bool,
	summary: &Summary,
	final_confirmations: i64,
) -> Transition {
	let covered = summary.confirmed >= amount;
	let in_sight = summary.confirmed + summary.seen >= amount;
	let deep = summary.min_confirmations.is_some_and(|c| c >= final_confirmations);
	let mut t = Transition::default();
	match state {
		SaleState::Open | SaleState::InProgress if covered => {
			t.state = Some(SaleState::Paid);
			t.counts_as_paid = true;
			t.is_final = deep;
		},
		SaleState::Open if in_sight => t.state = Some(SaleState::InProgress),
		SaleState::InProgress if onchain_progress && !in_sight => {
			t.state = Some(if expired { SaleState::Expired } else { SaleState::Open });
			t.notice = Some(if summary.replaced { Notice::PaymentReplaced } else { Notice::PaymentDropped });
		},
		SaleState::Paid if paid_onchain && !covered => t.state = Some(SaleState::PaymentIssue),
		SaleState::Paid if paid_onchain => t.is_final = deep,
		SaleState::PaymentIssue if covered => {
			t.state = Some(SaleState::Paid);
			t.is_final = deep;
		},
		_ => {},
	}
	t
}

#[cfg(test)]
mod test {
	use super::*;

	use SaleState::*;

	fn fees() -> FeePolicy {
		FeePolicy { estimate_sat_vb: Some(20.0), ratio: 0.5, floor_sat_vb: 1.0 }
	}

	fn obs(conf: i64, mempool: Mempool) -> TxObservation {
		TxObservation {
			txid: "aa".into(), amount: Amount::from_sat(1000), confirmations: conf, mempool,
			foreign_conflicts: false,
		}
	}

	fn in_mempool(rate: f64, parents: bool) -> Mempool {
		Mempool::In { fee_rate_sat_vb: rate, unconfirmed_parents: parents }
	}

	#[test]
	fn low_fee_rule() {
		let f = fees();
		assert!(f.is_low(9.0), "below half the estimate");
		assert!(!f.is_low(10.0));
		let none = FeePolicy { estimate_sat_vb: None, ratio: 0.5, floor_sat_vb: 2.0 };
		assert!(none.is_low(1.5), "below the floor");
		assert!(!none.is_low(3.0), "no estimate, above the floor");
	}

	#[test]
	fn seen_then_confirmed() {
		let r = classify(None, &obs(0, in_mempool(30.0, false)), &fees());
		assert_eq!(r.status, TxStatus::Seen);
		assert!(!r.low_fee && !r.unconfirmed_parents);
		let r = classify(Some(&r), &obs(1, Mempool::Unknown), &fees());
		assert_eq!(r.status, TxStatus::Confirmed);
	}

	#[test]
	fn warnings_from_the_mempool() {
		let r = classify(None, &obs(0, in_mempool(2.0, true)), &fees());
		assert!(r.low_fee && r.unconfirmed_parents);
		let s = summarize(&[r]);
		assert_eq!(s.warnings, vec![Warning::NotGuaranteed, Warning::LowFee, Warning::UnconfirmedParents]);
		assert_eq!(s.seen, Amount::from_sat(1000));
		assert_eq!(s.confirmed, Amount::ZERO, "an unconfirmed payment never counts");
	}

	#[test]
	fn absence_needs_an_explicit_signal() {
		let seen = classify(None, &obs(0, in_mempool(30.0, false)), &fees());
		// No answer from bitcoind: nothing changes.
		let r = classify(Some(&seen), &obs(0, Mempool::Unknown), &fees());
		assert_eq!(r.status, TxStatus::Seen);
		// One "not in mempool" answer is not enough.
		let r = classify(Some(&seen), &obs(0, Mempool::NotIn), &fees());
		assert_eq!(r.status, TxStatus::Seen);
		assert_eq!(r.absent_count, 1);
		// Back in the mempool resets the count.
		let back = classify(Some(&r), &obs(0, in_mempool(30.0, false)), &fees());
		assert_eq!(back.absent_count, 0);
		// Two in a row: gone, dropped.
		let r = classify(Some(&r), &obs(0, Mempool::NotIn), &fees());
		assert_eq!(r.status, TxStatus::Gone);
		assert!(!r.replaced);
		// Replaced by a transaction that does not pay the sale.
		let mut o = obs(0, Mempool::NotIn);
		o.foreign_conflicts = true;
		let r = classify(Some(&classify(Some(&seen), &o, &fees())), &o, &fees());
		assert_eq!(r.status, TxStatus::Gone);
		assert!(r.replaced);
		// A conflict with a confirmed transaction is explicit at once.
		let mut o = obs(-1, Mempool::Unknown);
		o.foreign_conflicts = true;
		let r = classify(Some(&seen), &o, &fees());
		assert_eq!((r.status, r.replaced), (TxStatus::Gone, true));
	}

	#[test]
	fn a_reorg_uncounts_a_confirmed_payment() {
		let c = classify(None, &obs(2, Mempool::Unknown), &fees());
		assert_eq!(c.status, TxStatus::Confirmed);
		let r = classify(Some(&c), &obs(0, in_mempool(30.0, false)), &fees());
		assert_eq!(r.status, TxStatus::Seen);
		let r = classify(Some(&c), &obs(0, Mempool::Unknown), &fees());
		assert_eq!(r.status, TxStatus::Seen, "no confirmation: it no longer counts");
	}

	fn summary(confirmed: u64, seen: u64, min_conf: Option<i64>, replaced: bool) -> Summary {
		Summary {
			confirmed: Amount::from_sat(confirmed), seen: Amount::from_sat(seen),
			min_confirmations: min_conf, warnings: vec![], replaced,
		}
	}

	#[test]
	fn sale_stages() {
		let a = Amount::from_sat(1000);
		// Seen in the mempool: in progress, never paid.
		let t = next_onchain_state(Open, a, false, true, false, &summary(0, 1000, None, false), 3);
		assert_eq!(t.state, Some(InProgress));
		assert!(!t.counts_as_paid);
		// Below the amount: stays open.
		let t = next_onchain_state(Open, a, false, true, false, &summary(0, 999, None, false), 3);
		assert_eq!(t.state, None);
		// An in-progress sale outlives its expiry.
		let t = next_onchain_state(InProgress, a, false, true, true, &summary(0, 1000, None, false), 3);
		assert_eq!(t.state, None);
		// Paid at 1 confirmation, final at 3.
		let t = next_onchain_state(InProgress, a, false, true, true, &summary(1000, 0, Some(1), false), 3);
		assert_eq!((t.state, t.counts_as_paid, t.is_final), (Some(Paid), true, false));
		let t = next_onchain_state(Paid, a, true, true, true, &summary(1000, 0, Some(3), false), 3);
		assert_eq!((t.state, t.is_final), (None, true));
		// A reorg after paid: payment issue, then resolved.
		let t = next_onchain_state(Paid, a, true, true, true, &summary(0, 1000, None, false), 3);
		assert_eq!(t.state, Some(PaymentIssue));
		let t = next_onchain_state(PaymentIssue, a, true, true, true, &summary(1000, 0, Some(1), false), 3);
		assert_eq!(t.state, Some(Paid));
		// A sale paid by its invoice ignores its address.
		let t = next_onchain_state(Paid, a, false, true, true, &summary(0, 0, None, false), 3);
		assert_eq!(t.state, None);
		// Replaced: back to open, or expired, with a notice.
		let t = next_onchain_state(InProgress, a, false, true, false, &summary(0, 0, None, true), 3);
		assert_eq!((t.state, t.notice), (Some(Open), Some(Notice::PaymentReplaced)));
		let t = next_onchain_state(InProgress, a, false, true, true, &summary(0, 0, None, false), 3);
		assert_eq!((t.state, t.notice), (Some(Expired), Some(Notice::PaymentDropped)));
		// An Ark payment awaiting delivery is not undone by the address.
		let t = next_onchain_state(InProgress, a, false, false, false, &summary(0, 0, None, false), 3);
		assert_eq!(t.state, None);
		// Ended sales do not change: a later payment is a late payment.
		for s in [Expired, Canceled] {
			let t = next_onchain_state(s, a, false, true, true, &summary(1000, 0, Some(1), false), 3);
			assert_eq!(t, Transition::default());
		}
	}

	#[test]
	fn replaced_warning_shows() {
		let gone = TxRecord {
			txid: "a".into(), amount: Amount::from_sat(1000), status: TxStatus::Gone, confirmations: 0,
			low_fee: false, unconfirmed_parents: false, replaced: true, absent_count: 2,
		};
		let s = summarize(&[gone]);
		assert_eq!(s.warnings, vec![Warning::Replaced]);
		assert_eq!(s.seen + s.confirmed, Amount::ZERO);
	}
}
