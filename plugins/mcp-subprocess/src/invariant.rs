//! §0.2 runtime invariant for `bough-plugin-mcp-subprocess`:
//!
//! **A process's crash and restart never removes its registration on `ctx.mcp` and never disturbs a sibling child entry.** Checked as a relation over child fiber uids and the server set across a restart.

use bough_kernel::InvariantSpec;

/// The specs this crate contributes. Filled by the work package that owns this crate.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
