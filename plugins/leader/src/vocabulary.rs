//! Invariant: `timeline/entry` is EVIDENCE because a timeline is rendered as truth (§16). An
//! entry with no cites would be the leader asserting a history nobody can check — and the ledger
//! itself refuses an Evidence step with no cites, so the rule is enforced at append rather than
//! remembered here.

use bough_plugin_ledger::{AgentName, ClassRule, Ref, StepTypeDef};

/// The owner string the step type is registered under.
pub const OWNER: &str = "leader";

/// The one step type this crate owns.
pub const TIMELINE_ENTRY: &str = "timeline/entry";

/// `timeline/entry` — Evidence.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TimelineEntryBody {
    pub title: String,
    /// RFC3339. The moment the entry is about, which is not the moment it was written.
    pub at: String,
    #[serde(default)]
    pub agents: Vec<AgentName>,
    #[serde(default)]
    pub refs: Vec<Ref>,
}

/// This crate's step types, for `LedgerHandle::declare_step_types`.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![StepTypeDef::of::<TimelineEntryBody>(TIMELINE_ENTRY, OWNER).class_rule(ClassRule::Evidence)]
}
