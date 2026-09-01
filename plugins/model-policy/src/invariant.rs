//! §0.2 runtime invariant for `bough-plugin-model-policy`:
//!
//! **An answer wake's request never carries `unattended`, and `model_override` never appears on an
//! answer wake.**
//!
//! §12 makes interactive non-overridable for anything answering Andrey; this is the check that says so
//! at runtime rather than in a comment.
//!
//! The check is NOT self-confirming. The listener's DECISION is one stream; the model that
//! actually reached the request is a second, read off the durable `request/header`'s call config
//! (`agent-loop` appends it after this waterfall has run). Comparing the two catches a later
//! `agent/request` listener rewriting `call.model` — which the old shape, re-checking `choose()`
//! against `choose()`'s own inputs, could never see.
//!
//! `had_override` records whether an override REACHED THE CALL, not whether the agent row
//! carried one: a resident agent may well have a `model_override` and still be messaged by
//! Andrey, and the honest statement is that the override never got into the request.

use std::collections::BTreeMap;

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_llm::WakeKind;
use parking_lot::Mutex;

/// One observed policy decision.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    /// The wake and step the decision was for: the join key against the durable header.
    pub wake: String,
    pub step_index: u32,
    pub wake_kind: WakeKind,
    pub answers_andrey: bool,
    pub chose: String,
    pub had_override: bool,
}

/// One model that actually reached a request, read off a durable `request/header`.
#[derive(Clone, Debug, PartialEq)]
pub struct SentObs {
    pub fiber: FiberUid,
    pub wake: String,
    pub step_index: u32,
    pub model: String,
}

/// The recorded stream, per fiber life (a reload keeps the `FiberUid`, so `apply` registers
/// [`forget`] as its inverse).
static RECORD: Mutex<Option<BTreeMap<FiberUid, Vec<Obs>>>> = Mutex::new(None);

/// Record one decision.
pub fn record(obs: Obs) {
    let mut g = RECORD.lock();
    g.get_or_insert_with(BTreeMap::new)
        .entry(obs.fiber)
        .or_default()
        .push(obs);
}

static SENT: Mutex<Option<BTreeMap<FiberUid, Vec<SentObs>>>> = Mutex::new(None);

/// Record one model the ledger says was actually requested.
pub fn record_sent(obs: SentObs) {
    let mut g = SENT.lock();
    g.get_or_insert_with(BTreeMap::new)
        .entry(obs.fiber)
        .or_default()
        .push(obs);
}

/// Every model the ledger says was actually requested, oldest first.
pub fn sent() -> Vec<SentObs> {
    let g = SENT.lock();
    match g.as_ref() {
        None => Vec::new(),
        Some(map) => map.values().flatten().cloned().collect(),
    }
}

/// Forget everything recorded for `fiber`.
pub fn forget(fiber: FiberUid) {
    let mut g = RECORD.lock();
    if let Some(map) = g.as_mut() {
        map.remove(&fiber);
    }
    let mut g = SENT.lock();
    if let Some(map) = g.as_mut() {
        map.remove(&fiber);
    }
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    let g = RECORD.lock();
    match g.as_ref() {
        None => Vec::new(),
        Some(map) => map.values().flatten().cloned().collect(),
    }
}

