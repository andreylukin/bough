//! The MCP connection manager, and the grant that decides who may ask (port of
//! `src/mcp/manager.ts`).
//!
//! THE INVARIANT THIS HOLDS: **a turn may call exactly the servers a human
//! granted it — and a subagent doing part of that granted work may call the same
//! set, and nothing else.** Two halves, and both are load-bearing:
//!
//!   1. **Registration is not a grant** (`config.rs`). Every call goes through
//!      [`require_granted`], which reads the grant FRESH: a program cannot enable
//!      a server for itself, and a grant revoked between turns is gone from the
//!      very next check with nothing to sweep.
//!   2. **The grant carries into subagents.** A subagent has a fresh, task-only
//!      thread and no activations of its own, so a child that resolved its own
//!      grant would resolve to *nothing* and every delegated MCP task would die
//!      at the first tool call — while a child that re-read the file would pick
//!      up grants made after it was spawned. Neither is what the human
//!      authorized. So the grant is CAPTURED AT SPAWN: a top-level turn carries
//!      [`McpGrant::Live`] (re-read per access), and the spawn converts it to
//!      [`McpGrant::Inherited`] — a plain snapshot.
//!
//! The enum IS the marker. The TS build installed `mcpGrant` as a live getter
//! plus a non-enumerable `Symbol.for("bough.mcp.liveGrant")`, precisely so a
//! bound top-level ctx could be told apart from a subagent's inherited snapshot
//! (both merely "have an mcpGrant") — without that distinction a top-level turn
//! is told it "inherited a grant it cannot widen", which is both false and the
//! opposite of the move that fixes it. In Rust the two states are two variants
//! and the distinction cannot be lost in a spread.
//!
//! NOTHING HERE IS CACHED. The registry and the activations are re-read per
//! operation, connections are consulted live, and [`McpManager::statuses`]
//! reports what the process actually holds at the instant it is called. A status
//! served from a cache is how a model ends up confidently calling a tool that was
//! revoked two turns ago. The only thing kept between calls is the connection
//! itself, which is a live process, not an answer.
//!
//! A SERVER THAT DOES NOT WORK IS A NAMED STATUS, NEVER A HANG. The client
//! already bounds every path out of a broken server (`client.rs`); this layer
//! adds the part the model sees: a failed connect is REMEMBERED per (session,
//! server) and reported as `state: "failed"` with the reason, so a down server
//! degrades to a line in `bough mcp` rather than to an exception the model has to
//! have caught, or to a spinner.
//!
//! A STDIO CONNECTION IS PER (SESSION, SERVER); A REMOTE ONE IS SHARED. Two
//! sessions working on different checkouts must not share one child process: the
//! child's cwd is the session's workspace, and a filesystem-backed server handed
//! the wrong tree answers about the wrong project. That reasoning is entirely
//! about a subprocess and a directory, and a remote server has neither — so
//! keying one by session bought nothing and cost the obvious thing: every new
//! conversation opened a second connection to the same endpoint and, until it
//! did, reported the server as not connected. [`scope_for`] is where the two part
//! company.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use futures::future::{BoxFuture, FutureExt, Shared};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::BoughError;
use crate::mcp::client::{
    McpCallResult, McpConnection, McpStdioClient, McpStdioOptions, McpTimeouts, McpToolInfo,
};
use crate::mcp::config::{
    activations_for, child_env, expand_headers, is_stdio, load_registry, mcp_error, require_server,
    McpConfigOptions, ServerConfig,
};
use crate::mcp::keychain::KeychainOptions;
use crate::mcp::oauth::{has_tokens, TokenStoreOptions};
use crate::mcp::remote::{McpRemoteClient, RemoteConnectOptions};
use crate::types::{system_clock, Clock, TurnCtx};

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// Reap a connection this long after its last use (on the next manager touch).
pub const IDLE_MS: i64 = 30 * 60_000;

/// The pool scope for remote connections. Not a session id, and it cannot
/// collide with one — session ids are UUIDs.
pub const SHARED_SCOPE: &str = "";

/// What a spawned server needs from the turn that wants it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpawnCtx {
    /// The child's cwd — the session's checkout, so a filesystem server sees it.
    pub workspace: String,
}

impl SpawnCtx {
    pub fn new(workspace: impl Into<String>) -> SpawnCtx {
        SpawnCtx {
            workspace: workspace.into(),
        }
    }
}

/// One server's connect outcome: its tools, or the sentence explaining why none.
#[derive(Clone, Debug, Default)]
pub struct ServerCatalog {
    pub name: String,
    pub tools: Vec<McpToolInfo>,
    pub error: Option<String>,
}

/// Why a (session, server) pair is not usable, in one word.
///
/// The old status carried only `alive: boolean`, which cannot tell "it never
/// started" from "it started and died" — and a server that failed to start had no
/// status row at all, so the one case the model most needs to see was invisible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpConnState {
    Connected,
    Exited,
    Failed,
}

/// One (session, server) pair as `bough mcp` and `GET /mcp/servers` report it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnStatus {
    pub server: String,
    pub session_id: String,
    pub state: McpConnState,
    pub alive: bool,
    pub tool_count: usize,
    /// Tool NAMES, so `bough mcp` carries a callable catalog: the live answer to
    /// "what can I call right now", which is the question the model is told to
    /// ask from a fresh call rather than from memory.
    pub tools: Vec<String>,
    pub last_used: i64,
    /// Present when `state` is not `connected`: what failed, and what resolves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
}

#[derive(Clone)]
struct Conn {
    client: Arc<dyn McpConnection>,
    tools: Vec<McpToolInfo>,
    spawn: SpawnCtx,
    session_id: String,
    server: String,
    last_used: i64,
}

/// A remembered connect failure — the reason a status row exists with no
/// connection.
#[derive(Clone, Debug)]
struct Failure {
    session_id: String,
    server: String,
    error: String,
    at: i64,
}

/// What a connector is handed. Injected so a test drives the whole manager —
/// grants, catalogs, degradation — against a fake transport, and so the remote
/// transport (row 3.4) can be installed without this file knowing about it.
#[derive(Clone)]
pub struct ConnectSpec {
    pub name: String,
    pub server: ServerConfig,
    pub spawn: SpawnCtx,
    pub config: McpConfigOptions,
    pub timeouts: Option<McpTimeouts>,
}

/// How a registry entry becomes a live connection.
pub type Connector = Arc<
    dyn Fn(ConnectSpec) -> BoxFuture<'static, Result<Arc<dyn McpConnection>, BoughError>>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct McpManagerOptions {
    /// Where the registry and grants live, and where `${VAR}` comes from.
    pub config: Option<McpConfigOptions>,
    /// Injected clock, epoch ms. Absent = the system clock.
    pub now: Option<Clock>,
    /// Absent = spawn a stdio child / open a remote transport.
    pub connect: Option<Connector>,
    /// Client deadlines. A test turns these down so a no-hang assertion is fast.
    pub timeouts: Option<McpTimeouts>,
    /// Idle reap window. Absent = [`IDLE_MS`].
    pub idle_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/// How this turn holds its grant.
///
/// `Live` re-reads the activations on every access, which is what makes a
/// revocation visible to the very next call. `Inherited` is the spawn-time
/// snapshot a subagent carries — including an EMPTY one, which is a grant
/// ("nothing") and not an absent one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpGrant {
    Live { session_id: String },
    Inherited(Vec<String>),
}

impl McpGrant {
    /// The value to hand a child at spawn: a Live grant is resolved NOW, an
    /// inherited one is passed through unwidened.
    pub fn snapshot(&self, opts: &McpConfigOptions) -> Vec<String> {
        match self {
            McpGrant::Live { session_id } => activations_for(Some(session_id), opts),
            McpGrant::Inherited(names) => names.clone(),
        }
    }
}

/// The ctx fields a grant decision needs. Narrow so a test needs no turn.
#[derive(Clone, Debug, Default)]
pub struct GrantCtx {
    pub session_id: String,
    /// Absent = this ctx was never bound; it resolves like a Live grant for its
    /// own session and hands a subagent nothing.
    pub grant: Option<McpGrant>,
}

