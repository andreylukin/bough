//! Invariant: "open" is DERIVED — a `claim/proposed` with no later `claim/accepted` or
//! `claim/rejected` naming it. Nothing stamps a claim closed, so a decision written by any binary
//! closes it for every reader (§3: membership is derived, never stamped).

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, Step, StepType, TrajId};

use crate::{kind, ClaimId, OpenClaim};

/// Where the proposer's name rides inside the `claim/proposed` body, beside
/// [`crate::kind::DETAIL_KEY`] and for the same reason: `ClaimProposed` is the ledger's type and
/// Phase 5 does not change it. The name is written rather than derived from the trajectory,
/// because a merge deletes the row a derivation would have read (§4).
pub const BY_KEY: &str = "by";

/// Which open claims to read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaimQuery {
    /// `None` ⇒ every trajectory.
    pub traj: Option<TrajId>,
    /// `None` ⇒ every proposer.
    pub by: Option<AgentName>,
    pub limit: Option<usize>,
}

/// The three step kinds a claim's life is made of.
pub fn kinds() -> Vec<StepType> {
    vec![
        StepType::new("claim/proposed"),
        StepType::new(crate::rate::CLAIM_ACCEPTED),
        StepType::new(crate::rate::CLAIM_REJECTED),
    ]
}

/// PURE: one proposal step as an [`OpenClaim`]. `None` if the body is not a proposal.
pub fn as_claim(step: &Step) -> Option<OpenClaim> {
    let body = step.body.as_ref();
    let claim = body.get("claim").and_then(|v| v.as_str())?;
    let kind_str = body.get("kind").and_then(|v| v.as_str()).unwrap_or("other");
    Some(OpenClaim {
        claim: ClaimId::new(claim),
        proposal: step.id.clone(),
        traj: step.traj.clone(),
        by: AgentName::new(
            body.get(BY_KEY)
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>"),
        ),
        kind: kind::parse(kind_str, body),
        title: body
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        body: body
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        at: step.at,
        cites: step.cites.as_ref().clone(),
    })
}

/// PURE: the claim ids a window has DECIDED, from its `claim/accepted` and `claim/rejected` steps.
pub fn decided(steps: &[Step]) -> BTreeSet<ClaimId> {
    steps
        .iter()
        .filter(|s| {
            s.kind.as_str() == crate::rate::CLAIM_ACCEPTED
                || s.kind.as_str() == crate::rate::CLAIM_REJECTED
        })
        .filter_map(|s| s.body.get("claim").and_then(|v| v.as_str()))
        .map(ClaimId::new)
        .collect()
}

/// PURE: the open claims in a window, newest first.
pub fn open(steps: &[Step], q: &ClaimQuery, limit: usize) -> Vec<OpenClaim> {
    let closed = decided(steps);
    let mut out: Vec<OpenClaim> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "claim/proposed")
        .filter_map(as_claim)
        .filter(|c| !closed.contains(&c.claim))
        .filter(|c| q.traj.as_ref().is_none_or(|t| *t == c.traj))
        .filter(|c| q.by.as_ref().is_none_or(|b| *b == c.by))
        .collect();
    out.reverse();
    out.truncate(q.limit.unwrap_or(limit));
    out
}
