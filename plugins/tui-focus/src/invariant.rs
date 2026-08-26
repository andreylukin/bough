//! §0.2 runtime invariant for `bough-plugin-tui-focus`:
//!
//! **No step is rendered twice: the live tail and the durable rows never overlap.** The tee and
//! the `ledger/step` listener race by construction, so the one thing that must hold over time is
//! that their outputs are disjoint at every frame (P3-D12).
//!
//! WP-4 owns the recorder and the check.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

/// PURE: the check, over the rows and the live tail of one frame.
pub fn check_frame(_rows: &[crate::Row], _live: &crate::LiveText) -> Result<(), String> {
    todo!("WP-4")
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "the_live_tail_and_the_durable_rows_never_overlap",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-4")
}
