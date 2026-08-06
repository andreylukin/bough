//! The stdio MCP client: newline-delimited JSON-RPC 2.0 to a child process
//! bough spawns itself (port of `src/mcp/client.ts`).
//!
//! THE INVARIANT THIS HOLDS: **a server that does not work fails, by name, in
//! bounded time.** Never a hang. A hung MCP server is worse than an absent one
//! — the turn it is attached to stalls behind it, the user sees a spinner, and
//! there is nothing in the transcript that says why. So every path out of this
//! module terminates:
//!
//!   - a binary that does not exist fails at spawn, naming the command;
//!   - a process that starts and never answers `initialize` fails on the connect
//!     deadline, with its stderr attached;
//!   - a process that dies — at any point, including mid-call — fails everything
//!     in flight immediately from its exit handler rather than waiting for
//!     timeouts;
//!   - every request carries its own deadline;
//!   - `tools/list` pagination is bounded, so a server that returns the same
//!     cursor forever is an error and not an infinite loop;
//!   - a server-initiated request (sampling, roots, elicitation) is REFUSED with
//!     a JSON-RPC error rather than ignored, because a server waiting on a reply
//!     that never comes is the same hang seen from the other end.
//!
//! Every failure is an `McpError` carrying a status and a sentence that names
//! the server, what failed, and the move that resolves it — the text reaches the
//! model as a caught exception inside its program.
//!
//! WHY HAND-ROLLED. Owning the spawn is the point: bough composes the child's
//! ENTIRE environment (`config::child_env`) so a third-party binary does not
//! inherit the user's provider keys, and it tracks the child so shutdown can
//! kill it ([`kill_all_mcp_servers`]).
//!
//! `tools/list` entries are parsed strictly first and leniently second —
//! dropping a callable tool from the catalog over a schema nit (an
//! `inputSchema` missing `type: "object"` is the common one) is a worse outcome
//! than listing it with a thin signature. `tools/call` results are read
//! leniently for the same reason: an unrecognized content block must not turn a
//! successful call into a failure.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex};

use crate::errors::BoughError;
use crate::mcp::config::mcp_error;

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

/// The version bough proposes at handshake. Pinned rather than imported: the TS
/// build reads it from `@modelcontextprotocol/sdk`, and these strings are the
/// whole of what that import contributed.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

/// Every version bough will accept from a server, newest first.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

const JSONRPC_VERSION: &str = "2.0";

/// How long a killed child gets to exit before SIGKILL.
pub const KILL_GRACE_MS: u64 = 2_000;
/// Stderr kept for `/mcp` status ("why is it failing").
pub const STDERR_TAIL_BYTES: usize = 4_096;
/// Stderr included in an error message — enough to diagnose, not enough to
/// flood a turn.
pub const STDERR_NOTE_BYTES: usize = 500;
/// `tools/list` pages followed before giving up. A server that returns a cursor
/// pointing at itself would otherwise loop forever inside a turn.
pub const MAX_TOOL_PAGES: usize = 50;

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// What the layer above needs from any transport. The stdio client below and
/// the remote client (row 3.4) both satisfy it, so nothing upstream branches on
/// which one a registry entry produced.
#[async_trait]
pub trait McpConnection: Send + Sync {
    /// The registry name this connection is for — every error carries it.
    fn name(&self) -> &str;
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, BoughError>;
    async fn call_tool(&self, name: &str, args: Value) -> Result<McpCallResult, BoughError>;
    /// Idempotent, and never fails — teardown that can fail is teardown that
    /// leaks a process.
    async fn close(&self);
    fn alive(&self) -> bool;
    /// Recent diagnostics — stderr for stdio, last transport error for remote.
    fn stderr_tail(&self) -> String;
}

/// The argument shape of a tool, as much of it as the catalog renders.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct McpToolSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

/// One tool as advertised by `tools/list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<McpToolSchema>,
    /// Behavior hints (`readOnlyHint`, `destructiveHint`, …). Server-supplied
    /// and therefore untrusted: they may SEED a classification, they never
    /// grant one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Map<String, Value>>,
}

/// One content block of a `tools/call` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct McpContentBlock {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A `tools/call` result, read leniently — see the module comment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<McpContentBlock>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// The tool itself failed. A tool error is DATA, not a transport failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Identity the server reported at handshake, for `/mcp` status.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub protocol_version: String,
}

