//! Invariant: a row whose type is unknown to this binary is REFUSED on read
//! ([`LedgerError::UnknownStepTypeOnRead`]) unless the row's stored `ignorable` flag is set, in
//! which case it is skipped and COUNTED — a skip nobody can see is indistinguishable from data
//! loss (§3, P1-D7).

use bough_plugin_ledger::{
    LedgerError, Pin, Rollup, RollupQuery, RowHash, Step, StepQuery, TrajId, TrajectoryView,
};

use crate::store::SqliteStore;

/// Materialize one row, applying the unknown-type rule.
pub fn row_to_step(
    store: &SqliteStore,
    row: &rusqlite::Row<'_>,
) -> Result<Option<Step>, LedgerError> {
    todo!("WP-2: read::row_to_step")
}

/// [`bough_plugin_ledger::LedgerStore::steps`].
pub async fn steps(store: &SqliteStore, q: &StepQuery) -> Result<Vec<Step>, LedgerError> {
    todo!("WP-2: read::steps")
}

/// Live pins: every `pin/set` minus every id a later `pin/set.supersedes` or `pin/retire.retires`
/// names. Age is never a criterion (§3).
pub async fn live_pins(store: &SqliteStore, trajs: &[TrajId]) -> Result<Vec<Pin>, LedgerError> {
    todo!("WP-2: read::live_pins")
}

/// Delivered mail not named by any `wake/end.consumed` set. Union, order-independent (§5).
pub async fn unconsumed_mail(store: &SqliteStore, traj: &TrajId) -> Result<Vec<Step>, LedgerError> {
    todo!("WP-2: read::unconsumed_mail")
}

/// [`bough_plugin_ledger::LedgerStore::rollups`].
pub async fn rollups(store: &SqliteStore, q: &RollupQuery) -> Result<Vec<Rollup>, LedgerError> {
    todo!("WP-2: read::rollups")
}

/// Stable per-row content hashes; for rollups the hash EXCLUDES `superseded_by`.
pub async fn row_hashes(
    store: &SqliteStore,
    scope: bough_plugin_ledger::HashScope,
) -> Result<Vec<RowHash>, LedgerError> {
    todo!("WP-2: read::row_hashes")
}

/// A whole trajectory as plain data, for the file view.
pub async fn trajectory_view(
    store: &SqliteStore,
    traj: &TrajId,
) -> Result<TrajectoryView, LedgerError> {
    todo!("WP-2: read::trajectory_view")
}
