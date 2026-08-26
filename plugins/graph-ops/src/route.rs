//! Invariant: this module is PURE, and it is the reason ambiguity can NEVER be guessed. A ref
//! claimed by two children, or a ref of the parent claimed by none while the parent is being
//! absorbed, is AMBIGUOUS — never resolved by order, by name, or by "most specific". Breaking a
//! tie silently is how mail ends up in the wrong lane with nobody able to say when it started.

use std::collections::BTreeSet;

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
pub fn plan_split(_parent: &BTreeSet<Ref>, _children: &[ChildSpec]) -> RoutingVerdict {
    todo!("WP-3: assign uniquely-claimed refs, leave the rest with the parent, report ties")
}

/// PURE. Merge takes the UNION, always (§3); `model_override` and `tick_floor` resolve from the
/// SURVIVOR by rule, so a merge's routing verdict is total and never ambiguous.
pub fn plan_merge(_survivor: &AgentRow, _absorbed: &AgentRow) -> RoutingPlan {
    todo!("WP-3: union the routing refs onto the survivor")
}
