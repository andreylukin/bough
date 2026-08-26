//! §0.2 runtime invariant for `bough-plugin-model-policy`:
//!
//! **An answer wake's request never carries `terra`, and `model_override` never appears on an
//! answer wake.**
//!
//! §12 makes sol non-overridable for anything answering Andrey; this is the check that says so
//! at runtime rather than in a comment. WP-5 owns the recorder and the check.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_llm::WakeKind;

/// One observed policy decision.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub wake_kind: WakeKind,
    pub answers_andrey: bool,
    pub chose: String,
    pub had_override: bool,
}

/// Record one decision. WP-5.
pub fn record(_obs: Obs) {
    todo!("WP-5")
}

/// Forget everything recorded for `fiber`. WP-5.
pub fn forget(_fiber: FiberUid) {
    todo!("WP-5")
}

/// Everything recorded so far. WP-5.
pub fn seen() -> Vec<Obs> {
    todo!("WP-5")
}

/// The whole invariant as a pure function of the observed decisions and the configured pair.
/// WP-5.
pub fn evaluate(_sol: &str, _terra: &str, _stream: &[Obs]) -> Result<(), String> {
    todo!("WP-5: an answer wake gets sol, and never an override")
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

async fn check(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-5: read the configured pair and evaluate the recorded decisions")
}
