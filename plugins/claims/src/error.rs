//! Invariant (§16): the refusal that matters here is [`ClaimsError::NotAndreysAct`], and it is a
//! REAL refusal rather than a warning. Acceptance is Andrey's act; a wake that reaches `decide`
//! is refused, and the error says why in the same words §16 does.

use bough_plugin_ledger::LedgerError;

use crate::ClaimId;

/// What the claims seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum ClaimsError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("no open claim `{0}`")]
    NoSuchClaim(ClaimId),
    #[error("claim `{0}` has already been decided")]
    AlreadyDecided(ClaimId),
    /// P5-D16: reached while an agent's ambient initiator scope was set, which is exactly the
    /// condition "this call is inside a wake". A guard against accident, not against a hostile
    /// in-process caller — §2 is explicit that ambient presence is never authorization.
    #[error("accepting a claim is Andrey's act; `{by}` is a wake")]
    NotAndreysAct { by: String },
    /// §2: only the leader proposes structure. The global `propose_claim` refuses the structural
    /// kinds; the leader-scoped twin in `tool-leader` accepts them.
    #[error("only the leader proposes structure (`{kind}` from a lane agent)")]
    NotTheLeader { kind: String },
    #[error("the graph seam refused: {0}")]
    Graph(#[from] bough_plugin_graph_ops::GraphError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
