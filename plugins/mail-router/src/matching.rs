//! Invariant: this module is PURE and it is the ONE place §3's fan-out rule is written — every
//! agent whose `routing_refs` intersect the envelope's refs, in NAME order. Never "the best
//! match"; never one winner. A router that picks a winner strands the true owner of an event, and
//! that failure is invisible until someone notices work that never happened.

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, AgentRow, Ref};

/// Every matching agent, name-ordered and deduplicated. An agent with empty `routing_refs`
/// matches nothing (an empty intersection is empty, and a lane that asked for nothing gets
/// nothing).
pub fn recipients(refs: &BTreeSet<Ref>, rows: &[AgentRow]) -> Vec<AgentName> {
    let mut out: BTreeSet<AgentName> = BTreeSet::new();
    for row in rows {
        if row.routing_refs.iter().any(|r| refs.contains(r)) {
            out.insert(row.name.clone());
        }
    }
    // A `BTreeSet<AgentName>` is already name-ordered and deduplicated; the collect makes the
    // ordering guarantee of the signature a fact rather than a comment.
    out.into_iter().collect()
}

/// The wake classes an envelope carries: its refs in the `class:` namespace, stripped of the
/// prefix (P5-D3). A ref outside the namespace is not a class and is ignored here.
pub fn wake_classes_of(refs: &BTreeSet<Ref>) -> BTreeSet<String> {
    refs.iter()
        .filter_map(|r| r.as_str().strip_prefix(CLASS_NAMESPACE))
        .filter(|c| !c.is_empty())
        .map(|c| c.to_string())
        .collect()
}

/// The namespace P5-D3 spells a wake class in.
pub const CLASS_NAMESPACE: &str = "class:";

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::TrajId;

    fn row(name: &str, refs: &[&str]) -> AgentRow {
        AgentRow {
            name: AgentName::new(name),
            traj: TrajId::new(format!("t-{name}")),
            routing_refs: refs.iter().map(Ref::new).collect(),
            wake_classes: BTreeSet::new(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        }
    }

    fn refs(items: &[&str]) -> BTreeSet<Ref> {
        items.iter().map(Ref::new).collect()
    }

    fn names(items: &[&str]) -> Vec<AgentName> {
        items.iter().map(AgentName::new).collect()
    }

    /// §3's fan-out rule, and the reason this module exists at all.
    #[test]
    fn every_matching_agent_is_returned_not_the_best_one() {
        let rows = [
            row("ci", &["gh:bough/bough#12", "repo:bough"]),
            row("infra", &["repo:bough"]),
            row("docs", &["repo:wiki"]),
        ];
        // `infra` matches on one ref, `ci` on two. A "best match" router would return `ci` alone.
        assert_eq!(
            recipients(&refs(&["repo:bough", "gh:bough/bough#12"]), &rows),
            names(&["ci", "infra"])
        );
    }

    #[test]
    fn a_partial_ref_overlap_matches() {
        let rows = [row("ci", &["repo:bough", "repo:wiki"])];
        // One ref in common out of two on each side is a match: the rule is intersection, not
        // containment.
        assert_eq!(
            recipients(&refs(&["repo:bough", "linear:ENG-1"]), &rows),
            names(&["ci"])
        );
    }

    #[test]
    fn an_agent_with_no_routing_refs_matches_nothing() {
        let rows = [row("fresh", &[])];
        assert!(recipients(&refs(&["repo:bough"]), &rows).is_empty());
        // Not even an empty envelope reaches it: an empty intersection is empty on both sides.
        assert!(recipients(&refs(&[]), &rows).is_empty());
    }

    #[test]
    fn recipients_are_name_ordered_and_deduplicated() {
        let rows = [
            row("zeta", &["repo:bough"]),
            row("alpha", &["repo:bough", "repo:bough"]),
            // The same row name twice is exactly what a mid-merge read can hand us.
            row("alpha", &["repo:bough"]),
            row("mid", &["repo:bough"]),
        ];
        assert_eq!(
            recipients(&refs(&["repo:bough"]), &rows),
            names(&["alpha", "mid", "zeta"])
        );
    }

    #[test]
    fn wake_classes_of_reads_the_class_namespace_only() {
        let carried = wake_classes_of(&refs(&[
            "class:ask",
            "class:ci-red",
            "repo:bough",
            "gh:bough/bough#12",
            // A bare `class:` names no class and must not become the empty class.
            "class:",
        ]));
        assert_eq!(
            carried,
            BTreeSet::from(["ask".to_string(), "ci-red".to_string()])
        );
    }
}
