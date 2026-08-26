//! §0.2 runtime invariant for `bough-plugin-system-schedules`:
//!
//! **A `Pending` reconsolidation fire never appends a step and never fails its row.** The row stays ACTIVE across any number of pending fires, and the catch-up pass requests at most one wake per live agent per fire.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
