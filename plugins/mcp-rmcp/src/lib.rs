//! Invariant: ONE CHILD ENTRY PER SERVER. The parent row reads `servers:` and mounts one
//! `mcp-server` child per enabled row through `ctx.mount`; children are effects of the parent, so
//! unloading the parent cascades and disabling one server's row removes exactly that server and
//! its tools (§0.3, SWAP).
//!
//! rmcp 3.x wants reqwest 0.13 and `bough-llm` holds 0.12. The dual-version arrangement recorded in
//! Phase 0 STANDS, bridged through `OAuthHttpClient`, and both are pinned to a minor (§13).

pub mod invariant;
pub mod server;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};

/// The catalog name of the parent row.
pub const PLUGIN_NAME: &str = "mcp-rmcp";
/// The catalog name of the per-server CHILD row.
pub const SERVER_PLUGIN_NAME: &str = "mcp-server";

/// The parent row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpRmcpConfig {
    pub servers: Vec<ServerRow>,
    pub connect_timeout_ms: u64,
    pub call_timeout_ms: u64,
}

/// One server, as the bundle spells it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerRow {
    pub name: String,
    pub transport: Transport,
    #[serde(default)]
    pub disabled: bool,
}

/// How to reach a server.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// PURE: the child entry one enabled [`ServerRow`] mounts as. WP-5.
pub fn child_entry(
    parent: &bough_kernel::EntryId,
    row: &ServerRow,
    connect_timeout_ms: u64,
    call_timeout_ms: u64,
) -> Result<bough_kernel::Entry, PluginError> {
    let _ = (parent, row, connect_timeout_ms, call_timeout_ms);
    todo!("WP-5: one child Entry per server, id `<parent>.<name>`, plugin `mcp-server`")
}

/// The parent row.
pub struct McpRmcpPlugin;

#[async_trait::async_trait]
impl Plugin for McpRmcpPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = McpRmcpConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["mcp"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-5: unique names, non-empty command / parseable url, non-zero timeouts")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: mount one child entry per enabled server row")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(McpRmcpPlugin);
bough_kernel::register_plugin!(server::McpServerPlugin);