/// Deadlines. Injected so a test asserts the no-hang property in milliseconds
/// instead of waiting out a production timeout.
#[derive(Clone, Copy, Debug, Default)]
pub struct McpTimeouts {
    /// Spawn → `initialize` answered.
    pub connect_ms: Option<u64>,
    /// Ordinary requests (`tools/list`).
    pub request_ms: Option<u64>,
    /// `tools/call`. Longer on purpose: browser automation and long fetches are
    /// legitimate.
    pub call_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct Deadlines {
    connect_ms: u64,
    request_ms: u64,
    call_ms: u64,
}

impl From<Option<McpTimeouts>> for Deadlines {
    fn from(t: Option<McpTimeouts>) -> Deadlines {
        let t = t.unwrap_or_default();
        Deadlines {
            connect_ms: t.connect_ms.unwrap_or(30_000),
            request_ms: t.request_ms.unwrap_or(30_000),
            call_ms: t.call_ms.unwrap_or(5 * 60_000),
        }
    }
}

/// How to spawn one stdio server.
#[derive(Clone, Debug, Default)]
pub struct McpStdioOptions {
    /// The registry name. Absent = the executable, so errors are never
    /// anonymous.
    pub name: Option<String>,
    /// argv, composed by the caller (`[command, ...args]`).
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    /// The child's ENTIRE environment (clear-env). The caller composes it —
    /// `config::child_env` — so a third-party binary never inherits the user's
    /// keys.
    pub env: std::collections::BTreeMap<String, String>,
    pub timeouts: Option<McpTimeouts>,
}

// ---------------------------------------------------------------------------
// Live children
// ---------------------------------------------------------------------------

/// Every connected stdio client in this process.
///
/// MCP servers are children of the server process, and the same trap background
/// shells have applies: a chatty server dies of SIGPIPE when our end of its
/// stdout closes, but a silent one — an idle HTTP bridge, a server between
/// requests — survives, reparented and invisible, with nothing left that knows
/// it exists. So shutdown kills them explicitly.
fn live_clients() -> &'static Mutex<HashMap<u64, Arc<Inner>>> {
    static LIVE: OnceLock<Mutex<HashMap<u64, Arc<Inner>>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_client_id() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::SeqCst)
}

/// SIGTERM every connected MCP server. Synchronous and best-effort, because the
/// caller is a signal handler on its way to exit and has no await to give.
/// Returns how many were signalled.
pub fn kill_all_mcp_servers() -> usize {
    let clients: Vec<Arc<Inner>> = {
        let live = live_clients().lock().unwrap();
        live.values().cloned().collect()
    };
    let mut killed = 0;
    for inner in clients {
        if inner.terminate() {
            killed += 1;
        }
    }
    killed
}

