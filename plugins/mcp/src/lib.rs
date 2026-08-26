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
    /// server row is disabled.
    ///
    /// The `Added` event is emitted only AFTER the registration is visible, so a listener that
    /// wakes on it and immediately asks for the server's tools always finds it.
    pub async fn server(
        &self,
        ctx: &Context,
        client: Arc<dyn McpClient>,
    ) -> Result<EffectHandle, PluginError> {
        let name = client.server().clone();
        if self
            .0
            .servers
            .lock()
            .iter()
            .any(|(_, c)| c.server() == &name)
        {
            return Err(PluginError::new(
                ctx.entry_id().clone(),
                anyhow::anyhow!("MCP server `{name}` is already registered"),
            ));
        }
        let id = self
            .0
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.0.servers.lock().push((id, client));

        let inner = self.0.clone();
        let disposer_ctx = ctx.clone();
        let gone = name.clone();
        let handle = ctx
            .effect(move |e| async move {
                e.defer_sync(move || {
                    inner.servers.lock().retain(|(i, _)| *i != id);
                    inner.tools.lock().retain(|(s, _)| s != &gone);
                    disposer_ctx.emit::<McpServersChanged>(ServerChange::Removed(gone.clone()));
                });
                Ok(())
            })
            .await?;
        ctx.emit::<McpServersChanged>(ServerChange::Added(name));
        Ok(handle)
    }

    /// Every registered server, sorted.
    pub fn servers(&self) -> Vec<ServerName> {
        let mut out: Vec<ServerName> = self
            .0
            .servers
            .lock()
            .iter()
            .map(|(_, c)| c.server().clone())
            .collect();
        out.sort();
        out
    }

    fn client(&self, server: &ServerName) -> Result<Arc<dyn McpClient>, McpError> {
        self.0
            .servers
            .lock()
            .iter()
            .find(|(_, c)| c.server() == server)
            .map(|(_, c)| c.clone())
            .ok_or_else(|| McpError::UnknownServer(server.clone()))
    }

    fn cached(&self, server: &ServerName) -> Option<Vec<McpToolInfo>> {
        self.0
            .tools
            .lock()
            .iter()
            .find(|(s, _)| s == server)
            .map(|(_, t)| t.clone())
    }

    fn store(&self, server: &ServerName, tools: Vec<McpToolInfo>) {
        let mut held = self.0.tools.lock();
        held.retain(|(s, _)| s != server);
        held.push((server.clone(), tools));
    }

    /// Cached per server; refilled by [`McpHandle::refresh`] and on a cache miss. `None` ⇒ every
    /// server, in server order.
    pub async fn tools(&self, server: Option<&ServerName>) -> Result<Vec<McpToolInfo>, McpError> {
        let wanted: Vec<ServerName> = match server {
            Some(s) => {
                // An unknown name is an error, not an empty list: `tools()` is how a caller finds
                // out a server row never mounted.
                self.client(s)?;
                vec![s.clone()]
            }
            None => self.servers(),
        };
        let mut out = Vec::new();
        for name in wanted {
            match self.cached(&name) {
                Some(t) => out.extend(t),
                None => {
                    let listed = self.client(&name)?.list_tools().await?;
                    self.store(&name, listed.clone());
                    out.extend(listed);
                }
            }
        }
        Ok(out)
    }

    /// One call, whose result carries [`McpHandle::cite_of`]'s cite — the seam's, never the
    /// server's.
    pub async fn call(
        &self,
        r: &McpToolRef,
        args: serde_json::Value,
    ) -> Result<McpCallResult, McpError> {
        let client = self.client(&r.server)?;
        if !client.is_ready() {
            return Err(McpError::Unavailable(r.server.clone()));
        }
        let known = self.tools(Some(&r.server)).await?;
        if !known.iter().any(|t| t.tool == r.tool) {
            return Err(McpError::UnknownTool {
                server: r.server.clone(),
                tool: r.tool.clone(),
            });
        }
        let mut result = client.call(&r.tool, args.clone()).await?;
        // The seam MINTS the citation (module invariant): whatever the server said is discarded.
        let minted = McpHandle::cite_of(r, &args);
        let supplied: Vec<String> = result.cites.iter().map(|c| c.r#ref.to_string()).collect();
        result.cites = vec![minted.clone()];
        invariant::record(invariant::Obs {
            minted: minted.r#ref.to_string(),
            client_supplied: supplied,
            delivered: result.cites.iter().map(|c| c.r#ref.to_string()).collect(),
        });
        Ok(result)
    }

    /// Re-list one server's tools; returns how many it now has.
    pub async fn refresh(&self, server: &ServerName) -> Result<usize, McpError> {
        let listed = self.client(server)?.list_tools().await?;
        let n = listed.len();
        self.store(server, listed);
        Ok(n)
    }

    /// PURE: the cite a call's result carries. Stable across builds for the same args, because
    /// `serde_json::Value` maps are `BTreeMap`s and `to_string` is therefore key-ordered.
    pub fn cite_of(r: &McpToolRef, args: &serde_json::Value) -> Cite {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(args.to_string().as_bytes());
        let digest = format!("{:x}", h.finalize());
        Cite {
            r#ref: bough_plugin_ledger::Ref::new(format!(
                "mcp:{}:{}:{}",
                r.server,
                r.tool,
                &digest[..16]
            )),
            url: None,
        }
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

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        ctx.provide::<Mcp>(McpHandle::new())
            .await
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(McpPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::KernelCore;

    fn ctx() -> Context {
        Context::root(KernelCore::new())
    }

    struct Stub {
        name: ServerName,
        tools: Vec<&'static str>,
        ready: bool,
        listed: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Stub {
        fn new(name: &str, tools: Vec<&'static str>) -> Arc<Stub> {
            Arc::new(Stub {
                name: ServerName::new(name),
                tools,
                ready: true,
                listed: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            })
        }
    }

    #[async_trait::async_trait]
    impl McpClient for Stub {
        fn server(&self) -> &ServerName {
            &self.name
        }
        async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
            self.listed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self
                .tools
                .iter()
                .map(|t| McpToolInfo {
                    server: self.name.clone(),
                    tool: (*t).to_string(),
                    description: format!("the {t} tool"),
                    input_schema: serde_json::json!({ "type": "object" }),
                })
                .collect())
        }
        async fn call(
            &self,
            tool: &str,
            _args: serde_json::Value,
        ) -> Result<McpCallResult, McpError> {
            Ok(McpCallResult {
                content: format!("ran {tool}"),
                value: None,
                // A server that tries to cite for itself: the seam must discard this.
                cites: vec![Cite {
                    r#ref: bough_plugin_ledger::Ref::new("gh:o/r#1"),
                    url: None,
                }],
                is_error: tool == "boom",
            })
        }
        fn is_ready(&self) -> bool {
            self.ready
        }
    }

    #[test]
    fn cite_of_is_pure_and_stable_across_two_builds_of_the_same_args() {
        let r = McpToolRef {
            server: ServerName::new("fixture"),
            tool: "echo".into(),
        };
        let a = McpHandle::cite_of(&r, &serde_json::json!({ "b": 2, "a": 1 }));
        let b = McpHandle::cite_of(&r, &serde_json::json!({ "a": 1, "b": 2 }));
        assert_eq!(a, b, "key order is not part of the identity");
        assert!(
            a.r#ref.to_string().starts_with("mcp:fixture:echo:"),
            "{}",
            a.r#ref
        );
        assert_eq!(a.r#ref.to_string().len(), "mcp:fixture:echo:".len() + 16);
        let other = McpHandle::cite_of(&r, &serde_json::json!({ "a": 2 }));
        assert_ne!(a, other, "different args, different cite");
    }

    #[tokio::test]
    async fn registering_a_server_is_an_effect_whose_disposer_withdraws_it() {
        let ctx = ctx();
        let mcp = McpHandle::new();
        let seen = Arc::new(parking_lot::Mutex::new(Vec::<ServerChange>::new()));
        let sink = seen.clone();
        ctx.on::<McpServersChanged, _, _>(move |c| {
            let sink = sink.clone();
            async move {
                sink.lock().push(c);
            }
        })
        .await
        .unwrap();

        let handle = mcp
            .server(&ctx, Stub::new("fixture", vec!["echo"]))
            .await
            .unwrap();
        assert_eq!(mcp.servers(), vec![ServerName::new("fixture")]);
        handle.dispose().await;
        assert!(
            mcp.servers().is_empty(),
            "the disposer withdraws the server"
        );
        // Give the emitted events a turn to land.
        tokio::task::yield_now().await;
        let seen = seen.lock().clone();
        assert_eq!(
            seen,
            vec![
                ServerChange::Added(ServerName::new("fixture")),
                ServerChange::Removed(ServerName::new("fixture")),
            ]
        );
    }

    #[tokio::test]
    async fn tools_caches_and_refresh_refills() {
        let ctx = ctx();
        let mcp = McpHandle::new();
        let stub = Stub::new("fixture", vec!["echo", "boom"]);
        let listed = stub.listed.clone();
        mcp.server(&ctx, stub).await.unwrap();

        assert_eq!(mcp.tools(None).await.unwrap().len(), 2);
        assert_eq!(listed.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(mcp.tools(None).await.unwrap().len(), 2);
        assert_eq!(
            listed.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the second read is served from the cache"
        );
        assert_eq!(mcp.refresh(&ServerName::new("fixture")).await.unwrap(), 2);
        assert_eq!(
            listed.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "refresh always goes to the server"
        );
    }

    #[tokio::test]
    async fn an_unknown_server_and_an_unknown_tool_are_the_right_typed_errors() {
        let ctx = ctx();
        let mcp = McpHandle::new();
        mcp.server(&ctx, Stub::new("fixture", vec!["echo"]))
            .await
            .unwrap();

        let nowhere = McpToolRef {
            server: ServerName::new("nope"),
            tool: "echo".into(),
        };
        assert!(matches!(
            mcp.call(&nowhere, serde_json::json!({})).await,
            Err(McpError::UnknownServer(s)) if s == ServerName::new("nope")
        ));
        let missing = McpToolRef {
            server: ServerName::new("fixture"),
            tool: "absent".into(),
        };
        assert!(matches!(
            mcp.call(&missing, serde_json::json!({})).await,
            Err(McpError::UnknownTool { tool, .. }) if tool == "absent"
        ));
        assert!(matches!(
            mcp.tools(Some(&ServerName::new("nope"))).await,
            Err(McpError::UnknownServer(_))
        ));
    }

    #[tokio::test]
    async fn a_calls_result_carries_the_seams_cite_and_never_the_servers() {
        let ctx = ctx();
        let mcp = McpHandle::new();
        mcp.server(&ctx, Stub::new("fixture", vec!["echo", "boom"]))
            .await
            .unwrap();
        let r = McpToolRef {
            server: ServerName::new("fixture"),
            tool: "echo".into(),
        };
        let args = serde_json::json!({ "text": "hi" });
        let out = mcp.call(&r, args.clone()).await.unwrap();
        assert_eq!(out.cites, vec![McpHandle::cite_of(&r, &args)]);
        assert!(!out.is_error);

        let boom = McpToolRef {
            server: ServerName::new("fixture"),
            tool: "boom".into(),
        };
        assert!(
            mcp.call(&boom, serde_json::json!({}))
                .await
                .unwrap()
                .is_error
        );
    }
}
