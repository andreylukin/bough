//! Invariant (§2): these tools are the leader's ONLY extra powers, and every one of them either
//! PROPOSES or CURATES. None of them accepts a claim, and none of them applies a graph op
//! directly: `propose_structure` writes `claim/proposed`, never an op. The leader is an ordinary
//! agent row with a wider vocabulary, not an authority.

pub mod invariant;
pub mod tools;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};

pub use tools::TOOL_NAMES;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tool-leader";

/// The row's config. Deliberately EMPTY: no `agent` field (P5-D10).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolLeaderConfig {}

/// The `tool.leader` row.
pub struct ToolLeaderPlugin;

#[async_trait::async_trait]
impl Plugin for ToolLeaderPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ToolLeaderConfig;

    fn inject() -> Inject {
        Inject::required(["leader", "tools", "claims", "graph"])
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        // The target comes from the BINDING, never from this row's config (P5-D10). Injecting
        // `leader` is also what makes the move atomic: when the `leader` row reloads against a
        // new `config.agent`, `ctx.leader` is withdrawn, this row unloads with it — taking the
        // five tools out of the old agent's scope — and reloads against the new binding.
        let leader = ctx
            .get::<bough_plugin_leader::Leader>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        tools::register(&ctx, &leader).await
    }

    fn invariants() -> Vec<InvariantSpec> {
        // See `invariant.rs`: no runtime invariant, and why.
        Vec::new()
    }
}

bough_kernel::register_plugin!(ToolLeaderPlugin);
