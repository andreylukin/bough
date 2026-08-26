//! §0.2 runtime invariant for `bough-plugin-mcp`:
//!
//! **Every `McpCallResult` that reached a trajectory carries exactly the cite `cite_of` mints for its (server, tool, args)**, and no tool result carries a cite a server supplied. The seam mints the citation; a foreign server never does.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
