//! Invariant (§7): the JOURNAL COMES FIRST. An intent row and an `action/intent` step are written
//! BEFORE the provider is called, and `action/done` after — so a crash between the two leaves an
//! intent-without-done row that reconciliation can LIST (and never re-execute).

use std::sync::Arc;

use bough_plugin_ledger::{ActionId, AgentName, IdemKey, StepId, WakeId};
use chrono::{DateTime, Utc};

use crate::kind::{ActionKind, ActionTarget};

/// One request to act on the world.
#[derive(Clone, Debug)]
pub struct ActionRequest {
    pub kind: ActionKind,
    pub target: ActionTarget,
    pub payload: serde_json::Value,
    pub agent: AgentName,
    pub wake: WakeId,
    /// The TRIGGERING step (§7's idem_key formula). NOT the action's own step.
    pub step: StepId,
    pub at: DateTime<Utc>,
}

/// `idem_key = sha256(kind ‖ canonical target ‖ triggering step id)`, hex (§7).
///
/// WP-7.
pub fn idem_key(_kind: ActionKind, _canonical_target: &str, _step: &StepId) -> IdemKey {
    todo!("WP-7: the one formula, with a stable separator")
}

/// What the Provider is handed. The marker is derived from the idem key, so the artifact carries
/// the journal's own name and reconciliation is a lookup against the world (§7).
pub struct ExecuteRequest {
    pub request: Arc<ActionRequest>,
    pub action: ActionId,
    pub idem_key: IdemKey,
    pub marker: String,
}

/// What a Provider produced.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionArtifact {
    /// Where the artifact is: a PR url, a commit sha, a comment id.
    pub locator: String,
    /// The marker the Provider embedded in it.
    pub marker: String,
    pub detail: serde_json::Value,
}

/// A journal row with an intent and no done. What reconciliation lists at boot.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingAction {
    pub action: ActionId,
    pub kind: ActionKind,
    pub idem_key: IdemKey,
    pub target: String,
    pub marker: String,
    pub at: DateTime<Utc>,
}

/// The marker derived from an idem key: what a Provider embeds in the artifact. WP-7.
pub fn marker_for(_idem: &IdemKey) -> String {
    todo!("WP-7: a short, greppable marker derived from the idem key")
}
