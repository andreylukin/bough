//! Invariant: one child fiber owns exactly one OS process, its JSON-RPC framing and its
//! supervision loop. The registration on `ctx.mcp` outlives a restart — that is what keeps the
//! tools registered while the process is down — and it is disposed only when this fiber unloads.
//!
//! While the process is down, `is_ready()` is `false`, `list_tools()` answers from the LAST
//! SUCCESSFUL LISTING, and `call()` fails with [`McpError::Unavailable`]. A tool that vanished
//! mid-wake would make the model's own tool list a lie about what it may ask for; a tool that
//! answers "not right now" does not.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use backon::BackoffBuilder;
use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_mcp::{McpCallResult, McpClient, McpError, McpToolInfo, ServerName};
use bough_plugin_runtime_actions::{RuntimeAction, RuntimeLimits};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::jsonrpc::{self, Incoming};
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

/// What a resident process's `bough/actions` notification is handed to.
///
/// The host wires this to `runtime_actions::execute_all`; a test wires it to a recorder. Either
/// way the process RETURNS actions and performs none — §9's "actions they emit THROUGH the plugin
/// API are code-enforced and journaled like ward actions".
pub type ActionsCallback = Arc<dyn Fn(Vec<RuntimeAction>) + Send + Sync>;

/// One supervised resident process, and the client over it.
pub struct ResidentProcess {
    name: ServerName,
    cfg: Arc<McpProcessConfig>,
    state: parking_lot::Mutex<ProcessState>,
    /// The LAST SUCCESSFUL listing. Survives a restart on purpose.
    tools: parking_lot::Mutex<Vec<McpToolInfo>>,
    /// The line sink of the CURRENT child, replaced on every respawn.
    outbox: parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    pending: parking_lot::Mutex<
        BTreeMap<u64, tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>>,
    >,
    next_id: AtomicU64,
    /// How many times it has been respawned. The test reads it.
    spawns: AtomicU32,
    ready: AtomicBool,
    on_actions: ActionsCallback,
    /// How long one request waits for its response before the call is Unavailable.
    call_timeout: Duration,
}

impl ResidentProcess {
    /// Build the client and start supervising. Returns as soon as the FIRST handshake has settled
    /// one way or the other, so a caller can register a client that is already `is_ready()`.
    ///
    /// The returned [`tokio::task::JoinHandle`] is the supervisor; the caller defers its abort, and
    /// aborting it is what kills the child (every spawn is `kill_on_drop`).
    pub async fn start(
        cfg: Arc<McpProcessConfig>,
        on_actions: ActionsCallback,
    ) -> (Arc<ResidentProcess>, tokio::task::JoinHandle<()>) {
        let me = Arc::new(ResidentProcess {
            name: ServerName::new(cfg.row.name.clone()),
            cfg: cfg.clone(),
            state: parking_lot::Mutex::new(ProcessState::Starting),
            tools: parking_lot::Mutex::new(Vec::new()),
            outbox: parking_lot::Mutex::new(None),
            pending: parking_lot::Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            spawns: AtomicU32::new(0),
            ready: AtomicBool::new(false),
            on_actions,
            call_timeout: Duration::from_millis(cfg.row.min_uptime_ms.max(2000)),
        });
        let (settled_tx, settled_rx) = tokio::sync::oneshot::channel::<()>();
        let sup = Arc::clone(&me);
        let handle = tokio::spawn(async move { sup.supervise(settled_tx).await });
        // Bounded: a process that never handshakes must not hold up the whole tree's boot.
        let _ = tokio::time::timeout(Duration::from_secs(10), settled_rx).await;
        (me, handle)
    }

    /// Where it stands.
    pub fn state(&self) -> ProcessState {
        self.state.lock().clone()
    }

    /// How many OS processes this client has started. `1` after a clean boot; `2` after one
    /// restart. The independence test reads it.
    pub fn spawn_count(&self) -> u32 {
        self.spawns.load(Ordering::SeqCst)
    }

