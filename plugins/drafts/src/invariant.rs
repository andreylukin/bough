//! §0.2 runtime invariant for `bough-plugin-drafts`:
//!
//! **No `draft/*` step is ever followed by an `action/intent` row naming the same audience, and no draft step is `Class::Evidence`.** A draft is the finished act: the absence of an outward act after one is what this checks.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
