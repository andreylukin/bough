//! Invariant: a dormancy failure names the agent it was about. Dormancy is a property of ONE lane
//! and a failure that cannot say whose is a failure nobody can act on.

use bough_plugin_ledger::{AgentName, LedgerError};

/// What the dormancy seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum DormancyError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("no agent named `{0}`")]
    NoSuchAgent(AgentName),
    /// Sleeping an already-dormant lane, or waking an already-live one, is IDEMPOTENT and is not
    /// this error; this is the case where the fold itself could not be read.
    #[error("the dormancy fold for `{agent}` could not be read: {detail}")]
    FoldUnreadable { agent: AgentName, detail: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