    /// The supervision loop: spawn, handshake, pump, and decide what a death means.
    async fn supervise(self: Arc<Self>, settled: tokio::sync::oneshot::Sender<()>) {
        let row = &self.cfg.row;
        let mut settled = Some(settled);
        // Jittered and capped (backon): a fleet of processes that all died on the same cause must
        // not all come back in the same millisecond.
        let mut backoff = backon::ExponentialBuilder::default()
            .with_jitter()
            .with_min_delay(Duration::from_millis(row.restart_delay_ms.max(1)))
            .with_max_delay(Duration::from_millis(row.restart_delay_ms.max(1) * 32))
            .without_max_times()
            .build();
        let mut fast_deaths: u32 = 0;

        loop {
            let started = Instant::now();
            let cause = match self.run_once().await {
                Ok(cause) => cause,
                Err(cause) => cause,
            };
            self.ready.store(false, Ordering::SeqCst);
            *self.outbox.lock() = None;
            // Every waiter on a dead child is answered, not left hanging.
            for (_, tx) in std::mem::take(&mut *self.pending.lock()) {
                let _ = tx.send(Err("the process went away".to_string()));
            }
            if let Some(tx) = settled.take() {
                let _ = tx.send(());
            }

            if started.elapsed() < Duration::from_millis(row.min_uptime_ms) {
                fast_deaths += 1;
            } else {
                // It stayed up: this is an ordinary crash, not a crash LOOP.
                fast_deaths = 0;
            }
            if fast_deaths > row.max_restarts {
                let reason = format!(
                    "died within {}ms {} times in a row; last: {cause}",
                    row.min_uptime_ms, fast_deaths
                );
                tracing::warn!(server = %self.name, reason = %reason,
                               "resident MCP process QUARANTINED");
                *self.state.lock() = ProcessState::Quarantined { reason };
                return;
            }
            *self.state.lock() = ProcessState::Restarting {
                attempt: fast_deaths,
                last: cause.clone(),
            };
            tracing::warn!(server = %self.name, cause = %cause, attempt = fast_deaths,
                           "resident MCP process exited; restarting");
            let delay = backoff.next().unwrap_or(Duration::from_millis(50));
            tokio::time::sleep(delay).await;
        }
    }

