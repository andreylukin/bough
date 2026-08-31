//! Invariant: a reset is an ACT on the agent's identity, so §3's two-entry-class rule makes it
//! EVIDENCE — it cites the raw steps the rebuild read, and the ledger refuses one with no cites.

use bough_plugin_ledger::{ClassRule, StepTypeDef};

/// The step type this crate owns, spelled once.
pub const DRIFT_RESET: &str = "drift/reset";

/// `drift/reset` — EVIDENCE. Cites the raw steps the rebuild read.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DriftReset {
    pub agent: bough_plugin_ledger::AgentName,
    pub digest: bough_plugin_ledger::RollupId,
    pub about_line: bough_plugin_ledger::StepId,
    pub signals: crate::Signals,
    pub attribution: bough_plugin_rollups::Attribution,
}

/// The step types this crate owns.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![
        StepTypeDef::of::<DriftReset>(DRIFT_RESET, crate::PLUGIN_NAME)
            .class_rule(ClassRule::Evidence),
    ]
}

/// Everything drift-watch can go wrong as.
#[derive(Debug, thiserror::Error)]
pub enum DriftError {
    #[error("agent `{0}` is not in the registry")]
    NoSuchAgent(String),
    #[error("agent `{0}` has no trajectory yet")]
    NoTrajectory(String),
    /// A rebuild "from raw evidence" with no raw evidence would have to invent the state half.
    #[error("agent `{0}` has no raw evidence to rebuild an identity from")]
    NoEvidence(String),
    #[error(transparent)]
    Rollups(#[from] bough_plugin_rollups::RollupsError),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
}
