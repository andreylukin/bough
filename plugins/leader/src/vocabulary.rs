//! Invariant: `timeline/entry` is EVIDENCE because a timeline is rendered as truth (§16). An
//! entry with no cites would be the leader asserting a history nobody can check.

use bough_plugin_ledger::{AgentName, Ref};

/// The owner string the step type is registered under.
pub const OWNER: &str = "leader";

/// `timeline/entry` — Evidence.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TimelineEntryBody {
    pub title: String,
    /// RFC3339. The moment the entry is about.
    pub at: String,
    #[serde(default)]
    pub agents: Vec<AgentName>,
    #[serde(default)]
    pub refs: Vec<Ref>,
}

/// Declare the step type on the bound ledger. Called once, from `apply`.
pub fn declare(_ledger: &bough_plugin_ledger::LedgerHandle) -> Result<(), crate::LeaderError> {
    todo!("WP-5: declare timeline/entry as Evidence")
}