/// The whole invariant as a pure function of the decisions, the models that actually reached the
/// requests, and the configured pair.
///
/// `sent` is the honest half: it comes from the ledger, not from this crate's own arithmetic.
pub fn evaluate(
    interactive: &str,
    _terra: &str,
    stream: &[Obs],
    sent: &[SentObs],
) -> Result<(), String> {
    for obs in stream {
        if !obs.answers_andrey {
            continue;
        }
        if obs.chose != interactive {
            return Err(format!(
                "a wake answering Andrey ({:?}) ran on `{}`, not on interactive `{}`",
                obs.wake_kind, obs.chose, interactive
            ));
        }
        if obs.had_override {
            return Err(format!(
                "a wake answering Andrey ({:?}) carried a model_override into its request; \
                 interactive is not overridable (§12)",
                obs.wake_kind
            ));
        }
    }
    // The join: what the ledger says was requested, against what the policy decided.
    for s in sent {
        let Some(d) = stream
            .iter()
            .find(|d| d.fiber == s.fiber && d.wake == s.wake && d.step_index == s.step_index)
        else {
            // A request nothing decided for is not this invariant's business; the loop's own V4
            // reconstruction owns "every request is ledgered".
            continue;
        };
        if s.model != d.chose {
            return Err(format!(
                "wake {} step {}: the policy chose `{}` but the request/header records `{}` \
                 — something after `model-policy` rewrote the model",
                s.wake, s.step_index, d.chose, s.model
            ));
        }
        if d.answers_andrey && s.model != interactive {
            return Err(format!(
                "wake {} step {} answered Andrey on `{}`, not on interactive `{}`",
                s.wake, s.step_index, s.model, interactive
            ));
        }
    }
    Ok(())
}

/// The spec `ModelPolicyPlugin::invariants` returns.
pub fn answer_wakes_get_sol() -> InvariantSpec {
    InvariantSpec {
        name: "an_answer_wake_always_gets_sol_and_never_an_override",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    // The configured pair is the row's own config; `apply` publishes it here so the check reads
    // the same two names the listener chose from.
    let (interactive, unattended) = configured();
    evaluate(&interactive, &unattended, &seen(), &sent()).map_err(|detail| InvariantViolation {
        invariant: "an_answer_wake_always_gets_sol_and_never_an_override",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

static CONFIGURED: Mutex<Option<(String, String)>> = Mutex::new(None);

/// `apply` publishes the configured pair so the check is a statement about THIS composition.
pub fn set_configured(interactive: &str, unattended: &str) {
    *CONFIGURED.lock() = Some((interactive.to_string(), unattended.to_string()));
}

/// The pair `apply` published, or two empty names if the row never applied.
pub fn configured() -> (String, String) {
    CONFIGURED.lock().clone().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(answers: bool, chose: &str, had_override: bool) -> Obs {
        Obs {
            fiber: FiberUid(1),
            wake: "w1".into(),
            step_index: 0,
            wake_kind: if answers {
                WakeKind::Answer
            } else {
                WakeKind::Drain
            },
            answers_andrey: answers,
            chose: chose.into(),
            had_override,
        }
    }

    fn a_sent(model: &str) -> SentObs {
        SentObs {
            fiber: FiberUid(1),
            wake: "w1".into(),
            step_index: 0,
            model: model.into(),
        }
    }

    #[test]
    fn an_unattended_wake_may_run_on_anything() {
        assert!(evaluate(
            "interactive",
            "unattended",
            &[obs(false, "unattended", false)],
            &[]
        )
        .is_ok());
        assert!(evaluate(
            "interactive",
            "unattended",
            &[obs(false, "other", true)],
            &[]
        )
        .is_ok());
    }

    /// The check the old shape could not make: the decision is not the evidence, the ledger is.
    #[test]
    fn a_later_listener_rewriting_the_model_is_a_violation() {
        let err = evaluate(
            "interactive",
            "unattended",
            &[obs(true, "interactive", false)],
            &[a_sent("something-else")],
        )
        .unwrap_err();
        assert!(err.contains("rewrote the model"), "{err}");
    }

    #[test]
    fn a_matching_decision_and_header_are_clean() {
        assert!(evaluate(
            "interactive",
            "unattended",
            &[obs(true, "interactive", false)],
            &[a_sent("interactive")]
        )
        .is_ok());
    }

    #[test]
    fn an_answer_wake_on_terra_is_a_violation() {
        let err = evaluate(
            "interactive",
            "unattended",
            &[obs(true, "unattended", false)],
            &[],
        )
        .unwrap_err();
        assert!(err.contains("not on interactive"), "{err}");
    }

    #[test]
    fn an_override_reaching_an_answer_wake_is_a_violation() {
        let err = evaluate(
            "interactive",
            "unattended",
            &[obs(true, "interactive", true)],
            &[],
        )
        .unwrap_err();
        assert!(err.contains("not overridable"), "{err}");
    }
}
