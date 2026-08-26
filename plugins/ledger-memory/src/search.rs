//! Invariant: search answers IDENTICALLY to ledger-sqlite for the queries the conformance suite
//! uses — a case-insensitive token match over body + cites, ordered `seq DESC, traj ASC`
//! (P1-D19). That agreement, not FTS parity in general, is what Phase 1 needs.

use bough_plugin_ledger::{LedgerError, SearchHit, SearchQuery};

use crate::store::MemoryStore;

/// [`bough_plugin_ledger::LedgerStore::search`].
pub fn search(store: &MemoryStore, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError> {
    todo!("WP-3: search::search")
}
