//! §0.2 runtime invariant for `bough-plugin-catch-up-on-wake`:
//!
//! **No agent has two catch-up wakes attributable to one `DidWake`**, and no disposed or worker agent has any. Checked against the `wake/start` rows whose cause is `CatchUp`.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
