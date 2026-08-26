//! §0.2 runtime invariant for `bough-plugin-actions-reconcile`:
//!
//! **No action row ever moves from `Intent` to `Intent` by way of a second `action/intent`, and no reconciliation pass produced a write.** An intent whose marker was absent is left `Intent` with exactly one `draft/*` step naming it.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
