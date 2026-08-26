//! Invariant: `connected(agent)` is `own_chain ∪ ancestry ∪ ref_matches`, computed AT NEED and
//! WRITING NOTHING (§3). A ref linked late therefore includes its history retroactively, with
//! nothing written onto the entries themselves (V6).

use bough_plugin_ledger::{AgentName, Connected, LedgerError};

use crate::store::SqliteStore;

/// Three indexed queries and no writes.
pub async fn connected(store: &SqliteStore, agent: &AgentName) -> Result<Connected, LedgerError> {
    todo!("WP-2: connected::connected")
}
