//! Invariant (§9's named example): oversized tool output is SPILLED to a file and replaced inline
//! by a locator, so the model always sees a bounded result and never a truncated one that
//! pretends to be whole.

use bough_plugin_tools::PostExecute;

/// The `tools/post-execute` listener the row registers. WP-3.
pub fn spill_if_oversized(_max_output_bytes: usize, _post: &mut PostExecute) {
    todo!("WP-3: write the overflow to a file, accept_content a locator line")
}
