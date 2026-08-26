//! Invariant: the registered tool set RECONCILES with the server set. This row listens on
//! `mcp/servers-changed` and adds/removes registrations to match, so disabling a server row removes
//! exactly its tools with no restart — and a tool the model can see is always a tool the seam can
//! route (§9's "the set shown and the set callable are the same set").
//!
//! P6-D8: every MCP tool is `is_concurrency_safe == false`. The seam cannot know what a foreign
//! server does, and §9 makes everything-but-`true` exclusive.

pub mod command;
pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_mcp::{McpHandle, McpToolInfo};
use bough_plugin_tools::ToolName;

/// The catalog name of the tool row.
pub const PLUGIN_NAME: &str = "tool-mcp";
/// The catalog name of the CLI row.
pub const CALL_PLUGIN_NAME: &str = "mcp-call";

/// The tool row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolMcpConfig {
    /// `"mcp__"`. A config field so a deployment can avoid a name clash, not a protocol constant.
    pub prefix: String,
    /// A result longer than this is truncated with a marker before it becomes a tool outcome.
    pub max_result_bytes: usize,
}

/// PURE: the tool name a discovered tool registers under. WP-5.
pub fn tool_name(prefix: &str, info: &McpToolInfo) -> ToolName {
    let _ = (prefix, info);
    todo!("WP-5: `<prefix><server>__<tool>`")
}

/// The live reconciler: what is registered now, keyed by server.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct McpTools {
    cfg: Arc<ToolMcpConfig>,
    mcp: McpHandle,
    registered: parking_lot::Mutex<
        Vec<(
            bough_plugin_mcp::ServerName,
            Vec<bough_kernel::EffectHandle>,
        )>,
    >,
}

impl McpTools {
    /// Register every tool of every server, and reconcile on every later change. WP-5.
    pub async fn reconcile(&self, ctx: &Context) -> Result<(), PluginError> {
        let _ = ctx;
        todo!("WP-5")
    }
}

/// The tool row.
pub struct ToolMcpPlugin;

#[async_trait::async_trait]
impl Plugin for ToolMcpPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ToolMcpConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["mcp", "tools"])
            .union(&bough_kernel::Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-5: non-empty `prefix`, `max_result_bytes > 0`")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: reconcile once, subscribe to `mcp/servers-changed`, register `/mcp`")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// How `bough mcp call` prints.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Print {
    Text,
    Json,
}

/// The CLI row's config. An empty `server` ⇒ the row mounts and DOES NOTHING, which is what makes
/// the headless profile usable without a call (the `exec` row's precedent).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpCallConfig {
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub tool: String,
    /// JSON text as typed.
    #[serde(default)]
    pub args: String,
    pub print: Print,
    pub exit_when_done: bool,
}

/// The CLI row.
pub struct McpCallPlugin;

#[async_trait::async_trait]
impl Plugin for McpCallPlugin {
    const NAME: &'static str = CALL_PLUGIN_NAME;
    type Config = McpCallConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["mcp"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!(
            "WP-5: an empty `server` is legal; a non-empty one needs a `tool` and parseable `args`"
        )
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: call, print, and quit when `exit_when_done`")
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

bough_kernel::register_plugin!(ToolMcpPlugin);
bough_kernel::register_plugin!(McpCallPlugin);
