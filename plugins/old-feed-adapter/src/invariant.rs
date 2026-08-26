//! §0.2 runtime invariant for `bough-plugin-old-feed-adapter`:
//!
//! **No step this row appends carries a `cmd:` / `bough:command:` ref, and no `mail/delivered`
//! step exists with two identical `jungler:event:` refs.** The first half asserts §14's rule that
//! command memory is priming and never mail; the second is the at-least-once ref guard, checked
//! against the ledger rather than documented.
//!
//! WP-6 owns the check.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::Step;

/// PURE: the check, over the steps this row appended.
pub fn check_steps(_appended: &[Step]) -> Result<(), String> {
    todo!("WP-6")
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "no_command_ref_and_no_duplicate_jungler_event",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-6")
}