/// How many stdio servers this process currently holds open.
pub fn live_mcp_server_count() -> usize {
    live_clients().lock().unwrap().len()
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

type Pending = oneshot::Sender<Result<Value, BoughError>>;

struct Inner {
    id: u64,
    name: String,
    pid: i32,
    seq: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    stdin: AsyncMutex<Option<ChildStdin>>,
    stderr_tail: Mutex<String>,
    exited: AtomicBool,
    closed: AtomicBool,
    info: Mutex<Option<McpServerInfo>>,
    deadlines: Deadlines,
    /// Flips true once the child has been reaped; `close` awaits it rather than
    /// racing the exit.
    exit: watch::Receiver<bool>,
}

impl Inner {
    fn alive(&self) -> bool {
        !self.exited.load(Ordering::SeqCst) && !self.closed.load(Ordering::SeqCst)
    }

    fn stderr_note(&self) -> String {
        let tail = self.stderr_tail.lock().unwrap().trim().to_string();
        if tail.is_empty() {
            return String::new();
        }
        format!(" — stderr: {}", last_chars(&tail, STDERR_NOTE_BYTES))
    }

    fn fail_all(&self, error: BoughError) {
        let taken: Vec<(u64, Pending)> = {
            let mut pending = self.pending.lock().unwrap();
            pending.drain().collect()
        };
        for (_, tx) in taken {
            let _ = tx.send(Err(error.clone()));
        }
    }

    /// SIGTERM the child without awaiting it. Returns whether a signal was
    /// actually sent.
    fn terminate(&self) -> bool {
        live_clients().lock().unwrap().remove(&self.id);
        if self.exited.load(Ordering::SeqCst) {
            return false;
        }
        signal_pid(self.pid, nix::sys::signal::Signal::SIGTERM)
    }

    async fn send(&self, message: &Value) {
        let line = format!("{message}\n");
        let mut guard = self.stdin.lock().await;
        if let Some(stdin) = guard.as_mut() {
            // stdin gone = the child is dead; its exit handler fails the pending
            // request with a message that says so.
            let _ = stdin.write_all(line.as_bytes()).await;
            let _ = stdin.flush().await;
        }
    }

    async fn request(
        self: &Arc<Self>,
        method: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<Value, BoughError> {
        if !self.alive() {
            return Err(mcp_error(
                502,
                format!(
                    "MCP server \"{}\" is not running, so {method} could not be sent{}. \
                     Reconnect it (POST /mcp/servers/{}/enable) or check the command in the registry.",
                    self.name,
                    self.stderr_note(),
                    self.name,
                ),
            ));
        }
        let id = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": method,
            "params": params,
        }))
        .await;

        // The deadline IS the timeout: a settled request drops its timer with
        // the future, so the "one armed timer per successful call" leak the TS
        // port had to fix cannot exist here.
        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
            Ok(Ok(result)) => result,
            // The sender was dropped without a value: only reachable if the
            // pending map were cleared without sending, which `fail_all` never
            // does. Report a disconnection rather than panicking.
            Ok(Err(_)) => Err(mcp_error(
                502,
                format!(
                    "MCP server \"{}\" was disconnected while the call was in flight.",
                    self.name
                ),
            )),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(mcp_error(
                    504,
                    format!(
                        "MCP {method} on server \"{}\" timed out after {timeout_ms}ms{}. \
                         The server is running but did not answer.",
                        self.name,
                        self.stderr_note(),
                    ),
                ))
            }
        }
    }

    fn dispatch(self: &Arc<Self>, line: &str) {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return; // a server that logs to stdout — skip the noise
        };
        let id = message.get("id");
        let method = message.get("method").and_then(|m| m.as_str());

        // A reply to one of ours.
        if let (Some(id), None) = (id, method) {
            let Some(key) = id
                .as_u64()
                .or_else(|| id.as_str().and_then(|s| s.parse().ok()))
            else {
                return;
            };
            let Some(tx) = self.pending.lock().unwrap().remove(&key) else {
                return; // already timed out, or never ours
            };
            if let Some(error) = message.get("error") {
                let code = error
                    .get("code")
                    .map(|c| format!(" (code {c})"))
                    .unwrap_or_default();
                let text = error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unspecified error");
                let _ = tx.send(Err(mcp_error(
                    502,
                    format!("MCP server \"{}\": {text}{code}", self.name),
                )));
            } else {
                let _ = tx.send(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
            }
            return;
        }

        // A server-initiated REQUEST (sampling, roots, elicitation): refuse it
        // explicitly. Ignoring it leaves the server blocked on a reply forever,
        // which is the same hang from the other side of the pipe. Notifications
        // need none.
        if let (Some(id), Some(method)) = (id, method) {
            let reply = json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": id,
                "error": { "code": -32601, "message": format!("bough does not support {method}") },
            });
            let inner = self.clone();
            tokio::spawn(async move { inner.send(&reply).await });
        }
    }
}

/// The last `n` characters of a string (the TS `.slice(-n)`).
fn last_chars(value: &str, n: usize) -> String {
    let count = value.chars().count();
    if count <= n {
        return value.to_string();
    }
    value.chars().skip(count - n).collect()
}

/// SIGTERM/SIGKILL one pid, best effort. `false` when the process is already
/// gone (or was never ours to signal).
fn signal_pid(pid: i32, sig: nix::sys::signal::Signal) -> bool {
    if pid <= 0 {
        return false;
    }
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), sig).is_ok()
}

/// A live stdio MCP server.
pub struct McpStdioClient {
    inner: Arc<Inner>,
}

/// Names the server and whether it is still running — enough for a test's
/// `unwrap_err`, and nothing that could print a child's environment.
impl std::fmt::Debug for McpStdioClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpStdioClient")
            .field("name", &self.inner.name)
            .field("alive", &self.inner.alive())
            .finish()
    }
}

