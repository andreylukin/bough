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

use bough_kernel::{ConfigError, Context, EffectHandle, InvariantSpec, Plugin, PluginError};
use bough_plugin_mcp::{
    Mcp, McpError, McpHandle, McpServersChanged, McpToolInfo, McpToolRef, ServerChange, ServerName,
};
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools, ToolsHandle,
};

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

/// PURE: the tool name a discovered tool registers under.
pub fn tool_name(prefix: &str, info: &McpToolInfo) -> ToolName {
    ToolName::new(format!("{prefix}{}__{}", info.server, info.tool))
}

/// PURE: `text`, cut at `max` bytes on a char boundary, with a marker saying so.
pub fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n[truncated: {} of {} bytes shown]",
        &text[..cut],
        cut,
        text.len()
    )
}

/// One MCP tool, as the model calls it.
pub struct McpTool {
    mcp: McpHandle,
    r: McpToolRef,
    max_result_bytes: usize,
}

#[async_trait::async_trait]
impl Tool for McpTool {
    /// P6-D8: the seam cannot know what a foreign server does, so no MCP tool is ever parallel.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        match self.mcp.call(&self.r, call.args.clone()).await {
            Ok(result) => {
                if result.is_error {
                    return Err(ToolFailure {
                        kind: FailureClass::Error,
                        message: truncate(&result.content, self.max_result_bytes),
                    });
                }
                Ok(ToolOutcome {
                    content: truncate(&result.content, self.max_result_bytes),
                    value: result.value,
                    cites: result.cites,
                    concludes_wake: false,
                })
            }
            Err(e) => Err(ToolFailure {
                kind: match e {
                    bough_plugin_mcp::McpError::UnknownServer(_)
                    | bough_plugin_mcp::McpError::UnknownTool { .. } => FailureClass::NotFound,
                    bough_plugin_mcp::McpError::Unavailable(_) => FailureClass::Blocked,
                    _ => FailureClass::Error,
                },
                message: e.to_string(),
            }),
        }
    }
}

/// PURE: the spec a discovered tool registers as.
pub fn spec_for(cfg: &ToolMcpConfig, mcp: &McpHandle, info: &McpToolInfo) -> ToolSpec {
    ToolSpec {
        name: tool_name(&cfg.prefix, info),
        description: info.description.clone(),
        input_schema: schemars::Schema::try_from(info.input_schema.clone())
            // A server that answered with a non-object schema still gets a legal one, or the whole
            // server would vanish because of one malformed tool.
            .unwrap_or_else(|_| {
                schemars::Schema::try_from(serde_json::json!({ "type": "object" }))
                    .expect("an empty object schema is legal")
            }),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool: Arc::new(McpTool {
            mcp: mcp.clone(),
            r: McpToolRef {
                server: info.server.clone(),
                tool: info.tool.clone(),
            },
            max_result_bytes: cfg.max_result_bytes,
        }),
    }
}

/// The live reconciler: what is registered now, keyed by server.
pub struct McpTools {
    cfg: Arc<ToolMcpConfig>,
    mcp: McpHandle,
    tools: ToolsHandle,
    /// Whose observations these are, so the invariant's stream is per-LIFE (§0.3).
    fiber: bough_kernel::FiberUid,
    registered: parking_lot::Mutex<Vec<(ServerName, Vec<EffectHandle>)>>,
}

impl McpTools {
    pub fn new(
        cfg: Arc<ToolMcpConfig>,
        mcp: McpHandle,
        tools: ToolsHandle,
        fiber: bough_kernel::FiberUid,
    ) -> Arc<McpTools> {
        Arc::new(McpTools {
            cfg,
            mcp,
            tools,
            fiber,
            registered: parking_lot::Mutex::new(Vec::new()),
        })
    }

    /// The servers this row currently holds registrations for.
    pub fn held(&self) -> Vec<ServerName> {
        let mut out: Vec<ServerName> = self
            .registered
            .lock()
            .iter()
            .map(|(s, _)| s.clone())
            .collect();
        out.sort();
        out
    }

