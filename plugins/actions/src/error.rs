//! Invariant (§7): a refusal SAYS WHY and names the kind. Phase 2 registers no Providers, so
//! `NoProvider` is what the model meets for all four kinds — and it must read as a capability
//! the harness does not have, never as a malfunction.

use bough_plugin_ledger::{ActionId, AgentName, LedgerError, StepId};

/// What the actions seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("no provider is registered for action kind `{0}`; the harness cannot perform it")]
    NoProvider(&'static str),
    /// Merge note 3 / V10: a name that is not one of §7's four, refused BY THE EXECUTOR. Runtime
    /// code and tool calls carry the kind as a STRING, and "there is no `slack_send`" is the
    /// actions seam's answer about its own vocabulary — not a parser's, one step earlier.
    #[error("`{name}` is not one of the four outward acts the harness can perform: {known}")]
    NoSuchKind { name: String, known: &'static str },
    /// Merge note 4: the payload names something §7 does not sanction (a ticket title, a new
    /// issue, both a status and a comment). A REFUSAL, not a provider malfunction — which is
    /// what `Provider { source }` would have read as.
    #[error("`{kind}` payload is not what §7 sanctions: {detail}")]
    BadPayload { kind: &'static str, detail: String },
    #[error(
        "action `{kind}` on `{target}` from step `{step}` is already journalled as `{action}`"
    )]
    Duplicate {
        kind: &'static str,
        target: String,
        step: StepId,
        action: ActionId,
    },
    #[error("`{0}` is not a valid target for `{1}`")]
    BadTarget(String, &'static str),
    #[error("`{0}` is not a known agent, so the action has no trajectory to be journalled in")]
    UnknownAgent(AgentName),
    #[error("the `{kind}` provider failed: {source}")]
    Provider {
        kind: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}
