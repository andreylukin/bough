//! §0.2 runtime invariant for `bough-plugin-about-line`:
//!
//! **Every `about/line` cites at least one step that exists, and follows a `completed`
//! `wake/end`.**
//!
//! The line is EVIDENCE, so its state half must be anchored in steps rather than in a
//! recollection; and a preempted wake refreshes nothing, which is exactly what "follows a
//! completed wake/end" checks. WP-5 owns the recorder and the check.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::Step;

/// The whole invariant as a pure function of a trajectory's steps. WP-5.
pub fn evaluate(_steps: &[Step]) -> Result<(), String> {
    todo!("WP-5: cites exist, and the preceding wake/end is completed")
}

/// The spec `AboutLinePlugin::invariants` returns.
pub fn lines_cite_and_follow_completed_wakes() -> InvariantSpec {
    InvariantSpec {
        name: "about_lines_cite_real_steps_and_follow_completed_wakes",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-5: read the ledger and evaluate")
}
