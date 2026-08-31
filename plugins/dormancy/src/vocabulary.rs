//! Invariant (P5-D2): dormancy is a DERIVED fold over `agent/dormancy` steps, never a column on
//! `agents`. §3 makes membership derived and never stamped, and a column would mean a schema
//! change in two ledger Providers plus the conformance suite for one boolean.

use bough_plugin_ledger::{ClassRule, StepTypeDef};
use bough_plugin_rollups::Attribution;

use crate::ReactivateCause;

/// The owner string the step type is registered under.
pub const OWNER: &str = "dormancy";

/// The step type name. Read by `tui-strip` BY NAME (P3-D11), so the strip gains no dependency on
/// this crate.
pub const STEP_TYPE: &str = "agent/dormancy";

/// `agent/dormancy` — [`ClassRule::Either`]. Appended to the agent's OWN trajectory, so the fold
/// is one `StepQuery { trajs: [traj], kinds: ["agent/dormancy"], order: SeqDesc, limit: 1 }` per
/// agent at activation.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AgentDormancy {
    pub dormant: bool,
    pub reason: String,
    pub by: Attribution,
    /// `None` when going to sleep; the trigger that reactivated when waking.
    #[serde(default)]
    pub cause: Option<ReactivateCause>,
}

/// The step types this crate owns, for `LedgerHandle::declare_step_types`.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![StepTypeDef::of::<AgentDormancy>(STEP_TYPE, OWNER).class_rule(ClassRule::Either)]
}
