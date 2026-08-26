//! §0.2 runtime invariant for `bough-plugin-wards-rhai`:
//!
//! **Every `ward/fired` step's `actions` list is exactly what `evaluate` returned for its `on` seq**, and no seam call this host made is absent from some `ward/fired` row. Purity of the script, checked against the journal rather than trusted.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
