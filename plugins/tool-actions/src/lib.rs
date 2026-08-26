//! Invariant (§7): the natural path for a model IS the journalled path. Each of the four
//! primitives is a tool that calls `ActionsHandle::execute` and nothing else, so an act on the
//! world cannot happen without an intent row — and in Phase 2, with no Provider mounted, each
//! returns a refusal that names the kind.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_actions::ActionKind;
use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tool-actions";

/// What every action tool takes: a target and a kind-specific payload.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ActionArgs {
    /// `owner/repo#12`, `TEAM-123`, a thread id — canonicalised by the seam, not here.
    pub target: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// One of the four primitives, as a tool.
pub struct ActionTool(pub ActionKind);

#[async_trait::async_trait]
impl Tool for ActionTool {
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-7: parse ActionArgs, actions.execute(..), map NoProvider onto a clear refusal")
    }
}

/// The model-visible name of each kind. A fifth spelling is not a tool at all (§7).
pub fn tool_name(kind: ActionKind) -> &'static str {
    kind.as_str()
}

/// No configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolActionsConfig {}

/// The consumer row: four tools, one per kind.
pub struct ToolActionsPlugin;

#[async_trait::async_trait]
impl Plugin for ToolActionsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ToolActionsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "actions"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7: register one ActionTool per ActionKind::all(), each as an effect")
    }
}

bough_kernel::register_plugin!(ToolActionsPlugin);
