//! §0.2 runtime invariant for `bough-plugin-tui-shell`:
//!
//! **Every registered pane's owner row is still ACTIVE, and no two panes share an id.** A pane
//! outliving the row that registered it is exactly the failure "registrations are effects"
//! forbids, and it is what the SWAP gate would otherwise hide.
//!
//! WP-2 owns the recorder and the check.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

/// PURE: the check, over the live pane list and the set of active row ids.
pub fn check_panes(
    _panes: &[crate::pane::PaneInfo],
    _active_rows: &[bough_kernel::EntryId],
) -> Result<(), String> {
    todo!("WP-2")
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "every_pane_has_a_live_owner_and_a_unique_id",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-2")
}
