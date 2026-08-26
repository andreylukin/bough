//! Invariant: the leader's failures name the TARGET agent. The whole row is "this set, mounted for
//! that agent", so an error that cannot say which agent is an error nobody can act on after a swap.

use bough_plugin_ledger::AgentName;

/// What the leader seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum LeaderError {
    #[error("the leader's target `{0}` has no agents row")]
    NoTarget(AgentName),
    #[error("nothing in the unsorted queue matches `{0}`")]
    NothingToAdopt(AgentName),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
    #[error("the mail seam refused: {0}")]
    Mail(#[from] bough_plugin_mail_router::MailError),
    #[error("the claims seam refused: {0}")]
    Claims(#[from] bough_plugin_claims::ClaimsError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
