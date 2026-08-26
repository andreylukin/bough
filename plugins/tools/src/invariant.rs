//! §0.2 runtime invariant for `bough-plugin-tools`:
//!
//! **Every `tool/result` has a matching `tool/call` in the SAME wake and the same step, and no
//! call is answered twice.**
//!
//! This is §0.2's own worked example. The check is a fold over the observed `ledger/step` stream,
//! per fiber and bounded. WP-3 owns the recorder and the check.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{StepType, WakeId};

/// One observed step, reduced to what the invariant is about.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub wake: WakeId,
    pub kind: StepType,
    /// `tool/call` and `tool/result` both carry one.
    pub call: String,
    pub step_index: u32,
}

/// Record one step. WP-3.
pub fn record(_obs: Obs) {
    todo!("WP-3")
}

/// Forget everything recorded for `fiber`. WP-3.
pub fn forget(_fiber: FiberUid) {
    todo!("WP-3")
}

/// Everything recorded so far, oldest first. WP-3.
pub fn seen() -> Vec<Obs> {
    todo!("WP-3")
}

/// The whole invariant as a pure function of the observed stream. WP-3.
pub fn evaluate(_stream: &[Obs]) -> Result<(), String> {
    todo!("WP-3: pair call and result within one wake and step")
}

/// The spec `ToolsPlugin::invariants` returns.
pub fn calls_and_results_pair_within_a_step() -> InvariantSpec {
    InvariantSpec {
        name: "tool_calls_and_results_pair_within_a_step",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "tool_calls_and_results_pair_within_a_step",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}
