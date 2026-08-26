//! Invariant: RESTARTING IS INDEPENDENT. One process crashing never touches another child entry or
//! the parent. While a process is down its client answers `is_ready() == false`, so its tools STAY
//! REGISTERED and a call fails with `McpError::Unavailable` instead of the tool vanishing mid-wake.
//!
//! A JSON-RPC NOTIFICATION named `bough/actions` whose params are `{ actions: [RuntimeAction] }` is
//! journaled through `runtime_actions::execute_all` — §9's "actions they emit THROUGH the plugin
//! API are code-enforced and journaled like ward actions".
//!
//! What a process does DIRECTLY, as a process running as Andrey, is trusted config and outside the
//! boundary's scope. §9 flags this, and so does this comment.

pub mod invariant;
pub mod process;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_runtime_actions::RuntimeLimits;

/// The catalog name of the parent row.
pub const PLUGIN_NAME: &str = "mcp-subprocess";
/// The catalog name of the per-process CHILD row.
pub const PROCESS_PLUGIN_NAME: &str = "mcp-process";

/// The JSON-RPC notification a resident plugin emits actions through.
pub const ACTIONS_NOTIFICATION: &str = "bough/actions";

/// The parent row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpSubprocessConfig {
    pub processes: Vec<ProcessRow>,
    pub limits: RuntimeLimits,
}

/// One resident process.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessRow {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Restart policy. Backoff is jittered (backon), capped, and a process that dies faster than
    /// `min_uptime_ms` `max_restarts` times in a row is QUARANTINED and reported.
    pub max_restarts: u32,
    pub min_uptime_ms: u64,
    pub restart_delay_ms: u64,
}

/// The parent row.
pub struct McpSubprocessPlugin;

#[async_trait::async_trait]
impl Plugin for McpSubprocessPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = McpSubprocessConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required([
            "mcp", "ledger", "agents", "actions", "workers", "schedule",
        ])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-7: unique names, non-empty command, non-zero restart policy")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7: mount one child entry per process row")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(McpSubprocessPlugin);
bough_kernel::register_plugin!(process::McpProcessPlugin);
