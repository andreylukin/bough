//! Invariant: all five tools register at `ToolScope::Agent(target)` where `target` comes from
//! `ctx.leader.target()` and NEVER from this row's own config (P5-D10). Two rows with two
//! spellings of one target is a misconfiguration that would present as "half the leader set
//! moved"; injecting the key makes the move atomic.
//!
//! `propose_claim` SHADOWS the global one from `claims`: it accepts the structural kinds the
//! global one refuses. That is V6's shadowing subject and a real difference in behaviour.

use bough_kernel::{Context, PluginError};

/// The five tool names, in registration order.
pub const TOOL_NAMES: [&str; 5] = [
    "propose_claim",
    "adopt_unsorted",
    "draft_requirement",
    "propose_structure",
    "note_timeline",
];

/// Register all five for the leader's target.
pub async fn register(
    _ctx: &Context,
    _leader: &bough_plugin_leader::LeaderHandle,
) -> Result<(), PluginError> {
    todo!("WP-5: register the five tools at ToolScope::Agent(leader.target())")
}
