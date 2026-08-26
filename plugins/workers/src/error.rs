//! Invariant (§7, §10): a bound refusal NAMES the bound, the current value and the limit, so a
//! spawner that hits one can tell the model something true instead of "failed".

use bough_plugin_agents::AgentError;

use crate::ids::WorkerId;
use crate::start::WorkerKind;

/// What the workers seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker bound `{bound}` reached: {current} of {limit}")]
    BoundsExceeded {
        bound: &'static str,
        current: usize,
        limit: usize,
    },
    #[error("no worker provider registered for kind `{0:?}`")]
    NoProvider(WorkerKind),
    #[error("the worker's report does not match seal `{seal}`: {detail}")]
    SealInvalid { seal: String, detail: String },
    #[error("worker `{0}` was cancelled")]
    Cancelled(WorkerId),
    #[error("the workers seam is not wired: {0}")]
    Seam(String),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
}
