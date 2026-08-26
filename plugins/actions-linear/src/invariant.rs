//! §0.2 runtime invariant for `bough-plugin-actions-linear`:
//!
//! **No `action/done` row this Provider wrote created a Linear issue**, and every comment it wrote carries its action's marker. The kind set it registers is exactly `[linear_write]`.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
