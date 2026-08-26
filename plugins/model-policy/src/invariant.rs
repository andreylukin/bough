//! §0.2 runtime invariant for `bough-plugin-model-policy`:
//!
//! **An answer wake's request never carries `terra`, and `model_override` never appears on an
//! answer wake.**
//!
//! §12 makes sol non-overridable for anything answering Andrey; this is the check that says so
//! at runtime rather than in a comment.
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
    pub wake_kind: WakeKind,
    pub answers_andrey: bool,
    pub chose: String,
    pub had_override: bool,
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

/// Forget everything recorded for `fiber`.
pub fn forget(fiber: FiberUid) {
    let mut g = RECORD.lock();
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

/// The whole invariant as a pure function of the observed decisions and the configured pair.
pub fn evaluate(sol: &str, _terra: &str, stream: &[Obs]) -> Result<(), String> {
    for obs in stream {
        if !obs.answers_andrey {
            continue;
        }
        if obs.chose != sol {
            return Err(format!(
                "a wake answering Andrey ({:?}) ran on `{}`, not on sol `{}`",
                obs.wake_kind, obs.chose, sol
            ));
        }
        if obs.had_override {
            return Err(format!(
                "a wake answering Andrey ({:?}) carried a model_override into its request; \
                 sol is not overridable (§12)",
                obs.wake_kind
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
    let (sol, terra) = configured();
    evaluate(&sol, &terra, &seen()).map_err(|detail| InvariantViolation {
        invariant: "an_answer_wake_always_gets_sol_and_never_an_override",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

static CONFIGURED: Mutex<Option<(String, String)>> = Mutex::new(None);

/// `apply` publishes the configured pair so the check is a statement about THIS composition.
pub fn set_configured(sol: &str, terra: &str) {
    *CONFIGURED.lock() = Some((sol.to_string(), terra.to_string()));
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

    #[test]
    fn an_unattended_wake_may_run_on_anything() {
        assert!(evaluate("sol", "terra", &[obs(false, "terra", false)]).is_ok());
        assert!(evaluate("sol", "terra", &[obs(false, "other", true)]).is_ok());
    }

    #[test]
    fn an_answer_wake_on_terra_is_a_violation() {
        let err = evaluate("sol", "terra", &[obs(true, "terra", false)]).unwrap_err();
        assert!(err.contains("not on sol"), "{err}");
    }

    #[test]
    fn an_override_reaching_an_answer_wake_is_a_violation() {
        let err = evaluate("sol", "terra", &[obs(true, "sol", true)]).unwrap_err();
        assert!(err.contains("not overridable"), "{err}");
    }
}
