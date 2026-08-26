//! Invariant: section order is a pure, total function of `(Slot, Place, SectionId)` — stable under
//! any input permutation (P1-D8).

use crate::RenderedSection;

/// Sort the rendered sections into §5's fixed order.
// `&mut Vec` rather than `&mut [_]`: the phase plan §2.7 fixes this signature, and a rung of the
// degradation ladder removes elements through it.
#[allow(clippy::ptr_arg)]
pub fn order(sections: &mut Vec<RenderedSection>) {
    todo!("WP-4: order::order")
}