impl GrantCtx {
    /// A bare session id — the shape the HTTP layer has.
    pub fn for_session(session_id: impl Into<String>) -> GrantCtx {
        GrantCtx {
            session_id: session_id.into(),
            grant: None,
        }
    }

    /// The grant a running turn carries.
    pub fn from_turn(ctx: &TurnCtx) -> GrantCtx {
        GrantCtx {
            session_id: ctx.session_id.clone(),
            grant: ctx.mcp_grant.clone(),
        }
    }

    /// True when this ctx holds a grant handed down from a spawner.
    pub fn is_inherited(&self) -> bool {
        matches!(self.grant, Some(McpGrant::Inherited(_)))
    }
}

/// The servers this turn may call, resolved fresh.
///
/// An INHERITED grant wins outright — including an empty one. A subagent spawned
/// by an ungranted turn must stay ungranted rather than falling through to the
/// global scope and quietly acquiring servers its spawner never had.
pub fn resolve_grant(ctx: &GrantCtx, opts: &McpConfigOptions) -> Vec<String> {
    match &ctx.grant {
        Some(McpGrant::Inherited(names)) => names.clone(),
        Some(McpGrant::Live { session_id }) => activations_for(Some(session_id), opts),
        None => activations_for(Some(&ctx.session_id), opts),
    }
}

/// Make a top-level turn's grant readable — and therefore inheritable — without
/// freezing it.
///
/// Idempotent, and it never overwrites an inherited grant: a subagent's ctx
/// already carries its spawner's snapshot, and re-deriving it from the child's
/// own (empty) activations would revoke it.
pub fn bind_turn_grant(ctx: &mut TurnCtx) {
    if ctx.mcp_grant.is_some() {
        return;
    }
    ctx.mcp_grant = Some(McpGrant::Live {
        session_id: ctx.session_id.clone(),
    });
}

/// The grant a spawn hands its child: the spawner's, resolved at this instant.
/// A turn that holds nothing hands nothing.
pub fn grant_for_spawn(ctx: &TurnCtx, opts: &McpConfigOptions) -> Option<Vec<String>> {
    ctx.mcp_grant.as_ref().map(|g| g.snapshot(opts))
}

/// Throw unless this turn may call `server`.
///
/// Three distinct outcomes, because collapsing them costs the model a round: a
/// name that is not registered at all (404, naming what is), a registered server
/// nobody granted (403, naming what *is* granted and who can grant it), and a
/// pass. A program cannot grant itself a server — saying so is what stops the
/// next round being spent trying.
pub fn require_granted(
    ctx: &GrantCtx,
    server: &str,
    opts: &McpConfigOptions,
) -> Result<(), BoughError> {
    require_server(server, opts)?; // 404 with the registered names
    let grant = resolve_grant(ctx, opts);
    if grant.iter().any(|g| g == server) {
        return Ok(());
    }
    Err(mcp_error(
        403,
        format!(
            "MCP server \"{server}\" is registered but not granted to this turn. {}{}",
            if grant.is_empty() {
                "Nothing is granted here. ".to_string()
            } else {
                format!("Granted here: {}. ", grant.join(", "))
            },
            if ctx.is_inherited() {
                "This session inherited its spawner's grant and cannot widen it — report what \
                 you could not do rather than retrying."
                    .to_string()
            } else {
                format!(
                    "A human grants one from /mcp (POST /mcp/servers/{server}/enable); a program \
                     cannot grant itself one. Say what you could not do and move on."
                )
            },
        ),
    ))
}

// ---------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------

type ConnectFuture = Shared<BoxFuture<'static, Result<Conn, BoughError>>>;

#[derive(Default)]
struct State {
    /// `"{scope} {server}"` → live connection.
    conns: HashMap<String, Conn>,
    /// In-flight connects, so concurrent callers share one spawn.
    connecting: HashMap<String, ConnectFuture>,
    /// `"{scope} {server}"` → the last connect failure, for the status surface.
    failures: HashMap<String, Failure>,
}

pub struct McpManager {
    state: Mutex<State>,
    opts: McpManagerOptions,
}

impl Default for McpManager {
    fn default() -> Self {
        McpManager::new(McpManagerOptions::default())
    }
}

impl McpManager {
    pub fn new(opts: McpManagerOptions) -> McpManager {
        McpManager {
            state: Mutex::new(State::default()),
            opts,
        }
    }

    fn now(&self) -> i64 {
        match &self.opts.now {
            Some(clock) => clock(),
            None => system_clock()(),
        }
    }

    /// Where this manager reads the registry and the grants.
    pub fn config(&self) -> McpConfigOptions {
        self.opts.config.clone().unwrap_or_default()
    }

    /// Connect (or reuse) each named server and report its catalog.
    ///
    /// A server that cannot connect yields an `error` entry instead of throwing:
    /// one broken server must not take the other three down with it, and the
    /// failure is recorded so `statuses()` reports it too.
    pub async fn ensure(
        &self,
        session_id: &str,
        servers: &[String],
        spawn: &SpawnCtx,
    ) -> Vec<ServerCatalog> {
        self.sweep();
        let registry = load_registry(&self.config()).servers;
        let mut out = Vec::with_capacity(servers.len());
        for name in servers {
            let Some(cfg) = registry.get(name) else {
                let error = format!("not registered — register it with PUT /mcp/servers/{name}");
                self.record_failure(session_id, name, &error);
                out.push(ServerCatalog {
                    name: name.clone(),
                    tools: Vec::new(),
                    error: Some(error),
                });
                continue;
            };
            match self.acquire(session_id, name, cfg, spawn).await {
                Ok(conn) => out.push(ServerCatalog {
                    name: name.clone(),
                    tools: conn.tools,
                    error: None,
                }),
                Err(e) => out.push(ServerCatalog {
                    name: name.clone(),
                    tools: Vec::new(),
                    error: Some(e.to_string()),
                }),
            }
        }
        out
    }

    /// Invoke one tool, connecting on demand.
    ///
    /// Connecting lazily means a granted server is callable the moment it is
    /// granted, and the only failures left are real ones: the server does not
    /// start, the tool does not exist, or the tool itself failed. An `isError`
    /// result THROWS with the server's own text, so it rejects inside the program
    /// like any other host-fn failure.
    pub async fn call(
        &self,
        session_id: &str,
        server: &str,
        tool: &str,
        args: Value,
        spawn: &SpawnCtx,
    ) -> Result<Value, BoughError> {
        self.sweep();
        let conn = self.live(session_id, server, spawn).await?;
        self.touch(&conn);
        if !conn.tools.iter().any(|t| t.name == tool) {
            let names: Vec<&str> = conn.tools.iter().map(|t| t.name.as_str()).collect();
            return Err(mcp_error(
                404,
                format!(
                    "MCP server \"{server}\" has no tool \"{tool}\". It advertises: {}. \
                     Run `bough mcp` for the live catalog rather than guessing a name.",
                    if names.is_empty() {
                        "(none)".to_string()
                    } else {
                        names.join(", ")
                    },
                ),
            ));
        }
        map_result(server, tool, conn.client.call_tool(tool, args).await?)
    }

    /// Drop and re-establish one (session, server) connection.
    pub async fn restart(
        &self,
        session_id: &str,
        server: &str,
        spawn: Option<&SpawnCtx>,
    ) -> Result<ConnStatus, BoughError> {
        let previous = {
            let state = self.state.lock().unwrap();
            state
                .conns
                .get(&key(session_id, server))
                .map(|c| c.spawn.clone())
        };
        let Some(where_) = spawn.cloned().or(previous) else {
            return Err(mcp_error(
                400,
                format!(
                    "MCP server \"{server}\" has no connection for this session to restart, and \
                     no workspace to start one in. Connect it first \
                     (POST /mcp/servers/{server}/connect)."
                ),
            ));
        };
        self.drop_conn(session_id, server).await;
        let cfg = require_server(server, &self.config())?;
        match self.acquire(session_id, server, &cfg, &where_).await {
            Ok(conn) => Ok(status_of(&conn)),
            Err(e) => {
                let failed = {
                    let state = self.state.lock().unwrap();
                    state.failures.get(&key(session_id, server)).cloned()
                };
                match failed {
                    Some(f) => Ok(failure_status(&f)),
                    None => Err(e),
                }
            }
        }
    }

