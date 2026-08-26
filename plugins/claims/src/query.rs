//! Invariant: "open" is DERIVED — a `claim/proposed` with no later `claim/accepted` or
//! `claim/rejected` naming it. Nothing stamps a claim closed, so a decision written by any binary
//! closes it for every reader (§3: membership is derived, never stamped).

use bough_plugin_ledger::{AgentName, TrajId};

/// Which open claims to read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaimQuery {
    /// `None` ⇒ every trajectory.
    pub traj: Option<TrajId>,
    /// `None` ⇒ every proposer.
    pub by: Option<AgentName>,
    pub limit: Option<usize>,
}
