//! Invariant: this module is PURE, and it is the reason ambiguity can NEVER be guessed. A ref
//! claimed by two children, or a ref of the parent claimed by none while the parent is being
//! absorbed, is AMBIGUOUS — never resolved by order, by name, or by "most specific". Breaking a
//! tie silently is how mail ends up in the wrong lane with nobody able to say when it started.

use std::collections::{BTreeMap, BTreeSet};

use bough_plugin_ledger::{AgentName, AgentRow, Ref};

use crate::plan::ChildSpec;

/// The routing a plan assigns.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutingPlan {
    /// Per new/surviving row: the refs it ends up with.
    pub assign: Vec<(AgentName, BTreeSet<Ref>)>,
    /// Refs the parent keeps.
    pub keep: BTreeSet<Ref>,
}

impl RoutingPlan {
    /// The refs planned for one name, or the empty set.
    pub fn refs_of(&self, name: &AgentName) -> BTreeSet<Ref> {
        self.assign
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, r)| r.clone())
            .unwrap_or_default()
    }
}

/// Settled, or a list of refs nobody may settle but Andrey.
#[derive(Clone, Debug, PartialEq)]
pub enum RoutingVerdict {
    Settled(RoutingPlan),
    Ambiguous(Vec<Ambiguity>),
}

/// One ref two children both claimed.
#[derive(Clone, Debug, PartialEq)]
pub struct Ambiguity {
    pub r#ref: Ref,
    pub claimed_by: Vec<AgentName>,
}

/// PURE. See the module invariant.
pub fn plan_split(parent: &BTreeSet<Ref>, children: &[ChildSpec]) -> RoutingVerdict {
    // Who claims what. A HEADLESS child (`agent: None`) has no row to route to, so its claims are
    // not claims at all — §4's fork takes no routing with it.
    let mut claims: BTreeMap<Ref, Vec<AgentName>> = BTreeMap::new();
    for c in children {
        let Some(name) = &c.agent else { continue };
        for r in &c.routing_refs {
            claims.entry(r.clone()).or_default().push(name.clone());
        }
    }

    let ambiguous: Vec<Ambiguity> = claims
        .iter()
        .filter(|(_, by)| by.len() > 1)
        .map(|(r, by)| Ambiguity {
            r#ref: r.clone(),
            claimed_by: by.clone(),
        })
        .collect();
    if !ambiguous.is_empty() {
        // NOT "the first claimant wins", NOT "the longest ref wins". The whole verdict fails so
        // the op refuses and Andrey settles it (§4).
        return RoutingVerdict::Ambiguous(ambiguous);
    }

    let assign: Vec<(AgentName, BTreeSet<Ref>)> = children
        .iter()
        .filter_map(|c| c.agent.clone().map(|n| (n, c.routing_refs.clone())))
        .collect();
    // Everything a child took is gone from the parent; everything nobody claimed STAYS with it.
    // A parent that keeps a ref is a parent that keeps receiving that mail — never a black hole.
    let taken: BTreeSet<Ref> = claims.keys().cloned().collect();
    let keep: BTreeSet<Ref> = parent.difference(&taken).cloned().collect();
    RoutingVerdict::Settled(RoutingPlan { assign, keep })
}

/// PURE. Merge takes the UNION, always (§3); `model_override` and `tick_floor` resolve from the
/// SURVIVOR by rule, so a merge's routing verdict is total and never ambiguous.
pub fn plan_merge(survivor: &AgentRow, absorbed: &AgentRow) -> RoutingPlan {
    let mut refs = survivor.routing_refs.clone();
    refs.extend(absorbed.routing_refs.iter().cloned());
    RoutingPlan {
        assign: vec![(survivor.name.clone(), refs)],
        // Nothing is left behind: the absorbed ROW goes and its refs move, so there is no third
        // party to keep anything.
        keep: BTreeSet::new(),
    }
}

