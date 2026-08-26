//! Invariant: one child fiber owns exactly one rmcp client, and its registration on `ctx.mcp` is
//! an effect of THIS fiber. Nothing else in the tree holds the client, so unloading this child is
//! the only way the server leaves — and it always removes exactly one server.

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_mcp::{Mcp, McpCallResult, McpClient, McpError, McpToolInfo, ServerName};
use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::ServiceExt;

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
    service: RunningService<RoleClient, ()>,
    call_timeout: Duration,
}

impl RmcpClient {
    /// Connect (stdio child process or HTTP) under `connect_timeout_ms`.
    pub async fn connect(cfg: Arc<McpServerConfig>) -> Result<Arc<RmcpClient>, McpError> {
        let name = ServerName::new(&cfg.name);
        let connect = Duration::from_millis(cfg.connect_timeout_ms);
        let service = match &cfg.transport {
            Transport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let mut c = tokio::process::Command::new(command);
                c.args(args);
                for (k, v) in env {
                    c.env(k, v);
                }
                if let Some(dir) = cwd {
                    c.current_dir(dir);
                }
                let transport = TokioChildProcess::new(c)
                    .map_err(|e| McpError::Transport(format!("spawning `{command}`: {e}")))?;
                tokio::time::timeout(connect, ().serve(transport))
                    .await
                    .map_err(|_| McpError::Transport(format!("`{command}` did not initialize")))?
                    .map_err(|e| McpError::Transport(e.to_string()))?
            }
            Transport::Http { url, headers } => {
                let mut config =
                    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                        url.clone(),
                    );
                for (k, v) in headers {
                    let key: http::HeaderName = k
                        .parse()
                        .map_err(|_| McpError::Transport(format!("bad header name `{k}`")))?;
                    let value: http::HeaderValue = v
                        .parse()
                        .map_err(|_| McpError::Transport(format!("bad header value for `{k}`")))?;
                    config.custom_headers.insert(key, value);
                }
                let transport = StreamableHttpClientTransport::from_config(config);
                tokio::time::timeout(connect, ().serve(transport))
                    .await
                    .map_err(|_| McpError::Transport(format!("`{url}` did not initialize")))?
                    .map_err(|e| McpError::Transport(e.to_string()))?
            }
        };
        Ok(Arc::new(RmcpClient {
            name,
            service,
            call_timeout: Duration::from_millis(cfg.call_timeout_ms),
        }))
    }

    /// Shut the client (and, for stdio, its child process) down.
    pub async fn shutdown(self: Arc<RmcpClient>) {
        if let Ok(client) = Arc::try_unwrap(self) {
            let _ = client.service.cancel().await;
        }
    }
}

#[async_trait::async_trait]
impl McpClient for RmcpClient {
    fn server(&self) -> &ServerName {
        &self.name
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let listed = tokio::time::timeout(self.call_timeout, self.service.list_all_tools())
            .await
            .map_err(|_| McpError::Transport(format!("`{}` timed out listing tools", self.name)))?
            .map_err(|e| McpError::Server(e.to_string()))?;
        Ok(listed
            .into_iter()
            .map(|t| McpToolInfo {
                server: self.name.clone(),
                tool: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: serde_json::Value::Object((*t.input_schema).clone()),
            })
            .collect())
    }

    async fn call(&self, tool: &str, args: serde_json::Value) -> Result<McpCallResult, McpError> {
        let arguments = match args {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => {
                return Err(McpError::Server(format!(
                    "MCP arguments must be a JSON object, got {other}"
                )))
            }
        };
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        let out = tokio::time::timeout(self.call_timeout, self.service.call_tool(params))
            .await
            .map_err(|_| McpError::Transport(format!("`{}`/`{tool}` timed out", self.name)))?
            .map_err(|e| McpError::Server(e.to_string()))?;
        let content = out
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(McpCallResult {
            content,
            value: out.structured_content,
            // The SEAM mints the cite; the client never does.
            cites: Vec::new(),
            // From the MCP result, never from a transport panic.
            is_error: out.is_error.unwrap_or(false),
        })
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
        crate::validate_row_name(&cfg.name)?;
        crate::validate_transport(&cfg.name, &cfg.transport)?;
        if cfg.connect_timeout_ms == 0 || cfg.call_timeout_ms == 0 {
            return Err(ConfigError::Rejected {
                detail: "timeouts must be greater than zero".into(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let mcp = ctx
            .get::<Mcp>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        let client = RmcpClient::connect(cfg.clone())
            .await
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), anyhow::Error::new(e)))?;
        // The shutdown is deferred BEFORE the registration, so unwinding (LIFO) withdraws the
        // server from the seam first and only then kills the child process.
        let shutdown = client.clone();
        ctx.effect(move |e| async move {
            let shutdown = shutdown.clone();
            e.defer(move || {
                let shutdown = shutdown.clone();
                Box::pin(async move {
                    shutdown.shutdown().await;
                })
            });
            Ok(())
        })
        .await?;
        mcp.server(&ctx, client).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}
