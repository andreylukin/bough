//! §0.2 runtime invariant for `bough-plugin-actions-github`:
//!
//! **Every artifact this Provider produced contains its action's marker, and no `push_to_pr` or `bot_thread_op` was executed without its pre-flight lookup.** Checked against the journal's `action/done` rows and the recorded argv of every `gh` write.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
