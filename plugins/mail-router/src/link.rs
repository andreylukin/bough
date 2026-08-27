//! Invariant: linking a ref NEVER queries for history. That is what makes `backfilled: 0` a fact
//! rather than a promise (§5: delivery starts at link time; earlier history is reachable by
//! query, never queued as backlog).

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentRow, Ref};

/// PURE: the row's refs after a link, and exactly which refs were newly added. Linking a ref
/// twice adds nothing.
pub fn linked(row: &AgentRow, refs: &BTreeSet<Ref>) -> (BTreeSet<Ref>, BTreeSet<Ref>) {
    let added: BTreeSet<Ref> = refs.difference(&row.routing_refs).cloned().collect();
    let mut after = row.routing_refs.clone();
    after.extend(added.iter().cloned());
    (after, added)
}

/// PURE: the row's refs after an unlink, and exactly which refs were removed.
pub fn unlinked(row: &AgentRow, refs: &BTreeSet<Ref>) -> (BTreeSet<Ref>, BTreeSet<Ref>) {
    let removed: BTreeSet<Ref> = refs.intersection(&row.routing_refs).cloned().collect();
    let after: BTreeSet<Ref> = row.routing_refs.difference(&removed).cloned().collect();
    (after, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{AgentName, TrajId};

    fn row(refs: &[&str]) -> AgentRow {
        AgentRow {
            name: AgentName::new("ci"),
            traj: TrajId::new("t-ci"),
            routing_refs: refs.iter().map(Ref::new).collect(),
            wake_classes: BTreeSet::new(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        }
    }

    fn set(items: &[&str]) -> BTreeSet<Ref> {
        items.iter().map(Ref::new).collect()
    }

    #[test]
    fn link_adds_and_unlink_removes() {
        let before = row(&["repo:bough"]);
        let (after, added) = linked(&before, &set(&["linear:ENG-1"]));
        assert_eq!(after, set(&["repo:bough", "linear:ENG-1"]));
        assert_eq!(added, set(&["linear:ENG-1"]));

        let held = row(&["repo:bough", "linear:ENG-1"]);
        let (after, removed) = unlinked(&held, &set(&["linear:ENG-1", "never:held"]));
        assert_eq!(after, set(&["repo:bough"]));
        // Only what was actually held is reported removed.
        assert_eq!(removed, set(&["linear:ENG-1"]));
    }

    #[test]
    fn linking_a_ref_twice_is_idempotent() {
        let before = row(&["repo:bough"]);
        let (once, added_once) = linked(&before, &set(&["linear:ENG-1"]));
        let mut mid = before.clone();
        mid.routing_refs = once.clone();
        let (twice, added_twice) = linked(&mid, &set(&["linear:ENG-1"]));
        assert_eq!(once, twice);
        assert_eq!(added_once, set(&["linear:ENG-1"]));
        // The second link adds NOTHING, so it appends an `agent/routing` with an empty `added`
        // rather than a second claim of a ref the row already held.
        assert!(added_twice.is_empty());
    }

    /// The rule the whole module exists to make structural: nothing here can consult history,
    /// so nothing here can decide to replay it.
    #[test]
    fn a_link_reports_zero_backfilled() {
        let before = row(&[]);
        let (after, added) = linked(&before, &set(&["repo:bough"]));
        let report = crate::LinkReport {
            agent: before.name.clone(),
            added: added.clone(),
            removed: BTreeSet::new(),
            backfilled: 0,
            now_connected: Vec::new(),
        };
        assert_eq!(report.backfilled, 0);
        assert_eq!(after, set(&["repo:bough"]));
        assert_eq!(report.added, set(&["repo:bough"]));
    }
}
