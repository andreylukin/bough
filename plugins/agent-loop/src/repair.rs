//! Invariant (§5): crash repair reads and writes STEPS ONLY. Rollups are never touched — a
//! half-finished wake is a gap in the chain, not a reason to re-derive anything above it.

use bough_plugin_ledger::{Step, StepId, TrajId};
use chrono::{DateTime, Utc};

/// What repair decided to append for one trajectory. Pure, so the whole of V9 is testable
/// without a store: the caller does the appending.
#[derive(Clone, Debug, PartialEq)]
pub struct Repair {
    pub traj: TrajId,
    /// One `tool/result { outcome: unknown }` per `tool/call` the orphaned wake never answered.
    pub unknown_results: Vec<StepId>,
    /// Whether a `wake/end { reason: interrupted, consumed: [] }` is owed.
    pub close_wake: bool,
}

/// Decide the repair for one trajectory's tail. Pure. WP-4.
pub fn plan(_tail: &[Step], _now: DateTime<Utc>) -> Option<Repair> {
    todo!("WP-4: an orphaned trailing wake, and its unanswered calls")
}
