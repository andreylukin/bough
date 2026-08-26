//! Invariant: expiry is an APPENDED marker, never an edit (§8). This module owns the SET a run of
//! markers folds down to, so the projector and the governance rows read one implementation. A
//! marker naming something unresolvable is IGNORED, never an error — a marker is data. Pins and
//! claims are absent from [`NEVER_EXPIRABLE`]'s complement by construction (§3, V7): a pin's only
//! relief valve is supersession.

use std::collections::BTreeSet;

use bough_plugin_ledger::{RollupId, Step, StepId};

/// What a run of `memory/expired` markers expires.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Expired {
    pub steps: BTreeSet<StepId>,
    pub rollups: BTreeSet<RollupId>,
}

/// PURE: fold `memory/expired` steps into the set.
///
/// A marker naming a target that is not a step or rollup ref is ignored, never an error.
pub fn parse(_markers: &[Step]) -> Expired {
    todo!("WP-1: expiry folding")
}

/// The step kinds an expiry pass may NEVER name (§3, V7).
pub const NEVER_EXPIRABLE: &[&str] = &[
    "pin/set",
    "pin/retire",
    "claim/proposed",
    "claim/accepted",
    "claim/rejected",
];
