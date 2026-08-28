//! Invariant: a preview that cannot be taken SAYS so, naming what was missing. An agent with no
//! trajectory is refused, never defaulted onto somebody else's chain (§0.2).

/// Why a preview could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("no agent named `{0}`")]
    NoSuchAgent(String),
    #[error("agent `{0}` has no trajectory")]
    NoTrajectory(String),
    #[error(transparent)]
    Projection(#[from] bough_plugin_projection::ProjectionError),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
}
