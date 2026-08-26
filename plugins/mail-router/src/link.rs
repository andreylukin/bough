//! Invariant: linking a ref NEVER queries for history. That is what makes `backfilled: 0` a fact
//! rather than a promise (§5: delivery starts at link time; earlier history is reachable by
//! query, never queued as backlog).

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentRow, Ref};

/// PURE: the row's refs after a link, and exactly which refs were newly added. Linking a ref
/// twice adds nothing.
pub fn linked(_row: &AgentRow, _refs: &BTreeSet<Ref>) -> (BTreeSet<Ref>, BTreeSet<Ref>) {
    todo!("WP-1: union, reporting only the genuinely new refs")
}

/// PURE: the row's refs after an unlink, and exactly which refs were removed.
pub fn unlinked(_row: &AgentRow, _refs: &BTreeSet<Ref>) -> (BTreeSet<Ref>, BTreeSet<Ref>) {
    todo!("WP-1: difference, reporting only the refs that were actually held")
}
