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

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: read ctx.leader.target() and register the five scoped tools")
    }

    fn invariants() -> Vec<InvariantSpec> {
        // See `invariant.rs`: no runtime invariant, and why.
        Vec::new()
    }
}

bough_kernel::register_plugin!(ToolLeaderPlugin);
