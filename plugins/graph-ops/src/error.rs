//! Invariant: a refusal names WHAT it refused and WHY, and an ambiguous op is refused rather than
//! guessed (§4). `Ambiguous` carries the question that was asked, so the caller can point at it.

use bough_plugin_ledger::{AgentName, LedgerError, Seq, WakeId};

/// What the graph seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("no agent named `{0}`")]
    NoSuchAgent(AgentName),
    /// P5-D7: an EXPLICIT `at_seq` inside an open wake is an error, never a silent adjustment —
    /// the caller named a point and deserves to know it was not legal.
    #[error("seq {} lies inside the open wake `{wake}`", at_seq.0)]
    OpenWake { wake: WakeId, at_seq: Seq },
    /// The chain has no seq outside an open wake to branch at.
    #[error("`{0}` has no resolvable fork point")]
    NoForkPoint(AgentName),
    /// §4: routing could not be settled. The leader question has already been asked.
    #[error("routing is ambiguous: {detail}")]
    Ambiguous { detail: String },
    /// A merge whose survivor Andrey has not named. Never inferred (§4).
    #[error("a merge needs a survivor named by Andrey")]
    NoSurvivor,
    /// More children than `max_children` allows.
    #[error("a split takes exactly {expected} children, got {got}")]
    ChildCount { expected: usize, got: usize },
    /// The step named by an undo is not one of ours.
    #[error("`{0}` is not a graph op step")]
    NotAnOp(bough_plugin_ledger::StepId),
    #[error("the digest seam refused: {0}")]
    Rollups(#[from] bough_plugin_rollups::RollupsError),
    #[error("the mail seam refused: {0}")]
    Mail(#[from] bough_plugin_mail_router::MailError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
