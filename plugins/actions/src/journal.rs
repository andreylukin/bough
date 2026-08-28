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

/// An [`ActionRequest`] whose kind has NOT been resolved yet: what runtime code and a tool call
/// carry, where the kind is a string somebody typed. [`crate::ActionsHandle::execute_by_name`]
/// resolves the name and refuses it if it is not one of §7's four (merge note 3).
#[derive(Clone, Debug)]
pub struct ActionRequestParts {
    pub target: ActionTarget,
    pub payload: serde_json::Value,
    pub agent: AgentName,
    pub wake: WakeId,
    /// The TRIGGERING step (§7's idem_key formula). NOT the action's own step.
    pub step: StepId,
    pub at: DateTime<Utc>,
}

impl ActionRequestParts {
    /// The full request, once the executor has resolved the name.
    pub fn with_kind(self, kind: ActionKind) -> ActionRequest {
        ActionRequest {
            kind,
            target: self.target,
            payload: self.payload,
            agent: self.agent,
            wake: self.wake,
            step: self.step,
            at: self.at,
        }
    }
}

/// `idem_key = sha256(kind ‖ canonical target ‖ triggering step id)`, hex (§7).
///
/// The separator is a NUL byte, which none of the three fields can contain, so no two distinct
/// triples can be spelled into one string.
pub fn idem_key(kind: ActionKind, canonical_target: &str, step: &StepId) -> IdemKey {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(kind.as_str().as_bytes());
    h.update([0u8]);
    h.update(canonical_target.as_bytes());
    h.update([0u8]);
    h.update(step.as_str().as_bytes());
    IdemKey::new(format!("{:x}", h.finalize()))
}

/// What the Provider is handed. The marker is derived from the idem key, so the artifact carries
/// the journal's own name and reconciliation is a lookup against the world (§7).
pub struct ExecuteRequest {
    pub request: Arc<ActionRequest>,
    pub action: ActionId,
    pub idem_key: IdemKey,
    pub marker: String,
    /// The target the idem key was computed over — the Provider acts on THIS, never on `raw`.
    pub canonical_target: String,
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

/// The marker derived from an idem key: what a Provider embeds in the artifact.
///
/// It is DERIVED, never chosen: reconciliation greps the world for the marker of an
/// intent-without-done row, so the marker must be recomputable from the journal alone (§7).
pub fn marker_for(idem: &IdemKey) -> String {
    format!("{}{}", MARKER_PREFIX, &idem.as_str()[..MARKER_HEX_LEN])
}

/// The fixed prefix of every marker. A protocol constant, not a tunable (§0.2): it is written into
/// artifacts that outlive any config.
pub const MARKER_PREFIX: &str = "bough-action:";

/// How much of the idem key the marker carries. 16 hex digits = 64 bits, ample against collision
/// among one person\'s actions and short enough to sit in a commit trailer.
pub const MARKER_HEX_LEN: usize = 16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_is_derived_from_the_idem_key() {
        let k = idem_key(ActionKind::OpenPr, "owner/repo", &StepId::new("s1"));
        assert_eq!(
            marker_for(&k),
            format!("bough-action:{}", &k.as_str()[..16])
        );
    }

    #[test]
    fn the_key_separates_its_three_fields() {
        // Without a separator these two would hash the same bytes.
        let a = idem_key(ActionKind::OpenPr, "ab", &StepId::new("c"));
        let b = idem_key(ActionKind::OpenPr, "a", &StepId::new("bc"));
        assert_ne!(a, b);
    }
}
