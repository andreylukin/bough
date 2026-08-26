//! §0.2 runtime invariant for `bough-plugin-collector-linear`:
//!
//! **No two `mail/delivered` steps on one trajectory carry the same `linear:` ref, and the API key appears in no step, no report and no log line.** The second half is checked by scanning what this row wrote for the configured secret.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