impl McpStdioClient {
    /// Spawn the server and run the MCP initialize handshake.
    ///
    /// Rejects — never hangs, never resolves a half-connected client — on a
    /// missing binary, a process that exits during the handshake, a process that
    /// never answers, a reply that is not an MCP handshake, or a protocol
    /// version bough does not speak.
    pub async fn connect(opts: McpStdioOptions) -> Result<McpStdioClient, BoughError> {
        let name = opts
            .name
            .clone()
            .or_else(|| opts.argv.first().cloned())
            .unwrap_or_else(|| "(unnamed)".to_string());
        let deadlines = Deadlines::from(opts.timeouts);
        if opts.argv.is_empty() {
            return Err(mcp_error(
                400,
                format!("MCP server \"{name}\" has no command to run — set `command`."),
            ));
        }

        let mut command = Command::new(&opts.argv[0]);
        command
            .args(&opts.argv[1..])
            .env_clear()
            .envs(opts.env.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        if let Some(cwd) = &opts.cwd {
            command.current_dir(cwd);
        }
        // A spawn failure must not surface as a raw ENOENT, which reads as a
        // missing FILE and says nothing about which server or which command.
        let mut child = command.spawn().map_err(|e| {
            mcp_error(
                502,
                format!(
                    "MCP server \"{name}\" failed to start: could not run {:?} ({e}). \
                     Check `command` in the registry and that it is on PATH.",
                    opts.argv[0]
                ),
            )
        })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pid = child.id().map(|p| p as i32).unwrap_or(-1);
        let (exit_tx, exit_rx) = watch::channel(false);

        let inner = Arc::new(Inner {
            id: next_client_id(),
            name: name.clone(),
            pid,
            seq: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            stdin: AsyncMutex::new(stdin),
            stderr_tail: Mutex::new(String::new()),
            exited: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            info: Mutex::new(None),
            deadlines,
            exit: exit_rx,
        });
        live_clients()
            .lock()
            .unwrap()
            .insert(inner.id, inner.clone());

        // The read loop: one JSON-RPC message per line, non-JSON skipped.
        if let Some(stdout) = stdout {
            let inner = inner.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        inner.dispatch(trimmed);
                    }
                }
            });
        }

        // The stderr tail, and the handle the exit watcher waits on so a death
        // report carries the diagnostic that explains it.
        let stderr_task = stderr.map(|mut stderr| {
            let inner = inner.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                            let mut tail = inner.stderr_tail.lock().unwrap();
                            tail.push_str(&chunk);
                            if tail.chars().count() > STDERR_TAIL_BYTES {
                                *tail = last_chars(&tail, STDERR_TAIL_BYTES);
                            }
                        }
                    }
                }
            })
        });

        // The whole point of this task: when the child dies, everything in
        // flight fails NOW with a message that says it died, instead of sitting
        // until its deadline and reporting a timeout for a process that is
        // already gone.
        {
            let inner = inner.clone();
            tokio::spawn(async move {
                let status = child.wait().await;
                // The stderr pipe closes with the process, so this settles at
                // once — and the death report then carries the diagnostic
                // instead of racing it.
                if let Some(task) = stderr_task {
                    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), task).await;
                }
                inner.exited.store(true, Ordering::SeqCst);
                live_clients().lock().unwrap().remove(&inner.id);
                let how = match &status {
                    Ok(s) => match exit_signal(s) {
                        Some(sig) => format!(" on {sig}"),
                        None => format!(" with code {}", s.code().unwrap_or(-1)),
                    },
                    Err(e) => format!(" ({e})"),
                };
                inner.fail_all(mcp_error(
                    502,
                    format!(
                        "MCP server \"{}\" exited{how}{}. Check the command in the registry \
                         (GET /mcp/servers) and run it by hand to see why it stopped.",
                        inner.name,
                        inner.stderr_note(),
                    ),
                ));
                let _ = exit_tx.send(true);
            });
        }

        let client = McpStdioClient {
            inner: inner.clone(),
        };
        match client.handshake(&name).await {
            Ok(()) => Ok(client),
            Err(error) => {
                client.close().await;
                Err(error)
            }
        }
    }

    async fn handshake(&self, name: &str) -> Result<(), BoughError> {
        let raw = self
            .inner
            .request(
                "initialize",
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "bough", "version": "0" },
                }),
                self.inner.deadlines.connect_ms,
            )
            .await?;
        let Some(parsed) = parse_initialize(&raw) else {
            return Err(mcp_error(
                502,
                format!(
                    "MCP server \"{name}\" answered initialize with something that is not an MCP \
                     handshake{}. It is probably not an MCP server, or it logs to stdout — MCP \
                     requires stdout to carry JSON-RPC only.",
                    self.inner.stderr_note(),
                ),
            ));
        };
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&parsed.protocol_version.as_str()) {
            return Err(mcp_error(
                502,
                format!(
                    "MCP server \"{name}\" speaks protocol version {}; bough speaks {}. \
                     Upgrade the server, or pin an older release of it.",
                    parsed.protocol_version,
                    SUPPORTED_PROTOCOL_VERSIONS.join(", "),
                ),
            ));
        }
        *self.inner.info.lock().unwrap() = Some(parsed);
        self.inner
            .send(&json!({ "jsonrpc": JSONRPC_VERSION, "method": "notifications/initialized" }))
            .await;
        Ok(())
    }

    /// Identity from the handshake — `None` until `connect` resolves.
    pub fn server_info(&self) -> Option<McpServerInfo> {
        self.inner.info.lock().unwrap().clone()
    }

    /// SIGTERM the child without awaiting it. For process shutdown, which has no
    /// await to give.
    pub fn terminate(&self) -> bool {
        self.inner.terminate()
    }

    /// Wait for the child to be reaped, or `ms` to pass.
    async fn await_exit(&self, ms: u64) -> bool {
        let mut exit = self.inner.exit.clone();
        tokio::time::timeout(std::time::Duration::from_millis(ms), async move {
            while !*exit.borrow_and_update() {
                if exit.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
        .is_ok()
    }
}

#[async_trait]
impl McpConnection for McpStdioClient {
    fn name(&self) -> &str {
        &self.inner.name
    }

    /// Every tool the server advertises, following pagination cursors.
    ///
    /// Bounded twice: a cursor that repeats and a page count that runs away are
    /// both errors, because either one inside a turn is a hang with extra steps.
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, BoughError> {
        let mut tools: Vec<McpToolInfo> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self
                .inner
                .request("tools/list", params, self.inner.deadlines.request_ms)
                .await?;
            for raw in result
                .get("tools")
                .and_then(|t| t.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(tool) = tool_info(raw) {
                    tools.push(tool);
                }
            }
            let next = result
                .get("nextCursor")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let Some(next) = next else { return Ok(tools) };
            if !seen.insert(next.clone()) {
                return Err(mcp_error(
                    502,
                    format!(
                        "MCP server \"{}\" repeated the tools/list cursor {:?}, so its tool list \
                         never ends. Reporting {} tools and stopping.",
                        self.inner.name,
                        next,
                        tools.len(),
                    ),
                ));
            }
            cursor = Some(next);
        }
        Err(mcp_error(
            502,
            format!(
                "MCP server \"{}\" paginated tools/list past {MAX_TOOL_PAGES} pages. \
                 Reporting {} tools and stopping.",
                self.inner.name,
                tools.len(),
            ),
        ))
    }

    /// Invoke one tool. A tool that FAILS comes back as `{isError: true}` — that
    /// is data the program reads, not an exception. Only transport, protocol and
    /// deadline failures throw.
    async fn call_tool(&self, name: &str, args: Value) -> Result<McpCallResult, BoughError> {
        let arguments = if args.is_null() { json!({}) } else { args };
        let raw = self
            .inner
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                self.inner.deadlines.call_ms,
            )
            .await?;
        Ok(call_result(&raw))
    }

    /// Terminate the child and release its pipes. Safe to call twice, safe on a
    /// client whose child already died, and never fails.
    async fn close(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            // Still wait for the child, so a second caller does not race the exit.
            self.await_exit(KILL_GRACE_MS * 2).await;
            return;
        }
        live_clients().lock().unwrap().remove(&self.inner.id);
        self.inner.fail_all(mcp_error(
            502,
            format!(
                "MCP server \"{}\" was disconnected while the call was in flight.",
                self.inner.name
            ),
        ));
        // End stdin: a well-behaved server exits when its input closes.
        {
            let mut guard = self.inner.stdin.lock().await;
            if let Some(mut stdin) = guard.take() {
                let _ = stdin.shutdown().await;
            }
        }
        if !self.inner.exited.load(Ordering::SeqCst) {
            signal_pid(self.inner.pid, nix::sys::signal::Signal::SIGTERM);
        }
        // Grace, then force: a wedged server must not outlive its connection.
        if !self.await_exit(KILL_GRACE_MS).await {
            signal_pid(self.inner.pid, nix::sys::signal::Signal::SIGKILL);
            self.await_exit(KILL_GRACE_MS).await;
        }
    }

    fn alive(&self) -> bool {
        self.inner.alive()
    }

    fn stderr_tail(&self) -> String {
        self.inner.stderr_tail.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// Result shapes
// ---------------------------------------------------------------------------

fn exit_signal(status: &std::process::ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|n| match n {
            1 => "SIGHUP".to_string(),
            2 => "SIGINT".to_string(),
            6 => "SIGABRT".to_string(),
            9 => "SIGKILL".to_string(),
            11 => "SIGSEGV".to_string(),
            13 => "SIGPIPE".to_string(),
            15 => "SIGTERM".to_string(),
            other => format!("signal {other}"),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// The handshake, validated strictly: a process that answers something else is
/// not an MCP server, and saying so at connect is far better than an empty tool
/// list later.
fn parse_initialize(raw: &Value) -> Option<McpServerInfo> {
    let obj = raw.as_object()?;
    let protocol_version = obj.get("protocolVersion")?.as_str()?.to_string();
    obj.get("capabilities")?.as_object()?;
    let server_info = obj.get("serverInfo")?.as_object()?;
    let name = server_info.get("name")?.as_str()?.to_string();
    let version = server_info.get("version")?.as_str()?.to_string();
    Some(McpServerInfo {
        name: Some(name),
        version: Some(version),
        protocol_version,
    })
}

/// One advertised tool: the strict shape first, a name-only fallback second.
/// `None` means the entry could not be called even in principle, so it is
/// dropped from the catalog rather than printed as a tool the model may try.
fn tool_info(raw: &Value) -> Option<McpToolInfo> {
    let obj = raw.as_object()?;
    let name = obj.get("name")?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let description = obj
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());
    let annotations = obj.get("annotations").and_then(|a| a.as_object()).cloned();
    // The strict shape carries `type: "object"`; the lenient one keeps whatever
    // schema there was, or none at all. Either way the tool stays callable —
    // dropping it over a schema nit is the worse outcome.
    let input_schema = obj
        .get("inputSchema")
        .and_then(|s| s.as_object())
        .map(|s| McpToolSchema {
            properties: s.get("properties").and_then(|p| p.as_object()).cloned(),
            required: s.get("required").and_then(|r| r.as_array()).map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.as_str().map(String::from))
                    .collect()
            }),
        });
    Some(McpToolInfo {
        name,
        description,
        input_schema,
        annotations,
    })
}