    /// One life of the process: spawn, handshake, list tools, pump until it dies. The `String` is
    /// what ended it, either way.
    async fn run_once(self: &Arc<Self>) -> Result<String, String> {
        let row = &self.cfg.row;
        *self.state.lock() = ProcessState::Starting;
        let mut cmd = tokio::process::Command::new(&row.command);
        cmd.args(&row.args)
            .envs(&row.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(cwd) = &row.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().map_err(|e| format!("could not start: {e}"))?;
        self.spawns.fetch_add(1, Ordering::SeqCst);
        let pid = child.id().unwrap_or(0);
        *self.state.lock() = ProcessState::Up { pid };

        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        *self.outbox.lock() = Some(tx);

        // The writer: one task owning the child's stdin, so `call` never blocks on it.
        let writer = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    return;
                }
            }
        });

        // The reader: pumps until EOF, resolving responses and dispatching notifications.
        let me = Arc::clone(self);
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                me.on_line(&line);
            }
        });

        // The handshake is a REQUEST, so it needs the pump already running.
        let handshake = self.handshake().await;
        match handshake {
            Ok(()) => {
                self.ready.store(true, Ordering::SeqCst);
                if let Err(e) = self.refresh_tools().await {
                    tracing::warn!(server = %self.name, error = %e, "tools/list failed");
                }
            }
            Err(e) => {
                tracing::warn!(server = %self.name, error = %e, "MCP handshake failed");
            }
        }

        let status = child.wait().await;
        reader.abort();
        writer.abort();
        Ok(match status {
            Ok(s) => format!("exited {s}"),
            Err(e) => format!("wait failed: {e}"),
        })
    }

    /// One incoming line. Pure dispatch over [`jsonrpc::parse_line`].
    fn on_line(&self, line: &str) {
        match jsonrpc::parse_line(line) {
            Incoming::Response { id, result } => {
                if let Some(tx) = self.pending.lock().remove(&id) {
                    let _ = tx.send(result);
                }
            }
            Incoming::Notification { method, params } if method == crate::ACTIONS_NOTIFICATION => {
                match serde_json::from_value::<ActionsParams>(params) {
                    Ok(p) => (self.on_actions)(p.actions),
                    Err(e) => tracing::warn!(server = %self.name, error = %e,
                                             "`bough/actions` params were not a list of actions"),
                }
            }
            Incoming::Notification { .. } => {}
            Incoming::Junk(detail) => {
                tracing::warn!(server = %self.name, detail = %detail, "unreadable MCP line")
            }
        }
    }

    async fn handshake(&self) -> Result<(), String> {
        self.rpc(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "bough", "version": env!("CARGO_PKG_VERSION") }
            }),
        )
        .await?;
        self.send(jsonrpc::notification(
            "notifications/initialized",
            serde_json::json!({}),
        ))
    }

    /// Re-list this process's tools and CACHE them. The cache is what a call-while-down reads.
    pub async fn refresh_tools(&self) -> Result<usize, String> {
        let v = self.rpc("tools/list", serde_json::json!({})).await?;
        let listed = v
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let tools: Vec<McpToolInfo> = listed
            .iter()
            .filter_map(|t| {
                Some(McpToolInfo {
                    server: self.name.clone(),
                    tool: t.get("name")?.as_str()?.to_string(),
                    description: t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();
        let n = tools.len();
        *self.tools.lock() = tools;
        Ok(n)
    }

    fn send(&self, line: String) -> Result<(), String> {
        let out = self.outbox.lock().clone();
        match out {
            Some(tx) => tx.send(line).map_err(|_| "the process is gone".to_string()),
            None => Err("the process is down".to_string()),
        }
    }

    /// One request/response round trip, bounded.
    async fn rpc(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().insert(id, tx);
        if let Err(e) = self.send(jsonrpc::request(id, method, params)) {
            self.pending.lock().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(self.call_timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("the process went away".to_string()),
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(format!("`{method}` did not answer in time"))
            }
        }
    }
}

/// The params of a `bough/actions` notification.
#[derive(Debug, serde::Deserialize)]
struct ActionsParams {
    #[serde(default)]
    actions: Vec<RuntimeAction>,
}

#[async_trait::async_trait]
impl McpClient for ResidentProcess {
    fn server(&self) -> &ServerName {
        &self.name
    }

    /// The last successful listing — including while the process is down, which is what keeps its
    /// tools registered across a restart.
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        Ok(self.tools.lock().clone())
    }

    async fn call(&self, tool: &str, args: serde_json::Value) -> Result<McpCallResult, McpError> {
        if !self.is_ready() {
            return Err(McpError::Unavailable(self.name.clone()));
        }
        let v = self
            .rpc(
                "tools/call",
                serde_json::json!({ "name": tool, "arguments": args }),
            )
            .await
            .map_err(McpError::Transport)?;
        let is_error = v.get("isError").and_then(|e| e.as_bool()).unwrap_or(false);
        let content = v
            .get("content")
            .and_then(|c| c.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        Ok(McpCallResult {
            content,
            value: v.get("structuredContent").cloned(),
            // The CITE is minted by the seam (`plugins/mcp`), never by the server or by a
            // transport: a foreign process cannot be trusted to say what it was asked.
            cites: Vec::new(),
            is_error,
        })
    }

    /// `false` while the process is restarting.
    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
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
        crate::validate_row(&cfg.row)
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let mcp = ctx
            .get::<bough_plugin_mcp::Mcp>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        let cx = crate::runtime_cx(&ctx, &entry, &cfg.row.name)?;
        let limits = cfg.limits.clone();
        let on_actions: ActionsCallback = Arc::new(move |actions: Vec<RuntimeAction>| {
            let (cx, limits) = (cx.clone(), limits.clone());
            // Detached on purpose: the reader pump must never wait on the write boundary.
            tokio::spawn(async move {
                bough_plugin_runtime_actions::execute_all(&cx, &actions, &limits).await;
            });
        });

        let (client, supervisor) = ResidentProcess::start(cfg.clone(), on_actions).await;
        // The supervisor is an EFFECT of this fiber: unloading the row kills the process (every
        // spawn is `kill_on_drop`) and leaves no trace.
        ctx.effect(move |e| async move {
            e.defer_sync(move || supervisor.abort());
            Ok(())
        })
        .await?;

        bough_plugin_mcp::McpHandle(mcp.0.clone())
            .server(&ctx, client)
            .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}
