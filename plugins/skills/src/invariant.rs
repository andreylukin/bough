//! §0.2 runtime invariant for `bough-plugin-skills`:
//!
//! **Every assembled projection contains a skill's section if and only if that request mentioned one of its triggers**, and at most `max_injected` skill sections appear, chosen by `SectionId` order and never by load order.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
