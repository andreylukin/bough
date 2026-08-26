//! Invariant (§5): mail consumption is the UNION of the `wake/end` consumed sets, order
//! independent; a drain wake consumes only ORDINARY seqs and an answer wake only its trigger;
//! ONE drain wake is in flight per agent; and at every `wake_end`, unconsumed ordinary mail
//! implies a scheduled drain wake. That last one is the STANDING invariant — it is checked after
//! every wake, not only in a test.
//!
//! P2-D16: urgency and drain scheduling are modules here rather than a `wake-scheduler` row.
//! §5 names one, but there is no second implementation in sight and §0.2 forbids splitting
//! preemptively. Flagged for the §15-item-6 review at phase close.

use bough_plugin_agents::{ClaimSelector, Message, Target};
use bough_plugin_ledger::{SeqRange, Step};
use bough_plugin_llm::WakeKind;

/// How urgently a message wants a wake (§5's two urgencies).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Urgency {
    /// An Andrey message or wake-class mail: wake NOW.
    Immediate,
    /// Ordinary mail: a debounced drain wake, unless another wake drains it first.
    Coalesced,
}

/// Decide the urgency of one message. Pure. WP-4.
pub fn urgency_of(_msg: &Message, _target: Target, _wake_flag: bool) -> Urgency {
    todo!("WP-4: Andrey or wake-class => Immediate, else Coalesced")
}

/// The claim selector for a wake of this kind. Pure. WP-4.
pub fn selector_for(_kind: WakeKind, _target: Target) -> ClaimSelector {
    todo!("WP-4: a drain claims ordinary seqs; an answer wake claims its trigger only")
}

/// The union of consumed sets across `wake/end` steps, order independent (§5). Pure. WP-4.
pub fn consumed_union(_wake_ends: &[Step]) -> Vec<SeqRange> {
    todo!("WP-4: SeqRange::union over every wake/end body")
}

/// Delivered mail not covered by the consumed union. Pure. WP-4.
pub fn unconsumed(_delivered: &[Step], _consumed: &[SeqRange]) -> Vec<Step> {
    todo!("WP-4: the complement, in seq order")
}

/// The standing invariant, as a pure predicate: given the unconsumed ordinary mail and whether a
/// drain wake is scheduled, is the loop in a legal state?
///
/// WP-4.
pub fn standing_invariant_holds(_unconsumed_ordinary: usize, _drain_scheduled: bool) -> bool {
    todo!("WP-4: unconsumed ordinary mail => a drain wake is scheduled")
}