/// `tools/call`, read leniently: the fields the harness uses, anything else
/// passed through. An unrecognized shape becomes structured content rather than
/// a failed call the server considered successful.
fn call_result(raw: &Value) -> McpCallResult {
    let Some(obj) = raw.as_object() else {
        return McpCallResult {
            structured_content: Some(raw.clone()),
            ..Default::default()
        };
    };
    let content = obj.get("content").and_then(|c| c.as_array()).map(|items| {
        items
            .iter()
            .filter_map(|item| {
                let block = item.as_object()?;
                Some(McpContentBlock {
                    r#type: block.get("type")?.as_str()?.to_string(),
                    text: block.get("text").and_then(|t| t.as_str()).map(String::from),
                })
            })
            .collect::<Vec<_>>()
    });
    let structured_content = obj.get("structuredContent").cloned();
    let is_error = obj.get("isError").and_then(|e| e.as_bool());
    if content.is_none() && structured_content.is_none() && is_error.is_none() {
        return McpCallResult {
            structured_content: Some(raw.clone()),
            ..Default::default()
        };
    }
    McpCallResult {
        content,
        structured_content,
        is_error,
    }
}

// ---------------------------------------------------------------------------
// The fixture server (shared with `manager.rs`)
// ---------------------------------------------------------------------------

