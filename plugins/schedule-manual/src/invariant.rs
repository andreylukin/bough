//! §0.2 runtime invariant for `bough-plugin-schedule-manual`:
//!
//! **No job ever runs without a `fire_now` / `fire_at` call.** Every recorded `JobRun` this Provider produced carries `FireReason::Manual` or a reason a caller named; a `Cadence` fire from this Provider is the violation.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
