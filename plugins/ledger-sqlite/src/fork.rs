//! Invariant: a fork's prefix must END OUTSIDE an open wake (§3). A prefix that lands inside one
//! is REFUSED, naming the wake and the seq it opened at — never silently clipped — and a refused
//! fork writes nothing at all. A successful fork writes the edge and the child's `fork/end-seed`
//! marker at seq 1 in ONE transaction.

use bough_plugin_ledger::{Fork, ForkOutcome, LedgerError, Seq, WakeId};

use crate::store::SqliteStore;

/// The whole fork path.
pub async fn fork(store: &SqliteStore, req: Fork) -> Result<ForkOutcome, LedgerError> {
    todo!("WP-2: fork::fork")
}

/// Scan the parent's `wake/*` markers up to and including `at_seq`; `Some` names the wake still
/// open there. A pure function of the scanned markers, so the rule is testable without a store.
pub fn open_wake_at(markers: &[(Seq, WakeId, bool)], at_seq: Seq) -> Option<(WakeId, Seq)> {
    todo!("WP-2: fork::open_wake_at")
}
