//! Invariant: the expiry marker is EVIDENCE, so the ledger itself refuses one with no cites — an
//! expiry that cannot say what justified it is not appendable (§3's two-entry-class rule).

use bough_plugin_ledger::{ClassRule, StepTypeDef};

/// The step type this row owns outright: one row per model call a pass makes.
pub const RECON_REQUEST: &str = "recon/request";

/// `recon/request` — a THOUGHT. Model-visible ⟺ ledgered (§0.2): the judge's call is
/// reconstructible from `(pass, the pair it judged, prompt_ver, model)`, and this is the row that
/// records the last two. It is also where a cost bench reads a pass's token counts, and it is
/// written for a FAILED call too, so a billed call is never invisible.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReconRequest {
    /// The pass's id — the same string its wake carries after the `recon:` prefix.
    pub pass: String,
    pub prompt_ver: String,
    pub model: String,
    /// The two steps the judge was shown.
    pub older: String,
    pub newer: String,
    /// A hash of the rendered input, so a replay can prove the same input produced the same
    /// verdict.
    pub input_digest: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// `true` when the stream failed before it ended; the verdict then CLEARS the pair, but the
    /// call still happened.
    pub failed: bool,
}

/// The step type this crate WRITES. It is not this crate's alone: `rollups-summarizer` appends
/// one too (the note a supersession leaves), so the definition lives in the seam and both rows
/// declare exactly it (§0.2 — row order carries no load semantics, and unloading one row leaves
/// the type standing for the other).
pub use bough_plugin_rollups::EXPIRED_STEP_TYPE as MEMORY_EXPIRED;

/// The marker body — the seam's, so the accepted body set cannot depend on mount order.
pub use bough_plugin_rollups::ExpiredBody as MemoryExpired;

/// Why a marker was appended.
pub use bough_plugin_rollups::ExpiryKind as ReconKind;

/// The step types this row declares.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![
        bough_plugin_rollups::expiry::step_type_def(),
        StepTypeDef::of::<ReconRequest>(RECON_REQUEST, crate::PLUGIN_NAME)
            .class_rule(ClassRule::Thought),
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
