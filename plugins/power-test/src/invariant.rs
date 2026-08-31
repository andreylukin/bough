//! §0.2 runtime invariant for `bough-plugin-power-test`:
//!
//! No runtime invariant: this Provider owns no data relation and no event stream of its own — it dispatches exactly what a caller hands it, and the `power` seam's own invariant already polices `power/changed`.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
