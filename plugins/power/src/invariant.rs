//! §0.2 runtime invariant for `bough-plugin-power`:
//!
//! **Every `power/changed` payload is reflected in its source's `last()`, and a `DidWake` is never dispatched without a preceding `WillSleep` from the same source.**

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
