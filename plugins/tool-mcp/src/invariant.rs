//! §0.2 runtime invariant for `bough-plugin-tool-mcp`:
//!
//! **Every registered `mcp__*` tool corresponds to a live server on `ctx.mcp`, and every live server's tools are registered.** Reconciliation, checked as a set equality rather than trusted to the listener.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