    /// Live rows for one session (or every session), plus the failures that
    /// explain a server with no connection. Never connects and never fails —
    /// status is a read.
    pub fn statuses(&self, session_id: Option<&str>) -> Vec<ConnStatus> {
        let mut state = self.state.lock().unwrap();
        let mut rows: Vec<ConnStatus> = Vec::new();
        let mut superseded: Vec<String> = Vec::new();
        for (k, conn) in state.conns.iter() {
            // A shared (remote) connection belongs to every conversation, so it
            // is reported in all of them — otherwise the panel says "not
            // connected" about a server that is connected and about to answer.
            if let Some(id) = session_id {
                if conn.session_id != id && conn.session_id != SHARED_SCOPE {
                    continue;
                }
            }
            rows.push(status_of(conn));
            superseded.push(k.clone());
        }
        for k in superseded {
            state.failures.remove(&k); // a live connection supersedes an old failure
        }
        for (k, failure) in state.failures.iter() {
            if let Some(id) = session_id {
                if failure.session_id != id && failure.session_id != SHARED_SCOPE {
                    continue;
                }
            }
            if state.conns.contains_key(k) {
                continue;
            }
            rows.push(failure_status(failure));
        }
        rows.sort_by(|a, b| a.server.cmp(&b.server));
        rows
    }

    /// Close one session's connection to one server. No-op when there is none.
    ///
    /// Named `drop_conn` and not `drop`: a method called `drop` on a type is
    /// shadowed by `Drop::drop` at every call site, so `manager.drop(a, b)`
    /// does not compile.
    ///
    /// BOTH scopes are tried, because the caller knows a session id and not which
    /// kind of entry this is — and a revoke that missed a shared remote
    /// connection would leave it serving every OTHER conversation.
    pub async fn drop_conn(&self, session_id: &str, server: &str) {
        let mut scopes = vec![session_id];
        if session_id != SHARED_SCOPE {
            scopes.push(SHARED_SCOPE);
        }
        let mut closing = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            for scope in scopes {
                let k = key(scope, server);
                state.failures.remove(&k);
                if let Some(conn) = state.conns.remove(&k) {
                    closing.push(conn);
                }
            }
        }
        for conn in closing {
            conn.client.close().await;
        }
    }

    /// Close every session's connection to one server — a registry edit, a
    /// removal, or cleared auth. A changed entry must not keep serving from the
    /// old definition.
    pub async fn drop_server(&self, server: &str) {
        let closing: Vec<Conn> = {
            let mut state = self.state.lock().unwrap();
            let keys: Vec<String> = state
                .conns
                .iter()
                .filter(|(_, c)| c.server == server)
                .map(|(k, _)| k.clone())
                .collect();
            let mut live = Vec::new();
            for k in keys {
                if let Some(conn) = state.conns.remove(&k) {
                    live.push(conn);
                }
            }
            let stale: Vec<String> = state
                .failures
                .iter()
                .filter(|(_, f)| f.server == server)
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale {
                state.failures.remove(&k);
            }
            live
        };
        for conn in closing {
            conn.client.close().await;
        }
    }

    /// Close everything. Shutdown, and the teardown of every test that connects.
    pub async fn drop_all(&self) {
        let closing: Vec<Conn> = {
            let mut state = self.state.lock().unwrap();
            let live: Vec<Conn> = state.conns.drain().map(|(_, c)| c).collect();
            state.failures.clear();
            live
        };
        for conn in closing {
            conn.client.close().await;
        }
    }

    // -- internals -----------------------------------------------------------

    /// An alive connection, reconnecting a dead or absent one.
    async fn live(
        &self,
        session_id: &str,
        server: &str,
        spawn: &SpawnCtx,
    ) -> Result<Conn, BoughError> {
        let cfg = require_server(server, &self.config())?;
        let existing = {
            let state = self.state.lock().unwrap();
            state
                .conns
                .get(&key(&scope_for(session_id, &cfg), server))
                .cloned()
        };
        match existing {
            Some(conn) if conn.client.alive() => return Ok(conn),
            Some(_) => self.drop_conn(session_id, server).await,
            None => {}
        }
        self.acquire(session_id, server, &cfg, spawn).await
    }

    async fn acquire(
        &self,
        session_id: &str,
        server: &str,
        cfg: &ServerConfig,
        spawn: &SpawnCtx,
    ) -> Result<Conn, BoughError> {
        // Remote servers pool under one shared scope — see `scope_for`.
        let scope = scope_for(session_id, cfg);
        let k = key(&scope, server);
        let now = self.now();

        // Reuse, refreshing `lastUsed` and overwriting `spawn`: a later turn's
        // workspace wins for the next respawn.
        {
            let mut state = self.state.lock().unwrap();
            if let Some(live) = state.conns.get_mut(&k) {
                if live.client.alive() {
                    live.last_used = now;
                    live.spawn = spawn.clone();
                    return Ok(live.clone());
                }
            }
        }

        // One in-flight connect per key, shared by every concurrent caller.
        let shared = {
            let mut state = self.state.lock().unwrap();
            match state.connecting.get(&k) {
                Some(f) => f.clone(),
                None => {
                    let fut = connect_one(
                        self.opts.connect.clone().unwrap_or_else(default_connector),
                        ConnectSpec {
                            name: server.to_string(),
                            server: cfg.clone(),
                            spawn: spawn.clone(),
                            config: self.config(),
                            timeouts: self.opts.timeouts,
                        },
                        scope.clone(),
                        now,
                    )
                    .boxed()
                    .shared();
                    state.connecting.insert(k.clone(), fut.clone());
                    fut
                }
            }
        };
        let result = shared.await;
        {
            let mut state = self.state.lock().unwrap();
            state.connecting.remove(&k);
            match &result {
                Ok(conn) => {
                    state.conns.insert(k.clone(), conn.clone());
                    state.failures.remove(&k);
                }
                Err(e) => {
                    state.failures.insert(
                        k.clone(),
                        Failure {
                            session_id: scope.clone(),
                            server: server.to_string(),
                            error: e.to_string(),
                            at: now,
                        },
                    );
                }
            }
        }
        result
    }

    fn record_failure(&self, session_id: &str, server: &str, error: &str) {
        let at = self.now();
        let mut state = self.state.lock().unwrap();
        state.failures.insert(
            key(session_id, server),
            Failure {
                session_id: session_id.to_string(),
                server: server.to_string(),
                error: error.to_string(),
                at,
            },
        );
    }

    fn touch(&self, conn: &Conn) {
        let now = self.now();
        let mut state = self.state.lock().unwrap();
        if let Some(live) = state.conns.get_mut(&key(&conn.session_id, &conn.server)) {
            live.last_used = now;
        }
    }

    /// Reap dead and idle connections. Opportunistic — no timer, no background
    /// work: a quiet server holds a subprocess for at most `idleMs` past its last
    /// call and the process has nothing to shut down.
    fn sweep(&self) {
        let now = self.now();
        let idle_ms = self.opts.idle_ms.unwrap_or(IDLE_MS);
        let reaped: Vec<Conn> = {
            let mut state = self.state.lock().unwrap();
            let keys: Vec<String> = state
                .conns
                .iter()
                .filter(|(_, c)| !c.client.alive() || now - c.last_used > idle_ms)
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .filter_map(|k| state.conns.remove(&k))
                .collect()
        };
        for conn in reaped {
            // Fire-and-forget, exactly as the TS `void conn.client.close()`: the
            // sweep runs at the top of a call and must not wait on a dead child.
            tokio::spawn(async move { conn.client.close().await });
        }
    }
}

