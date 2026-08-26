//! Invariant: validate BEFORE the transaction, write `steps` + `step_refs` (derived, never
//! caller-supplied) in ONE commit, and emit `ledger/step` only AFTER the commit returns — the
//! event is durable, so the row is readable when a listener sees it (§0.2, V7).

use bough_plugin_ledger::{Append, LedgerError, Step};

use crate::store::SqliteStore;

/// The whole append path for one step.
pub async fn append(store: &SqliteStore, req: Append) -> Result<Step, LedgerError> {
    todo!("WP-2: append::append")
}

/// One transaction, one contiguous seq run, one `ledger/step` per step, in seq order.
pub async fn append_batch(
    store: &SqliteStore,
    reqs: Vec<Append>,
) -> Result<Vec<Step>, LedgerError> {
    todo!("WP-2: append::append_batch")
}
