//! §0.2 runtime invariant for `bough-plugin-sleep-listener`:
//!
//! **The row is ACTIVE on every platform.** On macOS the source is `iokit` (or `nsworkspace` when IOKit gave no port); everywhere else it is `noop`. A row that failed to activate because the platform is not macOS is the violation.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