/// Connect and list, or fail. Separated from the manager so the future it
/// produces owns everything it needs and can be shared between callers.
async fn connect_one(
    connector: Connector,
    spec: ConnectSpec,
    scope: String,
    now: i64,
) -> Result<Conn, BoughError> {
    let server = spec.name.clone();
    let spawn = spec.spawn.clone();
    let client = connector(spec).await?;
    match client.list_tools().await {
        Ok(tools) => Ok(Conn {
            client,
            tools,
            spawn,
            session_id: scope,
            server,
            last_used: now,
        }),
        Err(e) => {
            // A server that connects and cannot list its tools is not usable, and
            // leaving its child running would leak a process nobody can reach.
            client.close().await;
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// The default transport
// ---------------------------------------------------------------------------

/// Turn one registry entry into a live connection: a spawned child for a stdio
/// entry, the Streamable HTTP transport for a `url` one.
///
/// The two clients satisfy the same [`McpConnection`], so this is the only place
/// in the manager that knows which kind an entry is.
pub fn default_connector() -> Connector {
    Arc::new(|spec: ConnectSpec| {
        async move {
            if !is_stdio(&spec.server) {
                return connect_remote(spec).await;
            }
            let command = spec.server.command.clone().unwrap_or_default();
            let mut argv = vec![command];
            argv.extend(spec.server.args.clone());
            let client = McpStdioClient::connect(McpStdioOptions {
                name: Some(spec.name.clone()),
                argv,
                // The entry's own `cwd` wins; otherwise the session's checkout,
                // so a filesystem-backed server sees the tree the turn is
                // working in.
                cwd: Some(
                    spec.server
                        .cwd
                        .clone()
                        .unwrap_or(spec.spawn.workspace.clone()),
                ),
                // The child's ENTIRE environment, composed by `config.rs` — a
                // third-party binary never inherits the user's provider keys.
                env: child_env(&spec.server, &spec.config)?,
                timeouts: spec.timeouts,
            })
            .await?;
            Ok(Arc::new(client) as Arc<dyn McpConnection>)
        }
        .boxed()
    })
}

/// The remote arm: the Streamable HTTP transport (`remote.rs`), with the
/// entry's static headers expanded HERE and never at load — a `${VAR}` or
/// `${keychain:…}` reference is resolved at the moment it is sent, so the secret
/// never enters the registry document, the `GET /mcp/servers` body, or the
/// `/mcp` panel.
///
/// A 401 arrives as an auth-required error whose message is the "^p, then a"
/// prompt rather than a fault, straight through `ensure`'s catalog error and
/// into the `failed` status row.
async fn connect_remote(spec: ConnectSpec) -> Result<Arc<dyn McpConnection>, BoughError> {
    let headers = static_headers(
        &spec.name,
        &spec.server,
        &spec.config,
        &KeychainOptions::default(),
        &TokenStoreOptions::default(),
    )
    .await?;
    let client = McpRemoteClient::connect(RemoteConnectOptions {
        name: spec.name.clone(),
        url: spec.server.url.clone().unwrap_or_default(),
        headers: headers.unwrap_or_default(),
        timeouts: spec.timeouts,
        ..Default::default()
    })
    .await?;
    Ok(Arc::new(client) as Arc<dyn McpConnection>)
}

/// The entry's static headers, or none when they cannot be resolved AND bough
/// has its own way in.
///
/// WHY AN UNRESOLVABLE HEADER IS NOT ALWAYS FATAL. `sync-mcp` writes
/// `Authorization: Bearer ${keychain:…}` pointing at a grant another client
/// holds. That grant can go dead — the other client stops refreshing it, or
/// leaves the entry behind with an empty token — and `expand_headers` then fails.
/// Failing from here killed the connection before the transport existed, so the
/// OAuth provider was never built and a token bough had stored ITSELF was never
/// tried. The visible symptom was the one that makes no sense to the user:
/// authorize the server, watch it succeed, and watch the row stay `◐` forever,
/// because the thing failing was a header nobody had been told about.
///
/// So: if bough holds tokens for this server, a dead reference is stale baggage
/// and is dropped. If it does not, the reference IS the intended credential and
/// its failure is the honest answer. Only the resolution is guarded — a header
/// that resolves is sent unchanged.
pub async fn static_headers(
    name: &str,
    server: &ServerConfig,
    config: &McpConfigOptions,
    keychain: &KeychainOptions,
    tokens: &TokenStoreOptions,
) -> Result<Option<std::collections::BTreeMap<String, String>>, BoughError> {
    if server.headers.is_empty() {
        return Ok(None);
    }
    match expand_headers(&server.headers, config, keychain).await {
        Ok(headers) => Ok(Some(headers)),
        Err(error) => {
            // The default token directory, which is the one the remote client
            // also builds its provider against — asking a different store than
            // the one that will be used would answer the wrong question.
            if has_tokens(name, tokens) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The process-wide manager
// ---------------------------------------------------------------------------

fn active() -> &'static RwLock<Option<Arc<McpManager>>> {
    static ACTIVE: OnceLock<RwLock<Option<Arc<McpManager>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(None))
}

/// One manager per process, because a connection is a live subprocess: the turn
/// runner, the HTTP endpoints and the CLI all have to reach the SAME child, or
/// `POST /mcp/servers/x/connect` would prove a server works in a process nobody
/// else can see. Tests construct their own [`McpManager`] with an injected
/// connector and never touch this one.
pub fn mcp_manager() -> Arc<McpManager> {
    {
        if let Some(existing) = active().read().unwrap().as_ref() {
            return existing.clone();
        }
    }
    let mut slot = active().write().unwrap();
    slot.get_or_insert_with(|| Arc::new(McpManager::default()))
        .clone()
}

/// Swap the process manager (tests, and the boot wiring). Returns the previous
/// one.
pub fn set_mcp_manager(next: Arc<McpManager>) -> Arc<McpManager> {
    let previous = mcp_manager();
    *active().write().unwrap() = Some(next);
    previous
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn key(session_id: &str, server: &str) -> String {
    format!("{session_id} {server}")
}

/// Which scope owns this server's connection.
///
/// PER SESSION IS A STDIO PROPERTY, and only that: a stdio child is spawned in
/// the session's checkout, so sharing one across conversations would hand a
/// filesystem-backed server the wrong tree. A remote server has no tree — it is a
/// URL and a credential — so the same reasoning says the opposite for it.
pub fn scope_for(session_id: &str, cfg: &ServerConfig) -> String {
    if is_stdio(cfg) {
        session_id.to_string()
    } else {
        SHARED_SCOPE.to_string()
    }
}

fn status_of(conn: &Conn) -> ConnStatus {
    let alive = conn.client.alive();
    let tail = conn.client.stderr_tail().trim().to_string();
    ConnStatus {
        server: conn.server.clone(),
        session_id: conn.session_id.clone(),
        state: if alive {
            McpConnState::Connected
        } else {
            McpConnState::Exited
        },
        alive,
        tool_count: conn.tools.len(),
        tools: conn.tools.iter().map(|t| t.name.clone()).collect(),
        last_used: conn.last_used,
        error: if alive {
            None
        } else {
            Some("the server process exited; the next call restarts it".to_string())
        },
        stderr_tail: if tail.is_empty() {
            None
        } else {
            Some(last_chars(&tail, STDERR_STATUS_CHARS))
        },
    }
}

/// Stderr carried on a status row.
const STDERR_STATUS_CHARS: usize = 500;

fn last_chars(value: &str, n: usize) -> String {
    let count = value.chars().count();
    if count <= n {
        return value.to_string();
    }
    value.chars().skip(count - n).collect()
}

fn failure_status(failure: &Failure) -> ConnStatus {
    ConnStatus {
        server: failure.server.clone(),
        session_id: failure.session_id.clone(),
        state: McpConnState::Failed,
        alive: false,
        tool_count: 0,
        tools: Vec::new(),
        last_used: failure.at,
        error: Some(failure.error.clone()),
        stderr_tail: None,
    }
}

/// Text of a call result's content blocks.
fn text_of(result: &McpCallResult) -> String {
    result
        .content
        .as_ref()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.r#type == "text")
                .filter_map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// A tool result as the program sees it: the structured content when the server
/// sent some, otherwise its text. A tool that FAILED throws with the server's own
/// words — the program can catch it, and the sentence names the server and the
/// tool so the next round is not spent asking which one broke.
fn map_result(server: &str, tool: &str, result: McpCallResult) -> Result<Value, BoughError> {
    if result.is_error.unwrap_or(false) {
        let text = text_of(&result);
        return Err(mcp_error(
            502,
            format!(
                "MCP {server}:{tool} failed: {}",
                if text.is_empty() {
                    "the server reported an error with no text".to_string()
                } else {
                    text
                }
            ),
        ));
    }
    let text = text_of(&result);
    Ok(result.structured_content.unwrap_or(Value::String(text)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::client::fixture;
    use crate::mcp::config::{save_registry, set_activation, ttl_to_expires};
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ---- fixtures ---------------------------------------------------------

    fn tmp_registry() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-mcp-manager-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("mcp.json")
    }

    /// A registry holding the fixture server under `name`, plus anything extra.
    fn seed_registry(file: &Path, name: &str, extra: Value) {
        let script = fixture::echo_server().to_string_lossy().to_string();
        let mut servers = serde_json::Map::new();
        servers.insert(
            name.to_string(),
            json!({"command": "/bin/sh", "args": [script]}),
        );
        if let Value::Object(more) = extra {
            for (k, v) in more {
                servers.insert(k, v);
            }
        }
        save_registry(&json!({ "servers": servers }), &opts(file)).unwrap();
    }

    fn opts(file: &Path) -> McpConfigOptions {
        McpConfigOptions::with_file(file)
    }

    /// Deadlines short enough that a regression reads as a failure, not a hung
    /// suite.
    const TIMEOUTS: McpTimeouts = McpTimeouts {
        connect_ms: Some(20_000),
        request_ms: Some(20_000),
        call_ms: Some(20_000),
    };

    fn manager(file: &Path, connect: Option<Connector>) -> McpManager {
        McpManager::new(McpManagerOptions {
            config: Some(opts(file)),
            timeouts: Some(TIMEOUTS),
            connect,
            ..Default::default()
        })
    }

    /// A connection that never spawns anything — for the paths where the point is
    /// the manager's bookkeeping, not a real server.
    struct FakeConnection {
        name: String,
        tools: Vec<String>,
        alive: AtomicBool,
        closed: Arc<Mutex<Vec<String>>>,
        label: String,
    }

    #[async_trait]
    impl McpConnection for FakeConnection {
        fn name(&self) -> &str {
            &self.name
        }
        async fn list_tools(&self) -> Result<Vec<McpToolInfo>, BoughError> {
            Ok(self
                .tools
                .iter()
                .map(|t| McpToolInfo {
                    name: t.clone(),
                    description: Some(format!("the {t} tool")),
                    ..Default::default()
                })
                .collect())
        }
        async fn call_tool(&self, tool: &str, args: Value) -> Result<McpCallResult, BoughError> {
            Ok(McpCallResult {
                content: Some(vec![crate::mcp::client::McpContentBlock {
                    r#type: "text".into(),
                    text: Some(format!("{tool}:{args}")),
                }]),
                ..Default::default()
            })
        }
        async fn close(&self) {
            self.alive.store(false, Ordering::SeqCst);
            self.closed.lock().unwrap().push(self.label.clone());
        }
        fn alive(&self) -> bool {
            self.alive.load(Ordering::SeqCst)
        }
        fn stderr_tail(&self) -> String {
            String::new()
        }
    }

    fn fake_connector(
        tools: &'static [&'static str],
        spawned: Arc<AtomicUsize>,
        closed: Arc<Mutex<Vec<String>>>,
    ) -> Connector {
        Arc::new(move |spec: ConnectSpec| {
            let spawned = spawned.clone();
            let closed = closed.clone();
            async move {
                spawned.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(FakeConnection {
                    name: spec.name.clone(),
                    tools: tools.iter().map(|t| t.to_string()).collect(),
                    alive: AtomicBool::new(true),
                    label: format!("{}@{}", spec.name, spec.spawn.workspace),
                    closed,
                }) as Arc<dyn McpConnection>)
            }
            .boxed()
        })
    }

    fn counter() -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(0))
    }

    fn sink() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    /// What a program does: check the grant, then call. The host functions this
    /// replaced are gone — a tool is reached through `bough mcp call` and the
    /// grant is enforced in the route — but grants and status did not move.
    async fn mcp_call(
        ctx: &GrantCtx,
        mgr: &McpManager,
        file: &Path,
        now: Option<i64>,
        server: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, BoughError> {
        let o = McpConfigOptions {
            file: Some(file.to_path_buf()),
            env: None,
            now,
        };
        require_granted(ctx, server, &o)?;
        mgr.call(&ctx.session_id, server, tool, args, &SpawnCtx::new("."))
            .await
    }

    // ---- grants -----------------------------------------------------------

    #[tokio::test]
    async fn a_registered_server_is_not_a_callable_one_until_a_human_grants_it() {
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let mgr = manager(&file, Some(fake_connector(&["echo"], counter(), sink())));
        let ctx = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };

        let refused = mcp_call(&ctx, &mgr, &file, None, "echo", "echo", json!({}))
            .await
            .unwrap_err();
        assert_eq!(refused.status(), 403);
        assert!(
            refused.to_string().contains("registered but not granted"),
            "{refused}"
        );
        // The message says who can fix it, so the next round is not spent trying.
        assert!(
            refused
                .to_string()
                .contains("a program cannot grant itself one"),
            "{refused}"
        );

        // An unregistered name is a different failure, and says so.
        let unknown = mcp_call(&ctx, &mgr, &file, None, "nope", "echo", json!({}))
            .await
            .unwrap_err();
        assert_eq!(unknown.status(), 404);
        assert!(
            unknown.to_string().contains("Registered servers: echo"),
            "{unknown}"
        );

        set_activation(Some("s1"), "echo", true, None, &opts(&file)).unwrap();
        let said = mcp_call(
            &ctx,
            &mgr,
            &file,
            None,
            "echo",
            "echo",
            json!({"text": "hi"}),
        )
        .await
        .unwrap();
        assert_eq!(said, json!("echo:{\"text\":\"hi\"}"));
        mgr.drop_all().await;
    }

    fn live(session_id: &str) -> Option<McpGrant> {
        Some(McpGrant::Live {
            session_id: session_id.to_string(),
        })
    }

    #[tokio::test]
    async fn a_lapsed_grant_fails_closed_and_the_clock_that_decides_is_injected() {
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let mgr = manager(&file, Some(fake_connector(&["echo"], counter(), sink())));
        let ctx = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };
        let now = 1_000_000i64;
        let expires = ttl_to_expires("2h", now).unwrap();
        set_activation(Some("s1"), "echo", true, Some(&expires), &opts(&file)).unwrap();

        let at = |t: i64| McpConfigOptions {
            file: Some(file.clone()),
            env: None,
            now: Some(t),
        };
        assert_eq!(resolve_grant(&ctx, &at(now)), vec!["echo"]);
        assert!(
            mcp_call(&ctx, &mgr, &file, Some(now), "echo", "echo", json!({}))
                .await
                .is_ok()
        );

        let later = now + 3 * 3_600_000; // two hours later, plus change
        assert!(
            resolve_grant(&ctx, &at(later)).is_empty(),
            "the grant lapsed with no sweep"
        );
        let refused = mcp_call(&ctx, &mgr, &file, Some(later), "echo", "echo", json!({}))
            .await
            .unwrap_err();
        assert_eq!(refused.status(), 403);
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn a_revoked_grant_is_visible_to_the_very_next_call() {
        // Not the next turn — the next CALL, with nothing else touched. A cache
        // of any lifetime fails this.
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let mgr = manager(&file, Some(fake_connector(&["echo"], counter(), sink())));
        let ctx = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };
        set_activation(Some("s1"), "echo", true, None, &opts(&file)).unwrap();
        assert_eq!(resolve_grant(&ctx, &opts(&file)), vec!["echo"]);

        set_activation(Some("s1"), "echo", false, None, &opts(&file)).unwrap();
        assert!(resolve_grant(&ctx, &opts(&file)).is_empty());
        let refused = mcp_call(&ctx, &mgr, &file, None, "echo", "echo", json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            refused.status(),
            403,
            "status and enforcement read the same grant"
        );

        // The other direction: a grant made mid-program is live immediately.
        set_activation(Some("s1"), "echo", true, None, &opts(&file)).unwrap();
        assert_eq!(resolve_grant(&ctx, &opts(&file)), vec!["echo"]);
        mgr.drop_all().await;
    }

    #[test]
    fn an_ungranted_spawner_hands_its_subagent_nothing_not_the_global_scope() {
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        set_activation(None, "echo", true, None, &opts(&file)).unwrap(); // GLOBALLY
        let o = opts(&file);

        // A top-level turn sees the global grant…
        assert_eq!(
            resolve_grant(&GrantCtx::for_session("s1"), &o),
            vec!["echo"]
        );
        // …but a child spawned by a turn that held nothing stays at nothing. An
        // empty inherited grant is a grant, not an absent one.
        let child = GrantCtx {
            session_id: "child".into(),
            grant: Some(McpGrant::Inherited(vec![])),
        };
        assert!(resolve_grant(&child, &o).is_empty());
        let refused = require_granted(&child, "echo", &o).unwrap_err();
        assert_eq!(refused.status(), 403);
        assert!(refused.to_string().contains("cannot widen it"), "{refused}");

        // …and the wording for a TOP-LEVEL turn is the opposite move.
        let top = GrantCtx {
            session_id: "s2".into(),
            grant: live("s2"),
        };
        set_activation(None, "echo", false, None, &o).unwrap();
        let refused = require_granted(&top, "echo", &o).unwrap_err();
        assert!(
            refused.to_string().contains("A human grants one"),
            "{refused}"
        );
    }

    #[test]
    fn bind_turn_grant_is_a_live_read_and_never_overwrites_an_inherited_one() {
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let o = opts(&file);

        let mut ctx = crate::agents::testkit::turn_ctx_for(
            &crate::agents::testkit::shared_db(),
            "s1",
            "t1",
            0,
        );
        bind_turn_grant(&mut ctx);
        let bound = GrantCtx::from_turn(&ctx);
        assert!(resolve_grant(&bound, &o).is_empty());
        set_activation(Some("s1"), "echo", true, None, &o).unwrap();
        assert_eq!(
            resolve_grant(&bound, &o),
            vec!["echo"],
            "re-read on every access"
        );
        assert!(
            !bound.is_inherited(),
            "a bound top-level grant is not an inherited one"
        );

        // The spawn takes the snapshot…
        let snapshot = grant_for_spawn(&ctx, &o).unwrap();
        assert_eq!(snapshot, vec!["echo"]);

        // …and an inherited grant is left exactly as it was: re-deriving it from
        // the child's own (empty) activations would revoke it.
        let mut child = crate::agents::testkit::turn_ctx_for(
            &crate::agents::testkit::shared_db(),
            "child",
            "t2",
            1,
        );
        child.mcp_grant = Some(McpGrant::Inherited(snapshot));
        bind_turn_grant(&mut child);
        set_activation(Some("s1"), "echo", false, None, &o).unwrap();
        assert_eq!(
            resolve_grant(&GrantCtx::from_turn(&child), &o),
            vec!["echo"]
        );
        assert!(GrantCtx::from_turn(&child).is_inherited());
        // A turn that holds nothing hands its child nothing.
        assert!(grant_for_spawn(
            &crate::agents::testkit::turn_ctx_for(
                &crate::agents::testkit::shared_db(),
                "s9",
                "t3",
                0
            ),
            &o
        )
        .is_none());
    }

    // ---- a down server degrades to a named status -------------------------

    #[tokio::test]
    async fn a_server_that_cannot_start_degrades_to_a_named_status_not_a_hang() {
        let _live = fixture::live_children_lock().read().await;
        let file = tmp_registry();
        save_registry(
            &json!({"servers": {"broken": {"command": "/nonexistent/mcp-server-binary"}}}),
            &opts(&file),
        )
        .unwrap();
        let mgr = manager(&file, None);
        let ctx = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };
        set_activation(Some("s1"), "broken", true, None, &opts(&file)).unwrap();

        // The call fails by name, bounded, with the move that resolves it.
        let failed = mcp_call(&ctx, &mgr, &file, None, "broken", "anything", json!({}))
            .await
            .unwrap_err();
        assert!(
            failed
                .to_string()
                .contains("MCP server \"broken\" failed to start"),
            "{failed}"
        );
        assert!(
            failed.to_string().contains("nonexistent/mcp-server-binary"),
            "{failed}"
        );

        // And the status the model is told to read carries it as a NAMED row.
        let rows = mgr.statuses(Some("s1"));
        let row = rows
            .iter()
            .find(|r| r.server == "broken")
            .expect("a failed server has a row");
        assert_eq!(row.state, McpConnState::Failed);
        assert!(!row.alive);
        assert!(row.tools.is_empty());
        assert!(row
            .error
            .as_deref()
            .unwrap_or("")
            .contains("failed to start"));
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn a_server_that_dies_is_reported_as_exited_and_the_next_call_restarts_it() {
        let _live = fixture::live_children_lock().read().await;
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let mgr = manager(&file, None);
        let ctx = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };
        set_activation(Some("s1"), "echo", true, None, &opts(&file)).unwrap();
        assert!(mcp_call(
            &ctx,
            &mgr,
            &file,
            None,
            "echo",
            "echo",
            json!({"text": "one"})
        )
        .await
        .is_ok());

        // `die` takes the child down mid-call. That is a call failure, by name.
        let died = mcp_call(&ctx, &mgr, &file, None, "echo", "die", json!({}))
            .await
            .unwrap_err();
        assert!(
            died.to_string().contains("MCP server \"echo\" exited"),
            "{died}"
        );

        let rows = mgr.statuses(Some("s1"));
        let row = rows.iter().find(|r| r.server == "echo").unwrap();
        assert_eq!(row.state, McpConnState::Exited);
        assert!(row
            .stderr_tail
            .as_deref()
            .unwrap_or("")
            .contains("asked to die"));

        // The next call reconnects rather than reporting a dead server forever.
        let healed = mcp_call(
            &ctx,
            &mgr,
            &file,
            None,
            "echo",
            "echo",
            json!({"text": "two"}),
        )
        .await
        .unwrap();
        assert_eq!(healed, json!({"echoed": "two"}));
        assert_eq!(
            mgr.statuses(Some("s1"))
                .iter()
                .find(|r| r.server == "echo")
                .unwrap()
                .state,
            McpConnState::Connected
        );
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn a_tool_failure_is_the_servers_own_words_and_an_unknown_tool_names_the_real_ones() {
        let _live = fixture::live_children_lock().read().await;
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let mgr = manager(&file, None);
        let ctx = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };
        set_activation(Some("s1"), "echo", true, None, &opts(&file)).unwrap();

        let boom = mcp_call(&ctx, &mgr, &file, None, "echo", "boom", json!({}))
            .await
            .unwrap_err();
        assert!(
            boom.to_string().contains("MCP echo:boom failed: kaboom"),
            "{boom}"
        );

        let typo = mcp_call(&ctx, &mgr, &file, None, "echo", "ecko", json!({}))
            .await
            .unwrap_err();
        assert_eq!(typo.status(), 404);
        assert!(typo.to_string().contains("has no tool \"ecko\""), "{typo}");
        assert!(
            typo.to_string()
                .contains("It advertises: echo, scream, boom, die, slow, loose"),
            "{typo}"
        );
        mgr.drop_all().await;
    }

    // ---- connection lifecycle ---------------------------------------------

    #[tokio::test]
    async fn connections_are_per_session_reused_across_calls_and_reaped_when_idle() {
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let spawned = counter();
        let clock = Arc::new(Mutex::new(0i64));
        let reader = clock.clone();
        let mgr = McpManager::new(McpManagerOptions {
            config: Some(opts(&file)),
            now: Some(Arc::new(move || *reader.lock().unwrap())),
            idle_ms: Some(1_000),
            connect: Some(fake_connector(&["echo"], spawned.clone(), sink())),
            ..Default::default()
        });
        set_activation(None, "echo", true, None, &opts(&file)).unwrap(); // global
        let one = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };
        let two = GrantCtx {
            session_id: "s2".into(),
            grant: live("s2"),
        };

        mcp_call(&one, &mgr, &file, None, "echo", "echo", json!({}))
            .await
            .unwrap();
        mcp_call(&one, &mgr, &file, None, "echo", "echo", json!({}))
            .await
            .unwrap();
        assert_eq!(
            spawned.load(Ordering::SeqCst),
            1,
            "one child serves every call in a session"
        );

        mcp_call(&two, &mgr, &file, None, "echo", "echo", json!({}))
            .await
            .unwrap();
        assert_eq!(
            spawned.load(Ordering::SeqCst),
            2,
            "a second session gets its own child"
        );
        assert_eq!(mgr.statuses(None).len(), 2);
        assert_eq!(
            mgr.statuses(Some("s1")).len(),
            1,
            "status is scoped to the session that asks"
        );

        *clock.lock().unwrap() += 5_000; // both are now idle past the window
        mcp_call(&one, &mgr, &file, None, "echo", "echo", json!({}))
            .await
            .unwrap();
        assert_eq!(
            spawned.load(Ordering::SeqCst),
            3,
            "the idle child was reaped, a fresh one spawned"
        );
        assert_eq!(
            mgr.statuses(None).len(),
            1,
            "the other session's idle child is gone too"
        );
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn a_remote_server_is_one_connection_for_every_conversation() {
        // Reported as "after I start the conversation, all of the mcps
        // disconnect". Keying by session is a statement about a subprocess and
        // its cwd; a remote server has neither.
        let file = tmp_registry();
        save_registry(
            &json!({"servers": {"remote": {"url": "https://mcp.example.com/mcp"}}}),
            &opts(&file),
        )
        .unwrap();
        let connects = counter();
        let mgr = manager(
            &file,
            Some(fake_connector(&["echo"], connects.clone(), sink())),
        );
        set_activation(None, "remote", true, None, &opts(&file)).unwrap();
        let one = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };
        let two = GrantCtx {
            session_id: "s2".into(),
            grant: live("s2"),
        };

        mcp_call(&one, &mgr, &file, None, "remote", "echo", json!({}))
            .await
            .unwrap();
        mcp_call(&two, &mgr, &file, None, "remote", "echo", json!({}))
            .await
            .unwrap();
        assert_eq!(
            connects.load(Ordering::SeqCst),
            1,
            "the second reuses the first's connection"
        );

        // …and BOTH conversations see it as connected.
        assert_eq!(mgr.statuses(Some("s1")).len(), 1);
        assert_eq!(mgr.statuses(Some("s2")).len(), 1);
        assert_eq!(
            mgr.statuses(None).len(),
            1,
            "one connection, not one per session"
        );

        // Revoking from either conversation closes the shared one — a drop that
        // missed it would leave it serving every other conversation.
        mgr.drop_conn("s2", "remote").await;
        assert!(mgr.statuses(None).is_empty());
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn drop_server_closes_every_sessions_connection_to_one_server() {
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let closed = sink();
        let mgr = manager(
            &file,
            Some(fake_connector(&["echo"], counter(), closed.clone())),
        );
        set_activation(None, "echo", true, None, &opts(&file)).unwrap();
        mgr.call("s1", "echo", "echo", json!({}), &SpawnCtx::new("/w1"))
            .await
            .unwrap();
        mgr.call("s2", "echo", "echo", json!({}), &SpawnCtx::new("/w2"))
            .await
            .unwrap();
        assert_eq!(mgr.statuses(None).len(), 2);

        mgr.drop_server("echo").await;
        let mut names = closed.lock().unwrap().clone();
        names.sort();
        assert_eq!(names, vec!["echo@/w1", "echo@/w2"]);
        assert!(
            mgr.statuses(None).is_empty(),
            "no rows left, live or failed"
        );
    }

    #[tokio::test]
    async fn ensure_reports_one_broken_server_without_taking_the_others_down() {
        let file = tmp_registry();
        save_registry(
            &json!({"servers": {
                "good": {"command": "/bin/true"},
                "bad": {"command": "/bin/false"}
            }}),
            &opts(&file),
        )
        .unwrap();
        let connector: Connector = Arc::new(|spec: ConnectSpec| {
            async move {
                if spec.name == "bad" {
                    return Err(mcp_error(502, "MCP server \"bad\" is broken"));
                }
                Ok(Arc::new(FakeConnection {
                    name: spec.name.clone(),
                    tools: vec!["one".into(), "two".into()],
                    alive: AtomicBool::new(true),
                    label: spec.name.clone(),
                    closed: Arc::new(Mutex::new(Vec::new())),
                }) as Arc<dyn McpConnection>)
            }
            .boxed()
        });
        let mgr = manager(&file, Some(connector));
        let names = ["good".to_string(), "bad".to_string(), "missing".to_string()];
        let catalogs = mgr.ensure("s1", &names, &SpawnCtx::new("/w")).await;
        assert_eq!(
            catalogs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["good", "bad", "missing"]
        );
        assert_eq!(
            catalogs[0]
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        assert!(catalogs[0].error.is_none());
        assert!(catalogs[1].error.as_deref().unwrap().contains("is broken"));
        assert!(catalogs[2]
            .error
            .as_deref()
            .unwrap()
            .contains("not registered"));

        // Both failures are visible in the status surface, named.
        let rows = mgr.statuses(Some("s1"));
        assert_eq!(
            rows.iter()
                .map(|r| (r.server.as_str(), r.state))
                .collect::<Vec<_>>(),
            vec![
                ("bad", McpConnState::Failed),
                ("good", McpConnState::Connected),
                ("missing", McpConnState::Failed),
            ]
        );
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn status_carries_the_live_tool_catalog_and_never_connects_on_its_own() {
        let _live = fixture::live_children_lock().read().await;
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let mgr = manager(&file, None);
        let ctx = GrantCtx {
            session_id: "s1".into(),
            grant: live("s1"),
        };
        set_activation(Some("s1"), "echo", true, None, &opts(&file)).unwrap();
        assert!(
            mgr.statuses(Some("s1")).is_empty(),
            "status never connects on its own"
        );

        mcp_call(
            &ctx,
            &mgr,
            &file,
            None,
            "echo",
            "echo",
            json!({"text": "x"}),
        )
        .await
        .unwrap();
        let rows = mgr.statuses(Some("s1"));
        assert_eq!(rows[0].server, "echo");
        assert_eq!(rows[0].tool_count, 6);
        assert_eq!(
            rows[0].tools,
            vec!["echo", "scream", "boom", "die", "slow", "loose"]
        );
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn concurrent_acquires_share_one_in_flight_connect() {
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let spawned = counter();
        let slow = spawned.clone();
        let connector: Connector = Arc::new(move |spec: ConnectSpec| {
            let spawned = slow.clone();
            async move {
                spawned.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok(Arc::new(FakeConnection {
                    name: spec.name.clone(),
                    tools: vec!["echo".into()],
                    alive: AtomicBool::new(true),
                    label: spec.name.clone(),
                    closed: Arc::new(Mutex::new(Vec::new())),
                }) as Arc<dyn McpConnection>)
            }
            .boxed()
        });
        let mgr = Arc::new(manager(&file, Some(connector)));
        let names = ["echo".to_string()];
        let a = {
            let mgr = mgr.clone();
            let names = names.clone();
            tokio::spawn(async move { mgr.ensure("s1", &names, &SpawnCtx::new("/w")).await })
        };
        let b = {
            let mgr = mgr.clone();
            let names = names.clone();
            tokio::spawn(async move { mgr.ensure("s1", &names, &SpawnCtx::new("/w")).await })
        };
        let (ra, rb) = (a.await.unwrap(), b.await.unwrap());
        assert!(ra[0].error.is_none() && rb[0].error.is_none());
        assert_eq!(
            spawned.load(Ordering::SeqCst),
            1,
            "one spawn, shared by both callers"
        );
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn a_registry_edited_on_disk_is_visible_to_the_very_next_read() {
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        let mgr = manager(&file, Some(fake_connector(&["echo"], counter(), sink())));
        assert_eq!(
            load_registry(&mgr.config())
                .servers
                .keys()
                .collect::<Vec<_>>(),
            vec!["echo"]
        );
        seed_registry(
            &file,
            "echo",
            json!({"linear": {"url": "https://mcp.linear.app/mcp"}}),
        );
        assert_eq!(
            load_registry(&mgr.config())
                .servers
                .keys()
                .collect::<Vec<_>>(),
            vec!["echo", "linear"]
        );
    }

    #[test]
    fn a_failed_tool_carries_the_servers_own_text_and_a_bare_failure_says_so() {
        let with_text = McpCallResult {
            content: Some(vec![crate::mcp::client::McpContentBlock {
                r#type: "text".into(),
                text: Some("kaboom".into()),
            }]),
            is_error: Some(true),
            ..Default::default()
        };
        let e = map_result("echo", "boom", with_text).unwrap_err();
        assert_eq!(e.status(), 502);
        assert!(
            e.to_string().contains("MCP echo:boom failed: kaboom"),
            "{e}"
        );

        let bare = McpCallResult {
            is_error: Some(true),
            ..Default::default()
        };
        let e = map_result("echo", "boom", bare).unwrap_err();
        assert!(
            e.to_string().contains("reported an error with no text"),
            "{e}"
        );

        // Structured content wins over text; text is the fallback.
        let structured = McpCallResult {
            structured_content: Some(json!({"ok": 1})),
            content: Some(vec![crate::mcp::client::McpContentBlock {
                r#type: "text".into(),
                text: Some("ignored".into()),
            }]),
            ..Default::default()
        };
        assert_eq!(map_result("s", "t", structured).unwrap(), json!({"ok": 1}));
    }

    #[tokio::test]
    async fn static_headers_drops_a_dead_reference_only_when_bough_has_its_own_token() {
        use crate::mcp::keychain::{reader_fn, KeychainResult};
        let file = tmp_registry();
        let dir = std::env::temp_dir().join(format!("bough-mcp-tokens-{}", uuid::Uuid::new_v4()));
        let tokens = TokenStoreOptions {
            dir: Some(dir.clone()),
        };
        let absent = KeychainOptions {
            keychain: Some(reader_fn(|_| async { KeychainResult::miss(44, "", None) })),
        };
        let server = ServerConfig {
            url: Some("https://mcp.example.com/mcp".into()),
            headers: [(
                "Authorization".to_string(),
                "Bearer ${keychain:Some Item#a.b}".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        // No tokens of bough's own: the reference IS the intended credential and
        // its failure is the honest answer.
        let e = static_headers("linear", &server, &opts(&file), &absent, &tokens)
            .await
            .unwrap_err();
        assert_eq!(e.status(), 400);

        // With bough's own tokens, the dead reference is stale baggage: dropped,
        // so the OAuth provider can answer.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("linear.json"),
            json!({"tokens": {"access_token": "t", "token_type": "Bearer"}}).to_string(),
        )
        .unwrap();
        assert!(
            static_headers("linear", &server, &opts(&file), &absent, &tokens)
                .await
                .unwrap()
                .is_none()
        );

        // A server with no headers at all asks nothing of any store.
        let bare = ServerConfig {
            url: server.url.clone(),
            ..Default::default()
        };
        assert!(
            static_headers("linear", &bare, &opts(&file), &absent, &tokens)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_unreachable_remote_entry_is_a_named_failure_row_not_a_hang() {
        // The default connector's remote arm is real (`remote.rs`); what must
        // not change is that a remote server bough cannot reach is a NAMED
        // status row, bounded, and never a hang or a silent absence.
        let file = tmp_registry();
        save_registry(
            &json!({"servers": {"linear": {"url": "http://127.0.0.1:1/mcp"}}}),
            &opts(&file),
        )
        .unwrap();
        let mgr = McpManager::new(McpManagerOptions {
            config: Some(opts(&file)),
            timeouts: Some(McpTimeouts {
                connect_ms: Some(1_000),
                request_ms: Some(1_000),
                call_ms: Some(1_000),
            }),
            ..Default::default()
        });
        let catalogs = mgr
            .ensure("s1", &["linear".to_string()], &SpawnCtx::new("."))
            .await;
        let error = catalogs[0].error.as_deref().unwrap_or("");
        assert!(error.contains("linear"), "{error}");
        assert_eq!(mgr.statuses(Some("s1"))[0].state, McpConnState::Failed);
    }

    /// The process manager is ONE object and the tests below swap it, so they
    /// run one at a time.
    fn singleton_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[tokio::test]
    async fn a_subagent_inherits_its_spawners_grant_and_a_later_revoke_leaves_it_armed() {
        // THE ACCEPTANCE CRITERION, driven through the real `launch_subagent`
        // against a real database and the real turn runner: the child's own ctx
        // is captured and its grant read from it. A child that resolved its own
        // grant would resolve to NOTHING (it has no activations), so the
        // assertion that would pass a broken implementation is "the child's
        // grant is empty"; this asserts the opposite.
        use crate::agents::subagent::{launch_subagent, LaunchDeps, SubagentOptions};
        use crate::agents::testkit::{
            recording_llm, seed_spawner, spawner_turn_ctx, AgentsFixture,
        };
        use crate::turn::runner::TurnDeps;
        use crate::types::TurnCtx;

        let _guard = singleton_lock().lock().await;
        let file = tmp_registry();
        seed_registry(&file, "echo", json!({}));
        // The subagent takes its snapshot through the PROCESS manager's config,
        // which is what boot points at `~/.bough/mcp.json`.
        let previous = set_mcp_manager(Arc::new(manager(&file, None)));

        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        // The human granted the SPAWNER, and only the spawner.
        set_activation(Some(&seeded.session.id), "echo", true, None, &opts(&file)).unwrap();
        let mut ctx = spawner_turn_ctx(&f, &seeded, recording_llm("done"));
        bind_turn_grant(&mut ctx);

        let captured: Arc<Mutex<Option<TurnCtx>>> = Arc::new(Mutex::new(None));
        let sink = captured.clone();
        let mut turn = TurnDeps {
            registry: Some(f.registry.clone()),
            ..crate::turn::testkit::stub_deps()
        };
        turn.program = None;
        turn.program_for = Some(Arc::new(move |c: &TurnCtx| {
            *sink.lock().unwrap() = Some(c.clone());
            crate::turn::testkit::ok_program()
        }));
        let launch = launch_subagent(
            &ctx,
            "Ask the echo server to say hello.",
            &SubagentOptions::default(),
            &LaunchDeps {
                turn: Some(turn),
                ..Default::default()
            },
        )
        .unwrap();
        launch.result.clone().await;

        let child_ctx = captured
            .lock()
            .unwrap()
            .clone()
            .expect("the child's turn ran");
        // The child is a session of its own with NO activations — the grant it
        // holds can only have come from its spawner.
        assert!(
            resolve_grant(&GrantCtx::for_session(&child_ctx.session_id), &opts(&file)).is_empty()
        );
        assert_eq!(
            child_ctx.mcp_grant,
            Some(McpGrant::Inherited(vec!["echo".to_string()])),
            "the spawner's grant crossed the boundary, as a snapshot"
        );
        let child_grant = GrantCtx::from_turn(&child_ctx);
        assert!(require_granted(&child_grant, "echo", &opts(&file)).is_ok());

        // The grant is a SNAPSHOT taken at spawn: revoking the spawner's grant
        // now does not disarm a child already running on the human's
        // authorization…
        set_activation(Some(&seeded.session.id), "echo", false, None, &opts(&file)).unwrap();
        assert_eq!(resolve_grant(&child_grant, &opts(&file)), vec!["echo"]);
        // …while the spawner itself is refused on its very next call.
        let spawner_grant = GrantCtx::from_turn(&ctx);
        assert!(resolve_grant(&spawner_grant, &opts(&file)).is_empty());
        let refused = require_granted(&spawner_grant, "echo", &opts(&file)).unwrap_err();
        assert_eq!(refused.status(), 403);
        // …and the two are told different things, because the moves differ.
        assert!(
            refused.to_string().contains("A human grants one"),
            "{refused}"
        );
        let child_refused = require_granted(
            &GrantCtx {
                grant: Some(McpGrant::Inherited(vec![])),
                ..child_grant
            },
            "echo",
            &opts(&file),
        )
        .unwrap_err();
        assert!(
            child_refused.to_string().contains("cannot widen it"),
            "{child_refused}"
        );

        set_mcp_manager(previous);
    }

    #[tokio::test]
    async fn the_process_manager_is_one_manager_and_a_swap_returns_the_previous() {
        let _guard = singleton_lock().lock().await;
        let first = mcp_manager();
        let file = tmp_registry();
        let next = Arc::new(manager(
            &file,
            Some(fake_connector(&["echo"], counter(), sink())),
        ));
        let previous = set_mcp_manager(next.clone());
        assert!(Arc::ptr_eq(&previous, &first));
        assert!(Arc::ptr_eq(&mcp_manager(), &next));
        set_mcp_manager(previous);
    }
}
