//! §0.2 runtime invariant for `bough-plugin-hooks-exec`:
//!
//! **A quarantined point is never invoked again in this process**, and no point is invoked more than once per dispatch of its event. Checked against the recorded exec counter and the `hook/fired` rows.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
