//! §0.2 runtime invariant for `bough-plugin-mcp-rmcp`:
//!
//! **The set of servers on `ctx.mcp` equals the set of enabled `ServerRow`s under this parent**, and every one of them is owned by exactly one child fiber. A server with no child entry, or a child entry with no server, is the violation.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
