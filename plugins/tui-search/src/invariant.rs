//! §0.2 runtime invariant for `bough-plugin-tui-search`:
//!
//! **Every hit row rendered names a step that still exists in the ledger.** A search pane is the
//! easiest place to show a fact that is no longer there; the check is over the pane's own rendered
//! rows against the ledger it queried.
//!
//! WP-5 owns the recorder and the check.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

/// PURE: the check, over the rendered rows and the step ids the ledger still holds.
pub fn check_rows(
    _rendered: &[crate::HitRow],
    _known: &[bough_plugin_ledger::StepId],
) -> Result<(), String> {
    todo!("WP-5")
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "every_rendered_hit_names_a_live_step",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-5")
}
