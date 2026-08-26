//! Invariant: FTS5 is external-content over `steps` with an insert-only trigger, and hits are
//! ordered `seq DESC, traj ASC` — NOT bm25 rank, which the memory provider cannot reproduce
//! (P1-D19). The conformance suite's whole value is that the two providers answer identically.

use bough_plugin_ledger::{LedgerError, SearchHit, SearchQuery};

use crate::store::SqliteStore;

/// [`bough_plugin_ledger::LedgerStore::search`].
pub async fn search(store: &SqliteStore, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError> {
    todo!("WP-2: search::search")
}
