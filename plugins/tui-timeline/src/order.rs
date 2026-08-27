//! Invariant: THE timeline is a pure function of `(rows, filter, limit)`. No clock, no ledger, no
//! I/O — which is what makes "a pure function of the ledger stream" a property a test can hold
//! (§17 Phase 8's V2).

use crate::filter::Filter;
use crate::Row;

/// PURE — **the** timeline.
///
/// A total order over rows from any number of trajectories: `step.at` ascending, ties broken by
/// `(traj, seq)`. Filtered, then truncated to the NEWEST `limit` rows, then returned OLDEST-first.
/// Shuffling the input cannot change the output.
///
/// WP-2.
pub fn timeline(rows: &[Row], f: &Filter, limit: usize) -> Vec<Row> {
    let _ = (rows, f, limit);
    todo!("WP-2: filter, total-order by (at, traj, seq), keep the newest `limit`, oldest-first")
}
