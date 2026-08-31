//! Invariant: every failure of the agents seam names the agent it is about, so a boot log or a
//! test failure never needs a second lookup to be actionable.

use bough_plugin_ledger::{AgentName, LedgerError};

use crate::agent::Status;

/// What the agents seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("an agent factory is already set by driver `{0}`")]
    FactoryAlreadySet(&'static str),
    #[error("no agent factory is set; mount an `agent-loop` row")]
    NoFactory,
    #[error("agent `{0}` is already live")]
    AlreadyLive(AgentName),
    #[error("no live agent named `{0}`")]
    NoSuchAgent(AgentName),
    #[error("setup for agent `{name}` failed, and the creation was rolled back: {detail}")]
    SetupFailed { name: AgentName, detail: String },
    #[error("agent `{name}` is disposed")]
    Disposed { name: AgentName },
    #[error("status `{0:?}` repeats the current status")]
    StatusRepeat(Status),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}