    /// Register every tool of every server, and drop what no longer has a server. Idempotent: a
    /// server already held is left alone, so a second reconcile does not churn the registry.
    pub async fn reconcile(&self, ctx: &Context) -> Result<(), PluginError> {
        let live = self.mcp.servers();

        // Withdraw first: a server that is gone must lose its tools even if a later add fails.
        let gone: Vec<(ServerName, Vec<EffectHandle>)> = {
            let mut held = self.registered.lock();
            let (gone, kept) = held.drain(..).partition(|(s, _)| !live.contains(s));
            *held = kept;
            gone
        };
        for (server, handles) in gone {
            for h in handles {
                h.dispose().await;
            }
            invariant::record(
                self.fiber,
                invariant::Obs::Withdrawn {
                    server: server.to_string(),
                },
            );
        }

        for server in live {
            if self.held().contains(&server) {
                continue;
            }
            let infos = match self.mcp.tools(Some(&server)).await {
                Ok(t) => t,
                // A server that cannot be listed is reported, never silently dropped: the row
                // stays mounted and a later `mcp/servers-changed` retries.
                Err(e) => {
                    tracing::warn!(%server, error = %e, "listing an MCP server's tools failed");
                    continue;
                }
            };
            let mut handles = Vec::new();
            for info in &infos {
                let spec = spec_for(&self.cfg, &self.mcp, info);
                let name = spec.name.to_string();
                match self.tools.register(ctx, spec).await {
                    Ok(h) => {
                        invariant::record(
                            self.fiber,
                            invariant::Obs::Registered {
                                server: server.to_string(),
                                name,
                            },
                        );
                        handles.push(h);
                    }
                    Err(e) => tracing::warn!(%server, error = %e, "registering an MCP tool failed"),
                }
            }
            self.registered.lock().push((server, handles));
        }
        Ok(())
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
        if cfg.prefix.is_empty() {
            return Err(ConfigError::Rejected {
                detail: "`prefix` must be non-empty".into(),
            });
        }
        if cfg.max_result_bytes == 0 {
            return Err(ConfigError::Rejected {
                detail: "`max_result_bytes` must be greater than zero".into(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let err = |e| PluginError::new(ctx.entry_id().clone(), e);
        let mcp = ctx.get::<Mcp>().map_err(err)?;
        let tools = ctx.get::<Tools>().map_err(err)?;
        let mine = ctx.fiber_uid();
        let state = McpTools::new(cfg.clone(), (*mcp).clone(), (*tools).clone(), mine);
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(mine));
            Ok(())
        })
        .await?;

        state.reconcile(&ctx).await?;

        let listener = state.clone();
        let listener_ctx = ctx.clone();
        ctx.on::<McpServersChanged, _, _>(move |_change: ServerChange| {
            let listener = listener.clone();
            let listener_ctx = listener_ctx.clone();
            async move {
                if let Err(e) = listener.reconcile(&listener_ctx).await {
                    tracing::warn!(error = %e, "reconciling MCP tools failed");
                }
            }
        })
        .await?;

        if let Some(commands) = ctx
            .try_get::<bough_plugin_commands::Commands>()
            .map_err(err)?
        {
            command::register(&ctx, &commands, &mcp).await?;
        }
        Ok(())
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

/// PURE: what one CLI row asks for. `None` ⇒ the row is inert.
pub fn planned_call(cfg: &McpCallConfig) -> Option<(McpToolRef, serde_json::Value)> {
    if cfg.server.is_empty() {
        return None;
    }
    let args = if cfg.args.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&cfg.args).unwrap_or(serde_json::Value::Null)
    };
    Some((
        McpToolRef {
            server: ServerName::new(&cfg.server),
            tool: cfg.tool.clone(),
        },
        args,
    ))
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
        // An empty `server` is LEGAL: the inert row is the headless profile's normal state.
        if cfg.server.is_empty() {
            return Ok(());
        }
        if cfg.tool.is_empty() {
            return Err(ConfigError::Rejected {
                detail: "a `server` was named but no `tool`".into(),
            });
        }
        if !cfg.args.trim().is_empty() {
            serde_json::from_str::<serde_json::Value>(&cfg.args).map_err(|e| {
                ConfigError::Rejected {
                    detail: format!("`args` is not JSON: {e}"),
                }
            })?;
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let Some((r, args)) = planned_call(&cfg) else {
            // Mounted and inert.
            return Ok(());
        };
        let mcp = ctx
            .get::<Mcp>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        let entry = ctx.entry_id().clone();
        let spawn_ctx = ctx.clone();
        let exit_when_done = cfg.exit_when_done;
        let print = cfg.print;
        spawn_ctx.effect_spawn(move |_e| async move {
            // A row's `apply` runs while the tree is still converging, and the mcp Provider may not
            // have connected its servers yet. Row order carries no load semantics (§0.2), so the
            // wait belongs here — bounded, so a genuinely absent server is an error and not a hang
            // (`exec-headless`'s `wait_for_factory` is the precedent).
            let code = match wait_for_server(&mcp, &r.server).await {
                Ok(()) => match mcp.call(&r, args).await {
                    Ok(out) => {
                        println!("{}", render(print, &out));
                        u8::from(out.is_error)
                    }
                    Err(e) => {
                        println!("error: {e}");
                        1
                    }
                },
                Err(e) => {
                    println!("error: {e}");
                    1
                }
            };
            if exit_when_done {
                if let Some(kernel) = ctx.kernel() {
                    kernel.request_exit(code);
                }
            }
            let _ = entry;
            Ok(())
        });
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

/// How long the CLI row waits for the named server to finish connecting.
///
/// A protocol bound on a startup race, not a deployment value (§0.2).
const SERVER_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// Wait until `server` is listed on the seam, or say it is not there.
async fn wait_for_server(mcp: &McpHandle, server: &ServerName) -> Result<(), McpError> {
    let deadline = std::time::Instant::now() + SERVER_WAIT;
    loop {
        if mcp.servers().iter().any(|s| s == server) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(McpError::UnknownServer(server.clone()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// PURE: one call's result as the CLI prints it.
pub fn render(print: Print, out: &bough_plugin_mcp::McpCallResult) -> String {
    match print {
        Print::Text => {
            let cites: Vec<String> = out.cites.iter().map(|c| c.r#ref.to_string()).collect();
            format!("{}\ncites: {}", out.content, cites.join(", "))
        }
        Print::Json => serde_json::json!({
            "content": out.content,
            "value": out.value,
            "is_error": out.is_error,
            "cites": out.cites.iter().map(|c| c.r#ref.to_string()).collect::<Vec<_>>(),
        })
        .to_string(),
    }
}

bough_kernel::register_plugin!(ToolMcpPlugin);
bough_kernel::register_plugin!(McpCallPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn info(server: &str, tool: &str) -> McpToolInfo {
        McpToolInfo {
            server: ServerName::new(server),
            tool: tool.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({ "type": "object" }),
        }
    }

    #[test]
    fn a_discovered_tool_registers_under_the_prefixed_double_underscore_name() {
        assert_eq!(
            tool_name("mcp__", &info("fixture", "echo")).as_str(),
            "mcp__fixture__echo"
        );
    }

    #[test]
    fn truncation_marks_itself_and_cuts_on_a_char_boundary() {
        assert_eq!(truncate("hello", 10), "hello");
        let cut = truncate("héllo world", 3);
        assert!(cut.starts_with("hé"), "{cut}");
        assert!(cut.contains("truncated"));
    }

    #[test]
    fn an_empty_server_makes_the_cli_row_inert() {
        let inert = McpCallConfig {
            server: String::new(),
            tool: String::new(),
            args: String::new(),
            print: Print::Text,
            exit_when_done: true,
        };
        assert!(McpCallPlugin::validate(&inert).is_ok());
        assert!(planned_call(&inert).is_none());
    }

    #[test]
    fn a_named_server_needs_a_tool_and_json_args() {
        let no_tool = McpCallConfig {
            server: "fixture".into(),
            tool: String::new(),
            args: String::new(),
            print: Print::Text,
            exit_when_done: true,
        };
        assert!(McpCallPlugin::validate(&no_tool)
            .unwrap_err()
            .to_string()
            .contains("no `tool`"));

        let bad_json = McpCallConfig {
            args: "{oops".into(),
            tool: "echo".into(),
            ..no_tool.clone()
        };
        assert!(McpCallPlugin::validate(&bad_json)
            .unwrap_err()
            .to_string()
            .contains("not JSON"));

        let good = McpCallConfig {
            args: "{\"text\":\"hi\"}".into(),
            tool: "echo".into(),
            ..no_tool
        };
        assert!(McpCallPlugin::validate(&good).is_ok());
        let (r, args) = planned_call(&good).unwrap();
        assert_eq!(r.tool, "echo");
        assert_eq!(args, serde_json::json!({ "text": "hi" }));
    }

    #[test]
    fn validate_refuses_an_empty_prefix_and_a_zero_budget() {
        assert!(ToolMcpPlugin::validate(&ToolMcpConfig {
            prefix: String::new(),
            max_result_bytes: 10
        })
        .is_err());
        assert!(ToolMcpPlugin::validate(&ToolMcpConfig {
            prefix: "mcp__".into(),
            max_result_bytes: 0
        })
        .is_err());
    }
}