/// PURE: the surviving ROW a merge writes. The union of the refs and the wake classes; the
/// SURVIVOR's `model_override` and `tick_floor`, never a blend and never the absorbed one's.
pub fn merged_row(survivor: &AgentRow, absorbed: &AgentRow) -> AgentRow {
    let mut wake_classes = survivor.wake_classes.clone();
    wake_classes.extend(absorbed.wake_classes.iter().cloned());
    AgentRow {
        name: survivor.name.clone(),
        traj: survivor.traj.clone(),
        routing_refs: plan_merge(survivor, absorbed).refs_of(&survivor.name),
        wake_classes,
        model_override: survivor.model_override.clone(),
        tick_floor: survivor.tick_floor,
        digest_rollup: survivor.digest_rollup.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::TrajId;

    fn refs(v: &[&str]) -> BTreeSet<Ref> {
        v.iter().map(|s| Ref::new(*s)).collect()
    }

    fn child(name: &str, rs: &[&str]) -> ChildSpec {
        ChildSpec {
            agent: Some(AgentName::new(name)),
            traj: TrajId::new(format!("lane/{name}")),
            routing_refs: refs(rs),
            wake_classes: Default::default(),
        }
    }

    fn row(name: &str, rs: &[&str]) -> AgentRow {
        AgentRow {
            name: AgentName::new(name),
            traj: TrajId::new(format!("lane/{name}")),
            routing_refs: refs(rs),
            wake_classes: ["ask".to_string()].into_iter().collect(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        }
    }

    #[test]
    fn a_ref_claimed_by_one_child_is_assigned() {
        let v = plan_split(
            &refs(&["gh:o/r", "slack:c1"]),
            &[child("a", &["gh:o/r"]), child("b", &["slack:c1"])],
        );
        let RoutingVerdict::Settled(p) = v else {
            panic!("two disjoint claims settle")
        };
        assert_eq!(p.refs_of(&AgentName::new("a")), refs(&["gh:o/r"]));
        assert_eq!(p.refs_of(&AgentName::new("b")), refs(&["slack:c1"]));
        assert!(p.keep.is_empty());
    }

    #[test]
    fn a_ref_claimed_by_two_children_is_ambiguous() {
        let v = plan_split(
            &refs(&["gh:o/r"]),
            &[child("a", &["gh:o/r"]), child("b", &["gh:o/r"])],
        );
        let RoutingVerdict::Ambiguous(a) = v else {
            panic!("a contested ref is never settled")
        };
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].r#ref, Ref::new("gh:o/r"));
        assert_eq!(
            a[0].claimed_by,
            vec![AgentName::new("a"), AgentName::new("b")]
        );
    }

    #[test]
    fn a_parent_ref_claimed_by_nobody_stays_with_the_parent() {
        let v = plan_split(
            &refs(&["gh:o/r", "slack:c1", "mail:ops"]),
            &[child("a", &["gh:o/r"]), child("b", &["slack:c1"])],
        );
        let RoutingVerdict::Settled(p) = v else {
            panic!("settles")
        };
        assert_eq!(p.keep, refs(&["mail:ops"]));
    }

    #[test]
    fn merge_unions_the_refs() {
        let p = plan_merge(&row("sol", &["gh:o/r"]), &row("terra", &["slack:c1"]));
        assert_eq!(
            p.refs_of(&AgentName::new("sol")),
            refs(&["gh:o/r", "slack:c1"])
        );
        assert!(p.keep.is_empty());
        // Idempotent over an overlap: a ref both lanes carried is one ref, not two.
        let p = plan_merge(&row("sol", &["gh:o/r"]), &row("terra", &["gh:o/r"]));
        assert_eq!(p.refs_of(&AgentName::new("sol")), refs(&["gh:o/r"]));
    }

    #[test]
    fn merge_takes_overrides_from_the_survivor() {
        let mut s = row("sol", &["gh:o/r"]);
        s.model_override = Some("claude-haiku-4-5-20251001".into());
        s.tick_floor = Some(std::time::Duration::from_secs(60));
        let mut a = row("terra", &["slack:c1"]);
        a.model_override = Some("some-other-model".into());
        a.tick_floor = Some(std::time::Duration::from_secs(5));
        a.wake_classes = ["deploy".to_string()].into_iter().collect();

        let merged = merged_row(&s, &a);
        assert_eq!(
            merged.model_override.as_deref(),
            Some("claude-haiku-4-5-20251001")
        );
        assert_eq!(merged.tick_floor, Some(std::time::Duration::from_secs(60)));
        assert_eq!(merged.routing_refs, refs(&["gh:o/r", "slack:c1"]));
        // Wake classes UNION: an urgency either lane honoured is an urgency the survivor honours.
        assert!(merged.wake_classes.contains("ask") && merged.wake_classes.contains("deploy"));
    }

    #[test]
    fn the_planner_never_breaks_a_tie_by_name_or_order() {
        let contested = refs(&["gh:o/r"]);
        let a = child("a", &["gh:o/r"]);
        let z = child("z", &["gh:o/r"]);
        // Neither order nor alphabet decides: BOTH orderings refuse, identically.
        let one = plan_split(&contested, &[a.clone(), z.clone()]);
        let two = plan_split(&contested, &[z, a]);
        let (RoutingVerdict::Ambiguous(mut x), RoutingVerdict::Ambiguous(mut y)) = (one, two)
        else {
            panic!("a tie is refused whichever way round it is handed over")
        };
        for v in [&mut x, &mut y] {
            v[0].claimed_by.sort();
        }
        assert_eq!(x, y);
    }

    /// A headless child (§4's fork) takes NO routing, so its refs cannot contest anything.
    #[test]
    fn a_headless_child_claims_nothing() {
        let mut fork = child("ignored", &["gh:o/r"]);
        fork.agent = None;
        let v = plan_split(&refs(&["gh:o/r"]), &[child("a", &["gh:o/r"]), fork]);
        let RoutingVerdict::Settled(p) = v else {
            panic!("a fork's spec cannot make a tie")
        };
        assert_eq!(p.assign.len(), 1);
        assert_eq!(p.refs_of(&AgentName::new("a")), refs(&["gh:o/r"]));
    }
}
