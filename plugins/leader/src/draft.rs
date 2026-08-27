//! Invariant (§2, §16): drafting a requirement from Andrey's words produces a CLAIM, never a pin.
//! The leader may write down what it heard; only Andrey can make it binding. A leader that could
//! pin its own reading of a conversation would be able to rewrite the requirements by paraphrase.

use bough_plugin_ledger::{Cite, StepId, TrajId, WakeId};
use chrono::{DateTime, Utc};

/// A requirement draft.
#[derive(Clone, Debug)]
pub struct DraftRequest {
    /// The trajectory the words were said on, so the claim cites them.
    pub traj: TrajId,
    /// The wake the draft is a step of.
    ///
    /// DEVIATION from plan §2.5, which lists no `wake`: `ClaimsHandle::propose` takes one, and a
    /// draft written by the `draft_requirement` TOOL happens inside a wake that the proposal
    /// belongs to. `None` — a draft made outside any wake — is what the claims seam turns into
    /// its synthetic `claim:<id>` wake.
    pub wake: Option<WakeId>,
    pub title: String,
    pub body: String,
    /// Andrey's own words. Cited, because the claim renders as a reading OF something.
    pub from: Vec<Cite>,
    /// Pins this requirement would supersede if accepted (§3's relief valve).
    pub supersedes: Vec<StepId>,
    pub at: DateTime<Utc>,
}

/// PURE: the [`bough_plugin_claims::ProposeRequest`] a draft becomes.
///
/// It is a `Requirement` CLAIM and it cites Andrey's words. Nothing here writes a pin, and there
/// is no branch that could: `ctx.claims.decide` is the only writer of `pin/set`, and it refuses
/// any actor but Andrey.
pub fn as_proposal(
    by: bough_plugin_ledger::AgentName,
    req: &DraftRequest,
) -> bough_plugin_claims::ProposeRequest {
    bough_plugin_claims::ProposeRequest {
        by,
        traj: req.traj.clone(),
        wake: req.wake.clone(),
        kind: bough_plugin_claims::ClaimKind::Requirement {
            supersedes: req.supersedes.clone(),
        },
        title: req.title.clone(),
        body: req.body.clone(),
        cites: req.from.clone(),
        at: req.at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{AgentName, Ref, TrajId};

    fn draft() -> DraftRequest {
        DraftRequest {
            traj: TrajId::new("t-sol"),
            wake: None,
            title: "ship the strip".to_string(),
            body: "the strip shows every lane".to_string(),
            from: vec![Cite {
                r#ref: Ref::new("step:s1"),
                url: None,
            }],
            supersedes: vec![StepId::new("p0")],
            at: chrono::Utc::now(),
        }
    }

    #[test]
    fn a_draft_is_a_requirement_claim_that_cites_the_words() {
        let p = as_proposal(AgentName::new("sol"), &draft());
        assert!(matches!(
            p.kind,
            bough_plugin_claims::ClaimKind::Requirement { .. }
        ));
        assert_eq!(
            p.cites.len(),
            1,
            "the claim renders as a reading OF something"
        );
        assert_eq!(p.by, AgentName::new("sol"));
    }

    #[test]
    fn the_supersedes_list_rides_the_kind_and_pins_nothing_yet() {
        let p = as_proposal(AgentName::new("sol"), &draft());
        match p.kind {
            bough_plugin_claims::ClaimKind::Requirement { supersedes } => {
                assert_eq!(supersedes, vec![StepId::new("p0")]);
            }
            other => panic!("a draft is a requirement, got {other:?}"),
        }
    }
}
