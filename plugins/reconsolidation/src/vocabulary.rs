//! Invariant: the expiry marker is EVIDENCE, so the ledger itself refuses one with no cites — an
//! expiry that cannot say what justified it is not appendable (§3's two-entry-class rule).

use bough_plugin_ledger::{ClassRule, Ref, StepTypeDef};

/// The step type this crate owns, spelled once.
pub const MEMORY_EXPIRED: &str = "memory/expired";

/// `memory/expired` — §8's "APPENDED marker the projector honors".
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MemoryExpired {
    /// What is expired: `step:…` or `rollup:…` refs. A ref of any other scheme is ignored by the
    /// projector, never an error — a marker is data.
    pub targets: Vec<Ref>,
    pub reason: String,
    pub kind: ReconKind,
}

/// Why a marker was appended.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ReconKind {
    /// Stale evidence, expired by a reconsolidation pass.
    Expiry,
    /// The note a supersession appends naming the block it replaced.
    Supersession,
}

/// The step types this crate owns.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![
        StepTypeDef::of::<MemoryExpired>(MEMORY_EXPIRED, crate::PLUGIN_NAME)
            .class_rule(ClassRule::Evidence),
    ]
}

/// Everything a reconsolidation pass can go wrong as.
#[derive(Debug, thiserror::Error)]
pub enum ReconError {
    #[error("agent `{0}` has no trajectory to reconsolidate")]
    NoTrajectory(String),
    #[error("the model call failed: {0}")]
    Model(String),
    #[error(transparent)]
    Rollups(#[from] bough_plugin_rollups::RollupsError),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
}
