//! Invariant: one child fiber owns exactly one rmcp client, and its registration on `ctx.mcp` is
//! an effect of THIS fiber. Nothing else in the tree holds the client, so unloading this child is
//! the only way the server leaves — and it always removes exactly one server.

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_mcp::{McpCallResult, McpClient, McpError, McpToolInfo, ServerName};

use crate::Transport;

/// The child row's config: one server row, plus the parent's timeouts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: Transport,
    pub connect_timeout_ms: u64,
    pub call_timeout_ms: u64,
}

/// One rmcp client.
pub struct RmcpClient {
    name: ServerName,
    // The rmcp running service. WP-5.
}

impl RmcpClient {
    /// Connect (stdio child process or HTTP) under `connect_timeout_ms`. WP-5.
    pub async fn connect(cfg: Arc<McpServerConfig>) -> Result<Arc<RmcpClient>, McpError> {
        let _ = cfg;
        todo!("WP-5")
    }
}

#[async_trait::async_trait]
impl McpClient for RmcpClient {
    fn server(&self) -> &ServerName {
        &self.name
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        todo!("WP-5")
    }

    async fn call(&self, tool: &str, args: serde_json::Value) -> Result<McpCallResult, McpError> {
        let _ = (tool, args);
        todo!(
            "WP-5: bounded by `call_timeout_ms`; `is_error` from the MCP result, not from a panic"
        )
    }
}

/// The per-server child row.
pub struct McpServerPlugin;

#[async_trait::async_trait]
impl Plugin for McpServerPlugin {
    const NAME: &'static str = crate::SERVER_PLUGIN_NAME;
    type Config = McpServerConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["mcp"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-5")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: connect, register on ctx.mcp as an effect, defer the shutdown")
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}