/// A real child process speaking real JSON-RPC, written to a temp path and made
/// executable. A POSIX shell script on purpose: the no-hang suite must run with
/// nothing on the machine but `sh` and `sed`.
#[cfg(test)]
pub(crate) mod fixture {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    const SCRIPT: &str = include_str!("testdata/echo_server.sh");

    /// `kill_all_mcp_servers` is process-wide by design — it is a shutdown
    /// handler — and `cargo test` runs the suite in ONE process with threads. So
    /// every test that spawns a real child holds this read guard, and the
    /// kill-everything test holds the write guard: without it that test would
    /// reap another test's server mid-call and the failure would land in the
    /// wrong place.
    pub fn live_children_lock() -> &'static tokio::sync::RwLock<()> {
        static LOCK: OnceLock<tokio::sync::RwLock<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::RwLock::new(()))
    }

    pub fn echo_server() -> PathBuf {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let dir =
                std::env::temp_dir().join(format!("bough-mcp-fixture-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("echo_server.sh");
            std::fs::write(&path, SCRIPT).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        })
        .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::{child_env, McpConfigOptions, ServerConfig};

    /// Deadlines short by production standards, long enough that a loaded
    /// machine still completes the handshake.
    fn timeouts(overrides: McpTimeouts) -> McpTimeouts {
        McpTimeouts {
            connect_ms: overrides.connect_ms.or(Some(20_000)),
            request_ms: overrides.request_ms.or(Some(20_000)),
            call_ms: overrides.call_ms.or(Some(20_000)),
        }
    }

    async fn connect_fixture(
        args: &[&str],
        overrides: McpTimeouts,
    ) -> Result<McpStdioClient, BoughError> {
        let script = fixture::echo_server();
        let server = ServerConfig {
            command: Some("/bin/sh".to_string()),
            args: std::iter::once(script.to_string_lossy().to_string())
                .chain(args.iter().map(|a| a.to_string()))
                .collect(),
            ..Default::default()
        };
        let mut argv = vec![server.command.clone().unwrap()];
        argv.extend(server.args.clone());
        McpStdioClient::connect(McpStdioOptions {
            name: Some("echo".to_string()),
            argv,
            cwd: None,
            env: child_env(&server, &McpConfigOptions::default()).unwrap(),
            timeouts: Some(timeouts(overrides)),
        })
        .await
    }

    #[tokio::test]
    async fn handshake_paginated_tools_list_call_and_close() {
        let _live = fixture::live_children_lock().read().await;
        let client = connect_fixture(&[], McpTimeouts::default()).await.unwrap();
        assert!(client.alive());
        assert_eq!(
            client.server_info().unwrap().name.as_deref(),
            Some("echo-fixture")
        );
        assert_eq!(client.server_info().unwrap().protocol_version, "2025-06-18");

        // Both pages, in order. The nameless entry is dropped; the one with a
        // sloppy inputSchema is KEPT, because it is callable.
        let tools = client.list_tools().await.unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["echo", "scream", "boom", "die", "slow", "loose"]
        );
        assert_eq!(
            tools[0].annotations.as_ref().unwrap().get("readOnlyHint"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            tools[0]
                .input_schema
                .as_ref()
                .unwrap()
                .required
                .as_ref()
                .unwrap(),
            &vec!["text".to_string()]
        );
        assert_eq!(
            tools[5]
                .input_schema
                .as_ref()
                .unwrap()
                .properties
                .as_ref()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["q"]
        );

        let echoed = client
            .call_tool("echo", json!({"text": "hi"}))
            .await
            .unwrap();
        assert_eq!(echoed.structured_content, Some(json!({"echoed": "hi"})));
        assert_eq!(
            echoed.content.as_ref().unwrap()[0].text.as_deref(),
            Some("hi")
        );
        assert!(!echoed.is_error.unwrap_or(false));

        // A tool that fails is DATA, not an exception.
        let boom = client.call_tool("boom", json!({})).await.unwrap();
        assert_eq!(boom.is_error, Some(true));
        assert_eq!(
            boom.content.as_ref().unwrap()[0].text.as_deref(),
            Some("kaboom")
        );

        // A tool advertised with a sloppy schema is still callable.
        let loose = client.call_tool("loose", json!({"q": "x"})).await.unwrap();
        assert_eq!(
            loose.content.as_ref().unwrap()[0].text.as_deref(),
            Some("q=x")
        );

        client.close().await;
        assert!(!client.alive());
    }

    #[tokio::test]
    async fn a_server_that_logs_to_stdout_still_connects() {
        let _live = fixture::live_children_lock().read().await;
        let client = connect_fixture(&["--noise"], McpTimeouts::default())
            .await
            .unwrap();
        assert_eq!(client.list_tools().await.unwrap().len(), 6);
        client.close().await;
    }

    #[tokio::test]
    async fn a_server_that_dies_mid_call_fails_that_call_by_name_with_its_stderr() {
        let _live = fixture::live_children_lock().read().await;
        let client = connect_fixture(&[], McpTimeouts::default()).await.unwrap();
        let error = client.call_tool("die", json!({})).await.unwrap_err();
        assert_eq!(error.status(), 502);
        let message = error.to_string();
        assert!(message.contains("MCP server \"echo\" exited"), "{message}");
        assert!(message.contains("code 3"), "{message}");
        // The diagnostic the user needs is attached, not buried in a log file.
        assert!(message.contains("asked to die"), "{message}");
        assert!(!client.alive());

        // And the connection stays failed rather than hanging the next call.
        let next = client.list_tools().await.unwrap_err();
        assert!(next.to_string().contains("is not running"), "{next}");
        client.close().await;
    }

    #[tokio::test]
    async fn a_call_the_server_never_answers_fails_on_its_deadline_server_still_alive() {
        let _live = fixture::live_children_lock().read().await;
        let client = connect_fixture(
            &[],
            McpTimeouts {
                call_ms: Some(300),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let error = client.call_tool("slow", json!({})).await.unwrap_err();
        assert_eq!(error.status(), 504);
        assert!(
            error
                .to_string()
                .contains("MCP tools/call on server \"echo\" timed out after 300ms"),
            "{error}"
        );
        // The server is fine — one call timing out must not condemn the connection.
        assert!(client.alive());
        assert_eq!(client.list_tools().await.unwrap().len(), 6);
        client.close().await;
    }

    #[tokio::test]
    async fn a_server_that_starts_and_never_handshakes_fails_on_the_connect_deadline() {
        let _live = fixture::live_children_lock().read().await;
        let started = std::time::Instant::now();
        let error = connect_fixture(
            &["--deaf"],
            McpTimeouts {
                connect_ms: Some(300),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.status(), 504);
        assert!(
            error
                .to_string()
                .contains("MCP initialize on server \"echo\" timed out after 300ms"),
            "{error}"
        );
        // Bounded, and bounded by the deadline we set — not a production default.
        assert!(
            started.elapsed().as_secs() < 10,
            "connect must not outlast its deadline"
        );
    }

    #[tokio::test]
    async fn a_binary_that_does_not_exist_fails_at_spawn_naming_the_command() {
        let _live = fixture::live_children_lock().read().await;
        let error = McpStdioClient::connect(McpStdioOptions {
            name: Some("ghost".to_string()),
            argv: vec![
                "/nonexistent/bough-mcp-server".to_string(),
                "--serve".to_string(),
            ],
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert_eq!(error.status(), 502);
        assert!(
            error
                .to_string()
                .contains("MCP server \"ghost\" failed to start"),
            "{error}"
        );
        assert!(error.to_string().contains("bough-mcp-server"), "{error}");
    }

    #[tokio::test]
    async fn a_process_that_exits_immediately_fails_the_handshake_instead_of_hanging() {
        let _live = fixture::live_children_lock().read().await;
        let error = McpStdioClient::connect(McpStdioOptions {
            name: Some("quitter".to_string()),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 1".to_string(),
            ],
            env: child_env(
                &ServerConfig {
                    command: Some("/bin/sh".into()),
                    ..Default::default()
                },
                &McpConfigOptions::default(),
            )
            .unwrap(),
            timeouts: Some(McpTimeouts {
                connect_ms: Some(20_000),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("MCP server \"quitter\" exited"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn requests_after_close_reject_rather_than_resolving_on_a_dead_pipe() {
        let _live = fixture::live_children_lock().read().await;
        let client = connect_fixture(&[], McpTimeouts::default()).await.unwrap();
        client.close().await;
        let error = client.list_tools().await.unwrap_err();
        assert!(error.to_string().contains("is not running"), "{error}");
        assert!(client
            .call_tool("echo", json!({"text": "hi"}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_server_initiated_request_is_refused_rather_than_ignored() {
        let _live = fixture::live_children_lock().read().await;
        // A server blocked on a reply that never comes is the same hang seen
        // from the other end of the pipe.
        let client = connect_fixture(&[], McpTimeouts::default()).await.unwrap();
        let pinged = client.call_tool("ping", json!({})).await.unwrap();
        assert_eq!(
            pinged.content.as_ref().unwrap()[0].text.as_deref(),
            Some("pinged")
        );
        // The fixture echoes what it received back on stderr.
        let mut tail = String::new();
        for _ in 0..50 {
            tail = client.stderr_tail();
            if tail.contains("-32601") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            tail.contains("bough does not support sampling/createMessage"),
            "{tail}"
        );
        client.close().await;
    }

    #[tokio::test]
    async fn shutdown_kills_every_live_server_the_wiring_boot_calls() {
        let _live = fixture::live_children_lock().write().await;
        let before = live_mcp_server_count();
        let a = connect_fixture(&[], McpTimeouts::default()).await.unwrap();
        let b = connect_fixture(&[], McpTimeouts::default()).await.unwrap();
        assert_eq!(live_mcp_server_count(), before + 2);

        assert!(kill_all_mcp_servers() >= 2);
        assert_eq!(live_mcp_server_count(), before);

        // The children are actually gone, and closing a killed client is safe.
        a.close().await;
        b.close().await;
        assert!(!a.alive());
        assert!(!b.alive());
    }

    #[test]
    fn an_unrecognized_call_result_becomes_structured_content_never_a_failure() {
        let odd = call_result(&json!({"anything": 1}));
        assert_eq!(odd.structured_content, Some(json!({"anything": 1})));
        assert!(odd.content.is_none());
        let scalar = call_result(&json!("plain"));
        assert_eq!(scalar.structured_content, Some(json!("plain")));
    }

    #[test]
    fn a_tool_entry_with_no_usable_name_is_dropped_entirely() {
        assert!(tool_info(&json!({"description": "an entry with no name"})).is_none());
        assert!(tool_info(&json!({"name": ""})).is_none());
        // …and one with a sloppy schema is kept, because it is callable.
        let loose =
            tool_info(&json!({"name": "loose", "inputSchema": {"properties": {"q": {}}}})).unwrap();
        assert!(loose.input_schema.unwrap().properties.is_some());
    }

    #[test]
    fn the_handshake_is_validated_strictly() {
        assert!(parse_initialize(&json!({"hello": "there"})).is_none());
        assert!(parse_initialize(&json!({"protocolVersion": "2025-06-18"})).is_none());
        let ok = parse_initialize(&json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "serverInfo": {"name": "s", "version": "1"}
        }))
        .unwrap();
        assert_eq!(ok.protocol_version, "2025-06-18");
    }
}
