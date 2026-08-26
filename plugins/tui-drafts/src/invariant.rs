//! §0.2 runtime invariant for `bough-plugin-tui-drafts`:
//!
//! **No key this pane handles reaches an outward seam.** Checked as a data relation: for every `PaneOutcome` this pane returned, no `action/intent` row and no `mail/delivered` step followed from it.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
