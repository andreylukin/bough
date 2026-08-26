//! Invariant (§2): this is the GLOBAL `propose_claim` — any lane agent may propose a
//! `Requirement`, a `Contradiction` or `Other`, and a STRUCTURAL kind is refused with the reason
//! "only the leader proposes structure". Its leader-scoped twin lives in `tool-leader` and accepts
//! the structural kinds; that difference is V6's shadowing subject and a real behavioural
//! difference rather than a test fixture.

use bough_kernel::{Context, PluginError};

use crate::ClaimsHandle;

/// The tool's name, in both this crate and `tool-leader`.
pub const TOOL_NAME: &str = "propose_claim";

/// Register the global tool, if `tools` is bound.
pub async fn register(_ctx: &Context, _claims: &ClaimsHandle) -> Result<(), PluginError> {
    todo!("WP-4: register propose_claim at ToolScope::Global")
}
