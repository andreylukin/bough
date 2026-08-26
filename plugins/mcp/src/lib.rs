//! Invariant: this crate is the mcp SERVICE DEFINITION (§6, §0.2). It owns the `mcp` key, the
//! client contract, the server registry, the tool cache and the CITE — and no transport.
//!
//! The cite is MINTED BY THE SEAM, not by the server: `mcp:<server>:<tool>:<sha256(args)[..16]>`.
//! §6 says a pull's results enter the trajectory as cited evidence, and a foreign server cannot be
//! trusted to say what it was asked, so the seam says it instead.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{
    ConfigError, Context, EffectHandle, EmitEvent, InvariantSpec, Plugin, PluginError, ServiceKey,
};
use bough_plugin_ledger::Cite;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "mcp";

/// The `mcp` service key.
pub struct Mcp;

impl ServiceKey for Mcp {
    type Value = McpHandle;
    const NAME: &'static str = "mcp";
}

bough_util::brand_id!(
    /// One MCP server, as its config row named it.
    pub struct ServerName;
);

/// One tool on one server.
#[derive(Clone, Debug, PartialEq)]
pub struct McpToolRef {
    pub server: ServerName,
    pub tool: String,
}

/// A discovered tool.
#[derive(Clone, Debug)]
pub struct McpToolInfo {
    pub server: ServerName,
    pub tool: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// What one call returned.
#[derive(Clone, Debug, PartialEq)]
pub struct McpCallResult {
    pub content: String,
    pub value: Option<serde_json::Value>,
    /// What makes a pull EVIDENCE (§6). Minted by the seam.
    pub cites: Vec<Cite>,
    pub is_error: bool,
}

/// What an MCP client does.
#[async_trait::async_trait]
pub trait McpClient: Send + Sync + 'static {
    fn server(&self) -> &ServerName;
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError>;
    async fn call(&self, tool: &str, args: serde_json::Value) -> Result<McpCallResult, McpError>;
    /// A resident subprocess host answers `false` while its process is restarting; `tool-mcp` keeps
    /// the tool registered and the call fails with [`McpError::Unavailable`] rather than the tool
    /// vanishing mid-wake.
    fn is_ready(&self) -> bool {
        true
    }
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct McpHandle(pub Arc<McpInner>);

/// The seam's live state: the server registry and the per-server tool cache.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct McpInner {
    servers: parking_lot::Mutex<Vec<(u64, Arc<dyn McpClient>)>>,
    tools: parking_lot::Mutex<Vec<(ServerName, Vec<McpToolInfo>)>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl McpHandle {
    /// An empty seam.
    pub fn new() -> McpHandle {
        McpHandle(Arc::new(McpInner {
            servers: parking_lot::Mutex::new(Vec::new()),
            tools: parking_lot::Mutex::new(Vec::new()),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }))
    }

    /// Register a server. An EFFECT: the disposer withdraws it AND emits
    /// [`ServerChange::Removed`], which is what makes `tool-mcp` unregister its tools when a
    /// server row is disabled. WP-5.
    pub async fn server(
        &self,
        ctx: &Context,
        client: Arc<dyn McpClient>,
    ) -> Result<EffectHandle, PluginError> {
        let _ = (ctx, client);
        todo!("WP-5")
    }

    /// Every registered server, sorted. WP-5.
    pub fn servers(&self) -> Vec<ServerName> {
        todo!("WP-5")
    }

    /// Cached per server; refreshed on `mcp/servers-changed` and on an explicit
    /// [`McpHandle::refresh`]. `None` ⇒ every server. WP-5.
    pub async fn tools(&self, server: Option<&ServerName>) -> Result<Vec<McpToolInfo>, McpError> {
        let _ = server;
        todo!("WP-5")
    }

    /// One call, whose result carries [`McpHandle::cite_of`]'s cite. WP-5.
    pub async fn call(
        &self,
        r: &McpToolRef,
        args: serde_json::Value,
    ) -> Result<McpCallResult, McpError> {
        let _ = (r, args);
        todo!("WP-5")
    }

    /// Re-list one server's tools; returns how many it now has. WP-5.
    pub async fn refresh(&self, server: &ServerName) -> Result<usize, McpError> {
        let _ = server;
        todo!("WP-5")
    }

    /// PURE: the cite a call's result carries. Stable across builds for the same args. WP-5.
    pub fn cite_of(r: &McpToolRef, args: &serde_json::Value) -> Cite {
        let _ = (r, args);
        todo!("WP-5: `mcp:<server>:<tool>:<sha256(canonical args)[..16]>`")
    }
}

impl Default for McpHandle {
    fn default() -> Self {
        McpHandle::new()
    }
}

/// What changed about the server set.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerChange {
    Added(ServerName),
    Removed(ServerName),
}

/// `mcp/servers-changed` — EMIT.
pub struct McpServersChanged;

impl EmitEvent for McpServersChanged {
    const NAME: &'static str = "mcp/servers-changed";
    type Payload = ServerChange;
}

/// What the mcp seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("no MCP server named `{0}`")]
    UnknownServer(ServerName),
    #[error("server `{server}` has no tool `{tool}`")]
    UnknownTool { server: ServerName, tool: String },
    #[error("server `{0}` is not ready")]
    Unavailable(ServerName),
    #[error("transport: {0}")]
    Transport(String),
    #[error("server error: {0}")]
    Server(String),
}

/// No configuration: the servers belong to the Provider row, not to the seam.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {}

/// The Service Definition row.
pub struct McpPlugin;

#[async_trait::async_trait]
impl Plugin for McpPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = McpConfig;

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: provide `mcp` with an empty seam")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(McpPlugin);
