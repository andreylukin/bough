//! §0.2 runtime invariant for `bough-plugin-projection-assembler`:
//!
//! **`model_visible_is_ledgered`** — every `SectionCites` entry of every projection assembled this
//! session names a step or rollup id that exists in the ledger. The statement and the pure
//! evaluation live in `bough_plugin_projection::invariant` (P1-D22); this module only names the
//! plugin.

use bough_kernel::InvariantSpec;

/// The one spec this provider returns.
pub fn specs() -> Vec<InvariantSpec> {
    vec![bough_plugin_projection::invariant::model_visible_is_ledgered(crate::PLUGIN_NAME)]
}
