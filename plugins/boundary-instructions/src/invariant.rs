//! §0.2 runtime invariant for `bough-plugin-boundary-instructions`:
//!
//! **Every projection this row contributed to carries `BOUNDARY_BLOCK` byte-for-byte, and no other section in the tree carries a paraphrase of it.** One source, checked against assembled projections rather than asserted in prose.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
