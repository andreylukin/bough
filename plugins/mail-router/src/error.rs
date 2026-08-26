//! Invariant: every failure of the mail seam names the agent or the envelope it was about, so a
//! misroute is diagnosable from the error alone.

use bough_plugin_ledger::{AgentName, LedgerError};

/// What the mail seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    /// The store said no.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    /// No `agents` row by that name.
    #[error("no agent named `{0}`")]
    NoSuchAgent(AgentName),
    /// The agent exists as a row but is not live, so `deliver` has nothing to call.
    #[error("agent `{0}` has no live handle to deliver to")]
    NotLive(AgentName),
    /// Delivery to one recipient failed after others had already landed. Names both halves: a
    /// partial fan-out is a fact the caller must see, never a silent retry.
    #[error(
        "delivery to `{agent}` failed after {delivered} recipient(s) already had it: {detail}"
    )]
    PartialFanOut {
        agent: AgentName,
        delivered: usize,
        detail: String,
    },
    /// A step could not be appended to the unsorted trajectory.
    #[error("the unsorted queue is unavailable: {0}")]
    Unsorted(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
