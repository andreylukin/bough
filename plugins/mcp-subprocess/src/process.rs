//! Invariant: one child fiber owns exactly one OS process, its JSON-RPC framing and its
//! supervision loop. The registration on `ctx.mcp` outlives a restart — that is what keeps the
//! tools registered while the process is down — and it is disposed only when this fiber unloads.

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_mcp::{McpCallResult, McpClient, McpError, McpToolInfo, ServerName};
use bough_plugin_runtime_actions::RuntimeLimits;

use crate::ProcessRow;

/// The child row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpProcessConfig {
    pub row: ProcessRow,
    pub limits: RuntimeLimits,
}

/// Where a supervised process stands.
#[derive(Clone, Debug, PartialEq)]
pub enum ProcessState {
    Starting,
    Up { pid: u32 },
    Restarting { attempt: u32, last: String },
    Quarantined { reason: String },
}

/// One supervised resident process, and the client over it.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct ResidentProcess {
    name: ServerName,
    state: parking_lot::Mutex<ProcessState>,
}

impl ResidentProcess {
    /// Spawn, handshake, and start the supervision loop. WP-7.
    pub async fn start(cfg: Arc<McpProcessConfig>) -> Result<Arc<ResidentProcess>, McpError> {
        let _ = cfg;
        todo!("WP-7")
    }

    /// Where it stands. WP-7.
    pub fn state(&self) -> ProcessState {
        todo!("WP-7")
    }
}

#[async_trait::async_trait]
impl McpClient for ResidentProcess {
    fn server(&self) -> &ServerName {
        &self.name
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        todo!("WP-7: the last successful listing while the process is down")
    }

    async fn call(&self, tool: &str, args: serde_json::Value) -> Result<McpCallResult, McpError> {
        let _ = (tool, args);
        todo!("WP-7: `McpError::Unavailable` while the process is down")
    }

    /// `false` while the process is restarting.
    fn is_ready(&self) -> bool {
        todo!("WP-7")
    }
}

/// The per-process child row.
pub struct McpProcessPlugin;

#[async_trait::async_trait]
impl Plugin for McpProcessPlugin {
    const NAME: &'static str = crate::PROCESS_PLUGIN_NAME;
    type Config = McpProcessConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required([
            "mcp", "ledger", "agents", "actions", "workers", "schedule",
        ])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-7")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7: start, register on ctx.mcp, listen for `bough/actions`, defer the kill")
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}
