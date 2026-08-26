//! Invariant (P5-D11): the persona section is OWNED by the `leader` row's fiber and SCOPED to the
//! target agent by spec. Registering it through the AGENT's ctx (the `worker-spawn` precedent)
//! would tie it to the agent's lifetime, and then moving the set would depend on the old agent
//! being torn down. Owning it here is exactly what makes the SWAP a config edit.

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_ledger::AgentName;

/// The section id the leader's persona is contributed under. Deliberately NOT `"persona"`: the
/// leader's persona moves with the leader SET, where a lane's moves with the lane list (P5-D17).
pub const SECTION_ID: &str = "leader.persona";

/// Register the persona section for `target`, owned by the calling row's ctx.
pub async fn register(
    _ctx: &Context,
    _target: &AgentName,
    _text: &str,
) -> Result<EffectHandle, PluginError> {
    todo!("WP-5: SectionSpec scoped Agent(target), slot Identity, place After")
}
