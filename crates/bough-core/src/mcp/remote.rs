//! Remote MCP servers — registry entries with a `url` — over the Streamable HTTP
//! transport, authenticated by the OAuth provider in `oauth.rs` (port of
//! `src/mcp/remote.ts`).
//!
//! THE INVARIANT THIS HOLDS is the same one the stdio client holds, with one
//! addition that is the whole point of this file: **a server that does not work
//! fails, by name, in bounded time — and a server that is merely UNAUTHORIZED fails
//! as a question, not as a fault.** A 401 becomes "not authorized — open the mcp
//! panel (^p) and press a on <name>" in the turn's catalog. Never a hang, never a
//! stack trace, never an entry that reads as "this server is broken" when the truth
//! is "nobody has approved it yet".
//!
//! Three properties make that true, and each one is load-bearing:
//!
//! **Every HTTP request is bounded.** Every request carries a per-request deadline
//! plus a connection-wide cancellation that `close()` fires.
//!
//! **A 401 is remembered even when the auth flow fails afterwards.** The transport
//! answers a 401 by running the OAuth flow, and if THAT fails — no OAuth metadata,
//! no registration endpoint, a rejected refresh token — the error that escapes is
//! about discovery or registration and no longer mentions 401 at all. A server whose
//! only problem is that nobody has authorized it would then surface as broken. The
//! fetch wrapper records a 401 from the MCP endpoint, and the error mapper trusts
//! that over the shape of whatever escaped.
//!
//! **Refresh is the transport's job, not ours.** An expired access token is not an
//! error path: the request gets a 401, the flow exchanges the refresh token, the
//! provider persists the new pair, and the request is retried — all inside one
//! `call_tool`. An expired REFRESH token degrades to the same authorization prompt
//! as a server that was never authorized, which is exactly right: in both cases the
//! human must approve access again.
//!
//! **The JSON-RPC channel goes DIRECT.** No egress proxy, no call-layer gate, no
//! sandbox. Stated because the previous tree argued the point at length.
//!
//! PORT NOTES. (1) There is no Rust MCP SDK in this workspace, so the transport is
//! hand-rolled on `reqwest`: POST the JSON-RPC message, accept either an
//! `application/json` body or a `text/event-stream` one and read the response frame
//! out of it, carry `Mcp-Session-Id` and `MCP-Protocol-Version`, and DELETE the
//! session on close. (2) The standalone server→client SSE `GET` subscription is not
//! opened: bough issues requests and reads their responses, and nothing above this
//! layer consumes a server-initiated message. That is also why the TS module's
//! "the SSE stream carries only the connection abort" carve-out has no analogue
//! here — there is no unbounded request to carve out.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::errors::{BoughError, ErrorKind};
use crate::mcp::client::{
    McpCallResult, McpConnection, McpContentBlock, McpServerInfo, McpTimeouts, McpToolInfo,
    McpToolSchema, LATEST_PROTOCOL_VERSION,
};
use crate::mcp::keychain::{claude_code_prefill, KeychainOptions};
use crate::mcp::oauth::{
    flow, origin_of, BoughOAuthProvider, FetchFn, HttpReq, HttpRes, ProviderOptions,
};

fn mcp(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Mcp, message)
}

/// `tools/list` pages followed before giving up — a self-referential cursor is a hang.
const MAX_TOOL_PAGES: usize = 50;

/// Transport diagnostics kept for `/mcp` status ("why is it failing").
const ERROR_TAIL_BYTES: usize = 4_096;

/// The excerpt of that tail an error message carries.
const ERROR_NOTE_BYTES: usize = 500;

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
            // Longer on purpose: browser automation and long fetches are legitimate.
            call_ms: t.call_ms.unwrap_or(5 * 60_000),
        }
    }
}

// ---------------------------------------------------------------------------
// The authorization prompt
// ---------------------------------------------------------------------------

/// The sentence a 401 turns into. Exported so the catalog, the `/mcp` panel and the
/// error all say the same words — a prompt the user recognizes in one place and not
/// in another is a prompt they do not act on.
///
/// NAMES A GESTURE THAT EXISTS. This said "/mcp auth <name>" for as long as it had
/// existed, and there has never been a `/mcp` slash command in this client — `/`
/// lists skills, and MCP is a PANEL. `^p` opens the tab, `a` starts the flow.
pub fn auth_prompt(server: &str) -> String {
    format!("not authorized — open the mcp panel (^p) and press a on {server}")
}

/// The server answered 401 and bough has no usable credentials for it.
///
/// PORT NOTE. In TS this is a distinct `McpAuthRequiredError` class carrying an
/// `authRequired = true` discriminator, because a class identity does not survive
/// the boundaries a JS error crosses. In Rust the discriminator is structural and
/// exact: status 401 on an `McpError`. Nothing else in this subsystem raises 401.
pub fn auth_required_error(server: &str, detail: Option<&str>) -> BoughError {
    let tail = match detail {
        Some(d) if !d.is_empty() => format!(" ({d})"),
        _ => String::new(),
    };
    mcp(
        401,
        format!(
            "MCP server \"{server}\": {}. The server answered 401 and bough has no token it \
             can use or refresh for it; that command returns the URL to approve access in \
             your browser, and the tools appear on the next turn. The rest of the turn is \
             unaffected.{tail}",
            auth_prompt(server)
        ),
    )
}

/// True when the failure is "a human must approve access", not "it is broken".
pub fn is_auth_required(error: &BoughError) -> bool {
    matches!(
        error,
        BoughError::Http {
            status: 401,
            kind: ErrorKind::Mcp,
            ..
        }
    )
}

// ---------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------

/// A bearer to present before any authorization flow runs.
#[derive(Debug, Clone, Default)]
pub enum Prefill {
    /// Ask `keychain.rs`: a covered host gets the machine's existing credential,
    /// anything else gets nothing.
    #[default]
    Ask,
    /// Do not prefill at all — what a test wants when it asserts the unauthorized
    /// path.
    None,
    Token(String),
}

/// Whether this connection authenticates, and with what.
#[derive(Clone, Default)]
pub enum RemoteAuth {
    /// A [`BoughOAuthProvider`] over the token store — what production wants.
    #[default]
    Default,
    /// Talk to a server that needs no auth at all.
    None,
    /// A caller-owned provider, so it can read back the captured authorization URL.
    Provider(Arc<BoughOAuthProvider>),
}

#[derive(Clone, Default)]
pub struct RemoteConnectOptions {
    /// The registry name. Every error message carries it, and it keys the tokens.
    pub name: String,
    /// The Streamable HTTP endpoint (`ServerConfig.url`).
    pub url: String,
    /// Static headers from the registry, already expanded. OAuth is NOT one of these.
    pub headers: BTreeMap<String, String>,
    pub timeouts: Option<McpTimeouts>,
    pub auth: RemoteAuth,
    /// Where token files live. Absent = `~/.bough/mcp/tokens`.
    pub dir: Option<std::path::PathBuf>,
    /// HTTP, injected in tests. Absent = `reqwest`.
    pub fetch: Option<FetchFn>,
    pub prefill: Prefill,
    /// The credential store the prefill is read from. Injected in tests.
    pub keychain: KeychainOptions,
}

/// One connected remote server.
///
/// Reads as the stdio client's twin on purpose: same [`McpConnection`] surface, same
/// bounded pagination, same "a tool error is DATA, not an exception" rule, same
/// lenient result reading. The differences are all in how it fails, which is the
/// half this module exists for.
pub struct McpRemoteClient {
    name: String,
    url: String,
    headers: BTreeMap<String, String>,
    auth: Option<Arc<BoughOAuthProvider>>,
    timeouts: Deadlines,
    fetch: FetchFn,
    /// Fired by `close()`: an in-flight request must not outlive its connection.
    abort: tokio_util::sync::CancellationToken,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<Option<String>>,
    next_id: AtomicU64,
    alive: AtomicBool,
    closed: AtomicBool,
    error_tail: Mutex<String>,
    /// See the module comment: a 401 seen from the MCP endpoint itself outranks the
    /// shape of whatever the auth flow failed with afterwards.
    saw_unauthorized: AtomicBool,
    info: Mutex<Option<McpServerInfo>>,
}

impl McpRemoteClient {
    /// Connect and run the MCP handshake.
    ///
    /// Rejects — never hangs, never resolves half-connected — on an unreachable
    /// host, a server that accepts the connection and never answers, an HTTP error,
    /// or an authorization the human has not granted. The last one rejects with a
    /// 401 [`auth_required_error`] so the caller can render a prompt, not a fault.
    pub async fn connect(opts: RemoteConnectOptions) -> Result<Self, BoughError> {
        let name = opts.name.clone();
        let timeouts: Deadlines = opts.timeouts.into();
        let absolute_http = origin_of(&opts.url)
            .is_some_and(|o| o.starts_with("http://") || o.starts_with("https://"));
        if !absolute_http {
            return Err(mcp(
                400,
                format!(
                    "MCP server \"{name}\" has an unusable `url` ({}). A remote server needs \
                     an absolute http(s) URL pointing at its MCP endpoint.",
                    serde_json::to_string(&opts.url).unwrap_or_default()
                ),
            ));
        }

        // PREFILL, resolved before the provider is built because the read is a
        // subprocess and the provider's `tokens()` is not async. Confined to hosts the
        // credential belongs to and silent when there is none, so a server with its
        // own OAuth flow is unaffected — and a token bough's own flow stored always
        // wins over this.
        let prefill = match &opts.prefill {
            Prefill::None => None,
            Prefill::Token(t) => Some(t.clone()),
            Prefill::Ask => claude_code_prefill(&opts.url, &opts.keychain).await,
        };
        let auth = match &opts.auth {
            RemoteAuth::None => None,
            RemoteAuth::Provider(p) => Some(p.clone()),
            RemoteAuth::Default => Some(Arc::new(BoughOAuthProvider::new(
                &name,
                &ProviderOptions {
                    dir: opts.dir.clone(),
                    prefill,
                    ..Default::default()
                },
            )?)),
        };

        let client = Self {
            name: name.clone(),
            url: opts.url.clone(),
            headers: opts.headers.clone(),
            auth,
            timeouts,
            fetch: opts.fetch.clone().unwrap_or_else(default_fetch),
            abort: tokio_util::sync::CancellationToken::new(),
            session_id: Mutex::new(None),
            protocol_version: Mutex::new(None),
            next_id: AtomicU64::new(0),
            alive: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            error_tail: Mutex::new(String::new()),
            saw_unauthorized: AtomicBool::new(false),
            info: Mutex::new(None),
        };

        // The connect deadline covers the whole handshake including any auth round
        // trips it triggers; each individual request is bounded under it. Both, so
        // neither a slow server nor a slow authorization server can park a turn.
        let result = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "bough", "version": "0" },
                }),
                timeouts.connect_ms,
                "connect",
            )
            .await;
        let result = match result {
            Ok(v) => v,
            Err(e) => {
                client.close().await;
                return Err(e);
            }
        };

        let protocol_version = result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(LATEST_PROTOCOL_VERSION)
            .to_string();
        *client.protocol_version.lock().unwrap() = Some(protocol_version.clone());
        *client.info.lock().unwrap() = Some(McpServerInfo {
            name: result
                .get("serverInfo")
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            version: result
                .get("serverInfo")
                .and_then(|s| s.get("version"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            protocol_version,
        });
        // The handshake is only complete once the server has been told so.
        client.notify("notifications/initialized").await;
        Ok(client)
    }

    /// Identity from the handshake, for `/mcp` status.
    pub fn server_info(&self) -> Option<McpServerInfo> {
        self.info.lock().unwrap().clone()
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    // -- JSON-RPC ------------------------------------------------------------

    /// One request, with the 401 → authorize → retry-once loop around it.
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout_ms: u64,
        what: &str,
    ) -> Result<Value, BoughError> {
        if !self.alive() {
            return Err(mcp(
                502,
                format!(
                    "MCP server \"{}\" is disconnected, so {what} could not be sent{}. \
                     Reconnect it (POST /mcp/servers/{}/enable).",
                    self.name,
                    self.note(),
                    self.name
                ),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });

        let first = self.post(&body, timeout_ms, what).await?;
        let res = if first.status == 401 {
            // Refresh is the transport's job: run the flow, then present the new
            // token once. A flow that ends in REDIRECT (or fails) is the prompt.
            self.saw_unauthorized.store(true, Ordering::SeqCst);
            match self.authorize().await {
                Ok(true) => self.post(&body, timeout_ms, what).await?,
                Ok(false) => return Err(auth_required_error(&self.name, None)),
                Err(detail) => return Err(auth_required_error(&self.name, Some(&detail))),
            }
        } else {
            first
        };

        if res.status == 401 {
            return Err(auth_required_error(&self.name, None));
        }
        if res.status < 200 || res.status >= 300 {
            return Err(self.transport_failure(what, &format!("HTTP {}", res.status)));
        }
        if let Some(sid) = res.header("mcp-session-id") {
            *self.session_id.lock().unwrap() = Some(sid.to_string());
        }
        let frame = self.frame_for(&res, id, what)?;
        if let Some(error) = frame.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("the server reported an error");
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            // -32001 is the SDK's RequestTimeout — the server accepted and stalled.
            if code == -32001 {
                return Err(self.timed_out(what));
            }
            return Err(self.transport_failure(what, message));
        }
        Ok(frame.get("result").cloned().unwrap_or(Value::Null))
    }

    /// A notification: no id, no answer, and a failure is not worth a turn.
    async fn notify(&self, method: &str) {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": {} });
        let _ = self.post(&body, self.timeouts.request_ms, method).await;
    }

    /// The JSON-RPC response frame, out of either an `application/json` body or the
    /// `text/event-stream` one a server in SSE-response mode answers with.
    fn frame_for(&self, res: &HttpRes, id: u64, what: &str) -> Result<Value, BoughError> {
        let content_type = res
            .header("content-type")
            .unwrap_or("")
            .to_ascii_lowercase();
        let candidates: Vec<Value> = if content_type.contains("text/event-stream") {
            sse_data_frames(&res.body)
        } else {
            serde_json::from_str::<Value>(&res.body)
                .into_iter()
                .collect()
        };
        for frame in candidates {
            // A batched answer is a list; a single one is an object.
            let items: Vec<Value> = match frame {
                Value::Array(items) => items,
                other => vec![other],
            };
            for item in items {
                if item.get("id").and_then(|v| v.as_u64()) == Some(id) {
                    return Ok(item);
                }
            }
        }
        Err(self.transport_failure(
            what,
            &format!(
                "the response carried no JSON-RPC answer for request {id} ({})",
                truncate(&res.body, 200)
            ),
        ))
    }

    /// POST one message. Returns the raw response so the caller can see a 401.
    async fn post(&self, body: &Value, timeout_ms: u64, what: &str) -> Result<HttpRes, BoughError> {
        let mut headers: Vec<(String, String)> = vec![
            ("content-type".into(), "application/json".into()),
            (
                "accept".into(),
                "application/json, text/event-stream".into(),
            ),
        ];
        // Static registry headers first: an OAuth token, when there is one, is the
        // more specific statement and overrides them.
        for (k, v) in &self.headers {
            headers.push((k.to_ascii_lowercase(), v.clone()));
        }
        if let Some(sid) = self.session_id.lock().unwrap().clone() {
            headers.push(("mcp-session-id".into(), sid));
        }
        if let Some(pv) = self.protocol_version.lock().unwrap().clone() {
            headers.push(("mcp-protocol-version".into(), pv));
        }
        if let Some(token) = self.bearer() {
            headers.retain(|(k, _)| k != "authorization");
            headers.push(("authorization".into(), format!("Bearer {token}")));
        }

        // BOUNDED HERE, around whatever fetch was injected, so the deadline holds
        // for the production client and a test double alike. The abort is the
        // connection-wide kill switch `close()` fires.
        let call = (self.fetch)(HttpReq {
            method: "POST".into(),
            url: self.url.clone(),
            headers,
            body: Some(body.to_string()),
        });
        let res = tokio::select! {
            _ = self.abort.cancelled() => Err("the connection was closed".to_string()),
            r = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), call) => {
                r.unwrap_or_else(|_| Err(format!("timed out after {timeout_ms}ms")))
            }
        };
        match res {
            Ok(res) => {
                // Only the MCP endpoint itself: a 401 from the token endpoint is the
                // auth flow's own business.
                if res.status == 401 {
                    self.saw_unauthorized.store(true, Ordering::SeqCst);
                }
                Ok(res)
            }
            Err(detail) => {
                // FORMAT FIRST, THEN RECORD. The tail is the "what went wrong
                // BEFORE this" diagnostic that `note()` appends, and TS fills it
                // only from the SDK's `onerror` callback — never from the error
                // it is in the middle of reporting. Recording first put the same
                // sentence in twice: `failed connect: <detail> — last transport
                // error: <detail>`. The tail still gets it, so the NEXT failure
                // can cite this one.
                let err = if is_timeout(&detail) {
                    self.timed_out(what)
                } else {
                    self.transport_failure(what, &detail)
                };
                self.record_error(&detail);
                Err(err)
            }
        }
    }

    fn bearer(&self) -> Option<String> {
        let provider = self.auth.as_ref()?;
        let tokens = provider.tokens().ok()??;
        (!tokens.access_token.is_empty()).then_some(tokens.access_token)
    }

    /// Run the OAuth flow after a 401. `Ok(true)` = try the request again;
    /// `Ok(false)` = the human must approve access; `Err` = the flow itself failed,
    /// and its reason becomes the prompt's parenthetical.
    async fn authorize(&self) -> Result<bool, String> {
        let Some(provider) = self.auth.as_ref() else {
            return Ok(false);
        };
        match flow::auth(provider, &self.url, None, &self.fetch).await {
            Ok(flow::AuthResult::Authorized) => Ok(true),
            Ok(flow::AuthResult::Redirect) => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    // -- failure shapes ------------------------------------------------------

    fn timed_out(&self, what: &str) -> BoughError {
        mcp(
            504,
            format!(
                "MCP {what} on server \"{}\" timed out — {} accepted the connection but did \
                 not answer{}. The server is up and stuck, or the URL is not an MCP endpoint.",
                self.name,
                self.url,
                self.note()
            ),
        )
    }

    fn transport_failure(&self, what: &str, detail: &str) -> BoughError {
        mcp(
            502,
            format!(
                "MCP server \"{}\" failed {what}: {detail}{}. Check `url` in the registry \
                 (GET /mcp/servers) and that {} is reachable.",
                self.name,
                self.note(),
                self.url
            ),
        )
    }

    fn record_error(&self, detail: &str) {
        let mut tail = self.error_tail.lock().unwrap();
        tail.push('\n');
        tail.push_str(detail);
        if tail.len() > ERROR_TAIL_BYTES {
            let cut = tail.len() - ERROR_TAIL_BYTES;
            let cut = floor_char_boundary(&tail, cut);
            *tail = tail[cut..].to_string();
        }
    }

    fn note(&self) -> String {
        let tail = self.stderr_tail();
        if tail.is_empty() {
            return String::new();
        }
        let cut = tail.len().saturating_sub(ERROR_NOTE_BYTES);
        let cut = floor_char_boundary(&tail, cut);
        format!(" — last transport error: {}", &tail[cut..])
    }
}

#[async_trait]
impl McpConnection for McpRemoteClient {
    fn name(&self) -> &str {
        &self.name
    }

    /// Every tool the server advertises, following pagination cursors.
    ///
    /// Bounded twice, exactly like the stdio client: a repeated cursor and a runaway
    /// page count are both errors, because either one inside a turn is a hang with
    /// extra steps.
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, BoughError> {
        let mut tools: Vec<McpToolInfo> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self
                .request("tools/list", params, self.timeouts.request_ms, "tools/list")
                .await?;
            if let Some(Value::Array(raw)) = result.get("tools") {
                for entry in raw {
                    if let Some(tool) = tool_info(entry) {
                        tools.push(tool);
                    }
                }
            }
            let next = result
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let Some(next) = next.filter(|s| !s.is_empty()) else {
                return Ok(tools);
            };
            if seen.contains(&next) {
                return Err(mcp(
                    502,
                    format!(
                        "MCP server \"{}\" repeated the tools/list cursor {}, so its tool list \
                         never ends. Reporting {} tools and stopping.",
                        self.name,
                        serde_json::to_string(&next).unwrap_or_default(),
                        tools.len()
                    ),
                ));
            }
            seen.push(next.clone());
            cursor = Some(next);
        }
        Err(mcp(
            502,
            format!(
                "MCP server \"{}\" paginated tools/list past {MAX_TOOL_PAGES} pages. Reporting \
                 {} tools and stopping.",
                self.name,
                tools.len()
            ),
        ))
    }

    /// Invoke one tool. A tool that FAILS comes back as `{isError: true}` — that is
    /// data the program reads, not an exception. Only transport, protocol, deadline
    /// and authorization failures throw.
    ///
    /// An access token that expired since `connect` is invisible here: the transport
    /// refreshes it and retries inside this call.
    async fn call_tool(&self, name: &str, args: Value) -> Result<McpCallResult, BoughError> {
        let args = if args.is_null() { json!({}) } else { args };
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": args }),
                self.timeouts.call_ms,
                &format!("tools/call {name}"),
            )
            .await?;
        Ok(call_result(&result))
    }

    /// Close the session and cancel anything in flight. Safe to call twice, never
    /// fails — teardown that can fail is teardown that leaks a connection.
    async fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.alive.store(false, Ordering::SeqCst);
        // Best effort: an unterminated session is the server's to reap, and a
        // failure here must never reach a caller that is tearing down.
        let sid = self.session_id.lock().unwrap().clone();
        if let Some(sid) = sid {
            let _ = (self.fetch)(HttpReq {
                method: "DELETE".into(),
                url: self.url.clone(),
                headers: vec![("mcp-session-id".into(), sid)],
                body: None,
            })
            .await;
        }
        // After the graceful close, so a request in flight is torn down rather than
        // left to finish against a session that no longer exists.
        self.abort.cancel();
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst) && !self.closed.load(Ordering::SeqCst)
    }

    /// Recent transport errors — the remote analogue of stdio's stderr tail.
    fn stderr_tail(&self) -> String {
        self.error_tail.lock().unwrap().trim().to_string()
    }
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// The default transport: `reqwest`, one bounded request.
fn default_fetch() -> FetchFn {
    Arc::new(|req: HttpReq| {
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .map_err(|e| e.to_string())?;
            let method =
                reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|e| e.to_string())?;
            let mut b = client.request(method, &req.url);
            for (k, v) in &req.headers {
                b = b.header(k, v);
            }
            if let Some(body) = req.body {
                b = b.body(body);
            }
            let res = b.send().await.map_err(|e| e.to_string())?;
            let status = res.status().as_u16();
            let headers = res
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_ascii_lowercase(),
                        v.to_str().unwrap_or("").to_string(),
                    )
                })
                .collect();
            let body = res.text().await.map_err(|e| e.to_string())?;
            Ok(HttpRes {
                status,
                headers,
                body,
            })
        }) as futures::future::BoxFuture<'static, Result<HttpRes, String>>
    })
}

fn is_timeout(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("timed out") || d.contains("timeout") || d.contains("operation timed out")
}

/// Every `data:` payload in an SSE body, parsed as JSON. Multi-line `data:` fields
/// are joined with a newline, per the EventSource grammar.
fn sse_data_frames(body: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut data = String::new();
    let mut flush = |data: &mut String| {
        if !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                out.push(v);
            }
            data.clear();
        }
    };
    for line in body.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            flush(&mut data);
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    flush(&mut data);
    out
}

/// One advertised tool. `None` means the entry could not be called even in
/// principle, so it is dropped rather than printed as a tool the model may try.
///
/// LENIENT BY DESIGN, same rule as the stdio client: dropping a callable tool from
/// the catalog over a schema nit — an `inputSchema` missing `type: "object"` is the
/// common one — is a worse outcome than a thin signature.
fn tool_info(raw: &Value) -> Option<McpToolInfo> {
    let name = raw
        .get("name")?
        .as_str()
        .filter(|s| !s.is_empty())?
        .to_string();
    let schema = raw
        .get("inputSchema")
        .and_then(|s| s.as_object())
        .map(|s| McpToolSchema {
            properties: s.get("properties").and_then(|p| p.as_object()).cloned(),
            required: s.get("required").and_then(|r| r.as_array()).map(|r| {
                r.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            }),
        });
    Some(McpToolInfo {
        name,
        description: raw
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string()),
        input_schema: schema,
        annotations: raw.get("annotations").and_then(|a| a.as_object()).cloned(),
    })
}

/// A `tools/call` result, read leniently. An unrecognized shape becomes
/// `structuredContent`, never a failure.
fn call_result(raw: &Value) -> McpCallResult {
    let content = raw.get("content").and_then(|c| c.as_array()).map(|blocks| {
        blocks
            .iter()
            .filter_map(|b| {
                Some(McpContentBlock {
                    r#type: b.get("type")?.as_str()?.to_string(),
                    text: b
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string()),
                })
            })
            .collect::<Vec<_>>()
    });
    let structured = raw.get("structuredContent").cloned();
    let is_error = raw.get("isError").and_then(|e| e.as_bool());
    if content.is_none() && structured.is_none() && is_error.is_none() {
        let known: Map<String, Value> = Map::new();
        let _ = known;
        return McpCallResult {
            content: None,
            structured_content: Some(raw.clone()),
            is_error: None,
        };
    }
    McpCallResult {
        content,
        structured_content: structured,
        is_error,
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let cut = floor_char_boundary(s, n);
    format!("{}…", &s[..cut])
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    //! Driven against a real loopback fixture that speaks real JSON-RPC and a real
    //! OAuth 2.1 flow — RFC 9728 discovery, dynamic client registration, PKCE,
    //! refresh grants. The happy path is here, but the tests that matter are the
    //! failure ones, because this module exists for how it fails:
    //!
    //!   - a 401 becomes an AUTHORIZATION PROMPT carried in the catalog entry as a
    //!     prompt, not as a fault and not as a hang;
    //!   - an EXPIRED REFRESH TOKEN degrades to exactly the same prompt, because the
    //!     human's move is the same;
    //!   - a refresh that CAN succeed is invisible: the transport swaps the token
    //!     mid-request and the caller sees a working connection;
    //!   - a server that accepts a connection and never answers fails on a deadline.
    //!
    //! Every deadline here is in the hundreds of milliseconds, so a regression that
    //! reintroduces a hang shows up as a failing test rather than as a suite that
    //! never finishes. Hermetic: loopback only, no real `~/.bough`, no outbound
    //! network.

    use super::*;
    use crate::mcp::oauth::{ClientInfo, OAuthTokens, Stored, TokenStore, TokenStoreOptions};
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ---- the fixture ------------------------------------------------------

    #[derive(Clone, Default)]
    struct FixtureOptions {
        /// Bearer the MCP endpoint accepts. Absent = the endpoint needs no auth.
        accept: Option<String>,
        /// Refresh tokens the token endpoint honors, and what each mints.
        refresh: Vec<(String, (String, Option<String>))>,
        /// Accept the POST and never answer it — the hang this module must not have.
        stall: bool,
        /// Answer every POST 401 with no OAuth metadata anywhere: the flow fails
        /// AFTER the 401, and the prompt must survive that.
        bare_401: bool,
        /// Answer the JSON-RPC response as `text/event-stream` instead of JSON.
        sse_mode: bool,
    }

    #[derive(Default, Debug)]
    struct Seen {
        /// Every `authorization` header the MCP endpoint received, in order.
        bearers: Vec<Option<String>>,
        /// Every grant_type the token endpoint was asked for, in order.
        grants: Vec<String>,
        /// How many times a client dynamically registered.
        registrations: usize,
        /// Bodies posted to /register, so the client metadata can be asserted.
        registered: Vec<Value>,
    }

    struct Fixture {
        url: String,
        seen: Arc<Mutex<Seen>>,
        shutdown: tokio_util::sync::CancellationToken,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.shutdown.cancel();
        }
    }

    /// One loopback server playing three roles at once: the MCP resource server, its
    /// RFC 9728 metadata, and the authorization server. Real HTTP end to end — the
    /// point is to exercise the transport's own auth handling, which a mocked fetch
    /// would step over.
    async fn start_fixture(opts: FixtureOptions) -> Fixture {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shutdown = tokio_util::sync::CancellationToken::new();

        let task_base = base.clone();
        let task_seen = seen.clone();
        let task_shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = task_shutdown.cancelled() => break,
                    a = listener.accept() => a,
                };
                let Ok((stream, _)) = accepted else { break };
                let base = task_base.clone();
                let seen = task_seen.clone();
                let opts = opts.clone();
                let shutdown = task_shutdown.clone();
                tokio::spawn(async move {
                    let _ = serve_one(stream, base, seen, opts, shutdown).await;
                });
            }
        });
        Fixture {
            url: format!("{base}/mcp"),
            seen,
            shutdown,
        }
    }

    /// A minimal HTTP/1.1 server: request line, headers, `Content-Length` body, one
    /// response, close. Enough for the three roles above and nothing more.
    async fn serve_one(
        mut stream: tokio::net::TcpStream,
        base: String,
        seen: Arc<Mutex<Seen>>,
        opts: FixtureOptions,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        // Headers first.
        let head_end = loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = find(&buf, b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split(' ');
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let path = target.split('?').next().unwrap_or("/").to_string();
        let mut headers: HashMap<String, String> = HashMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let len: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = buf[head_end..].to_vec();
        while body.len() < len {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&body).to_string();

        let json = |v: Value, status: u16| -> (u16, String, String) {
            (status, "application/json".to_string(), v.to_string())
        };

        let (status, content_type, out) =
            if path.starts_with("/.well-known/oauth-protected-resource") && !opts.bare_401 {
                json(
                    json!({ "resource": format!("{base}/mcp"), "authorization_servers": [base] }),
                    200,
                )
            } else if path.starts_with("/.well-known/") && !opts.bare_401 {
                json(
                    json!({
                        "issuer": base,
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "registration_endpoint": format!("{base}/register"),
                        "response_types_supported": ["code"],
                        "grant_types_supported": ["authorization_code", "refresh_token"],
                        "code_challenge_methods_supported": ["S256"],
                        "token_endpoint_auth_methods_supported": ["none"],
                    }),
                    200,
                )
            } else if path == "/register" && method == "POST" {
                let metadata: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                {
                    let mut s = seen.lock().unwrap();
                    s.registrations += 1;
                    s.registered.push(metadata.clone());
                }
                json(
                    json!({
                        "client_id": "dyn-client",
                        "redirect_uris": metadata["redirect_uris"],
                        "token_endpoint_auth_method": "none",
                    }),
                    201,
                )
            } else if path == "/token" && method == "POST" {
                let form = crate::mcp::oauth::parse_query(&body);
                let grant = form.get("grant_type").cloned().unwrap_or_default();
                seen.lock().unwrap().grants.push(grant.clone());
                let minted = if grant == "refresh_token" {
                    let r = form.get("refresh_token").cloned().unwrap_or_default();
                    opts.refresh
                        .iter()
                        .find(|(k, _)| *k == r)
                        .map(|(_, v)| v.clone())
                } else {
                    None
                };
                match minted {
                    None => json(json!({ "error": "invalid_grant" }), 400),
                    Some((access, refresh)) => {
                        let mut doc = json!({
                            "token_type": "Bearer", "expires_in": 3600, "access_token": access,
                        });
                        if let Some(r) = refresh {
                            doc["refresh_token"] = json!(r);
                        }
                        json(doc, 200)
                    }
                }
            } else if path != "/mcp" {
                (404, "text/plain".to_string(), "not found".to_string())
            } else if method != "POST" {
                // The transport does not open the standalone SSE GET; anything else is
                // the session DELETE, which this fixture just acknowledges.
                (
                    if method == "DELETE" { 200 } else { 405 },
                    "text/plain".to_string(),
                    String::new(),
                )
            } else {
                let authorization = headers.get("authorization").cloned();
                seen.lock().unwrap().bearers.push(authorization.clone());
                if opts.bare_401 {
                    (401, "text/plain".to_string(), "nope".to_string())
                } else if opts
                    .accept
                    .as_ref()
                    .is_some_and(|a| authorization.as_deref() != Some(&format!("Bearer {a}")))
                {
                    (
                        401,
                        "application/json".to_string(),
                        json!({ "error": "invalid_token" }).to_string(),
                    )
                } else if opts.stall {
                    // Accepted and never answered. Exactly the shape of a hang.
                    shutdown.cancelled().await;
                    return Ok(());
                } else {
                    let msg: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    match msg.get("id").and_then(|v| v.as_u64()) {
                        None => (202, "text/plain".to_string(), String::new()), // a notification
                        Some(id) => {
                            let result = rpc_result(&msg);
                            let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                            if opts.sse_mode {
                                (
                                    200,
                                    "text/event-stream".to_string(),
                                    format!("event: message\ndata: {frame}\n\n"),
                                )
                            } else {
                                json(frame, 200)
                            }
                        }
                    }
                }
            };

        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{out}",
            out.len()
        );
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await?;
        Ok(())
    }

    fn rpc_result(msg: &Value) -> Value {
        match msg.get("method").and_then(|m| m.as_str()).unwrap_or("") {
            "initialize" => json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "http-fixture", "version": "0" },
            }),
            "tools/list" => {
                if msg
                    .get("params")
                    .and_then(|p| p.get("cursor"))
                    .and_then(|c| c.as_str())
                    == Some("p2")
                {
                    json!({ "tools": [{
                        "name": "boom",
                        "description": "Always fails.",
                        "inputSchema": { "type": "object", "properties": {} },
                    }] })
                } else {
                    json!({
                        "tools": [{
                            "name": "echo",
                            "description": "Echo the text back.",
                            "inputSchema": {
                                "type": "object",
                                "properties": { "text": { "type": "string" } },
                            },
                            "annotations": { "readOnlyHint": true },
                        }],
                        "nextCursor": "p2",
                    })
                }
            }
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if name == "echo" {
                    let text = args.get("text").cloned().unwrap_or(Value::Null);
                    json!({
                        "content": [{ "type": "text", "text": text }],
                        "structuredContent": { "echoed": text },
                    })
                } else {
                    json!({ "content": [{ "type": "text", "text": "kaboom" }], "isError": true })
                }
            }
            _ => json!({}),
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    fn temp_store() -> TokenStore {
        let dir = std::env::temp_dir().join(format!(
            "bough-mcp-tokens-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TokenStore::new(&TokenStoreOptions { dir: Some(dir) })
    }

    fn fast() -> Option<McpTimeouts> {
        Some(McpTimeouts {
            connect_ms: Some(4_000),
            request_ms: Some(2_000),
            call_ms: Some(2_000),
        })
    }

    /// What the layer above does with a connection attempt: one catalog entry per
    /// granted server, an `error` sentence when it did not connect, and
    /// `auth_required` when that sentence is a prompt rather than a fault.
    async fn catalog_entry(opts: RemoteConnectOptions) -> (Vec<String>, Option<String>, bool) {
        match McpRemoteClient::connect(opts).await {
            Err(error) => (vec![], Some(error.to_string()), is_auth_required(&error)),
            Ok(client) => {
                let out = client.list_tools().await;
                client.close().await;
                match out {
                    Ok(tools) => (tools.into_iter().map(|t| t.name).collect(), None, false),
                    Err(error) => (vec![], Some(error.to_string()), is_auth_required(&error)),
                }
            }
        }
    }

    // ---- the happy path ---------------------------------------------------

    #[tokio::test]
    async fn connects_paginates_tools_and_round_trips_a_call() {
        let fx = start_fixture(FixtureOptions::default()).await;
        let client = McpRemoteClient::connect(RemoteConnectOptions {
            name: "fix".into(),
            url: fx.url.clone(),
            auth: RemoteAuth::None, // this fixture needs no auth
            timeouts: fast(),
            ..Default::default()
        })
        .await
        .unwrap();

        let tools = client.list_tools().await.unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["echo", "boom"]
        );
        assert_eq!(
            tools[0].annotations.as_ref().unwrap()["readOnlyHint"],
            json!(true)
        );
        assert_eq!(
            tools[0]
                .input_schema
                .as_ref()
                .unwrap()
                .properties
                .as_ref()
                .unwrap()["text"],
            json!({ "type": "string" })
        );
        assert_eq!(
            client.server_info().unwrap().name.as_deref(),
            Some("http-fixture")
        );

        let echoed = client
            .call_tool("echo", json!({ "text": "hi" }))
            .await
            .unwrap();
        assert_eq!(echoed.structured_content, Some(json!({ "echoed": "hi" })));
        // A tool that fails is DATA, not an exception.
        let boom = client.call_tool("boom", json!({})).await.unwrap();
        assert_eq!(boom.is_error, Some(true));

        client.close().await;
        assert!(!client.alive());
    }

    #[tokio::test]
    async fn a_server_answering_in_sse_mode_is_read_the_same_way() {
        // A Streamable HTTP server may answer a POST with `text/event-stream`
        // carrying the response frame. Reading only `application/json` would make
        // every such server look like it answered nothing.
        let fx = start_fixture(FixtureOptions {
            sse_mode: true,
            ..Default::default()
        })
        .await;
        let client = McpRemoteClient::connect(RemoteConnectOptions {
            name: "fix".into(),
            url: fx.url.clone(),
            auth: RemoteAuth::None,
            timeouts: fast(),
            ..Default::default()
        })
        .await
        .unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["echo", "boom"]
        );
        client.close().await;
    }

    #[tokio::test]
    async fn static_registry_headers_reach_the_server() {
        let fx = start_fixture(FixtureOptions {
            accept: Some("static-token".into()),
            ..Default::default()
        })
        .await;
        let client = McpRemoteClient::connect(RemoteConnectOptions {
            name: "fix".into(),
            url: fx.url.clone(),
            headers: [(
                "authorization".to_string(),
                "Bearer static-token".to_string(),
            )]
            .into_iter()
            .collect(),
            auth: RemoteAuth::None,
            timeouts: fast(),
            ..Default::default()
        })
        .await
        .unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["echo", "boom"]
        );
        client.close().await;
    }

    // ---- 401 — the authorization prompt -----------------------------------

    #[tokio::test]
    async fn a_401_surfaces_as_an_authorization_prompt_not_an_error() {
        let fx = start_fixture(FixtureOptions {
            accept: Some("never-issued".into()),
            ..Default::default()
        })
        .await;
        let store = temp_store();
        let provider = Arc::new(
            BoughOAuthProvider::new(
                "notion",
                &ProviderOptions {
                    dir: Some(store.dir.clone()),
                    redirect_url: Some("http://127.0.0.1:4321/mcp/oauth/callback".into()),
                    ..Default::default()
                },
            )
            .unwrap(),
        );

        let (tools, error, auth_required) = catalog_entry(RemoteConnectOptions {
            name: "notion".into(),
            url: fx.url.clone(),
            dir: Some(store.dir.clone()),
            auth: RemoteAuth::Provider(provider.clone()),
            prefill: Prefill::None,
            timeouts: fast(),
            ..Default::default()
        })
        .await;

        // The catalog entry a turn renders: no tools, one sentence, flagged as a
        // prompt. NOT an exception thrown into the turn, and not a hang.
        assert!(tools.is_empty());
        assert!(auth_required);
        let error = error.unwrap();
        assert!(error.contains(&auth_prompt("notion")), "{error}");
        assert!(error.contains("not authorized"), "{error}");
        // NAMES A GESTURE THAT EXISTS: a prompt naming a slash command is a failure.
        assert!(!error.contains("/mcp auth"), "{error}");
        assert!(error.contains("^p"), "{error}");

        // And the human's next step actually exists: the flow got as far as PKCE, so
        // there is a URL to open and a verifier waiting for the callback.
        let url = provider
            .authorization_url()
            .expect("an authorization URL for the human");
        let q = crate::mcp::oauth::parse_query(url.split_once('?').unwrap().1);
        assert_eq!(
            q.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            q.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:4321/mcp/oauth/callback")
        );
        assert!(q.get("state").unwrap().starts_with("notion."));
        assert_eq!(fx.seen.lock().unwrap().registrations, 1);
        assert_eq!(
            fx.seen.lock().unwrap().registered[0]["token_endpoint_auth_method"],
            json!("none")
        );
    }

    #[tokio::test]
    async fn the_401_prompt_survives_an_auth_flow_that_fails_after_the_401() {
        // A server that answers 401 but publishes no OAuth metadata at all: discovery
        // and registration both fail, and the error that escapes is about THAT. It
        // must still read as "nobody has authorized this yet", because that is what
        // the user has to fix.
        let fx = start_fixture(FixtureOptions {
            bare_401: true,
            ..Default::default()
        })
        .await;
        let store = temp_store();
        let error = McpRemoteClient::connect(RemoteConnectOptions {
            name: "bare".into(),
            url: fx.url.clone(),
            dir: Some(store.dir.clone()),
            prefill: Prefill::None,
            timeouts: fast(),
            ..Default::default()
        })
        .await
        .err()
        .expect("an auth prompt");
        assert_eq!(error.status(), 401);
        assert!(is_auth_required(&error));
        assert!(error.to_string().contains(&auth_prompt("bare")), "{error}");
    }

    // ---- refresh -----------------------------------------------------------

    #[tokio::test]
    async fn an_expired_access_token_is_refreshed_inside_the_transport_invisibly() {
        let fx = start_fixture(FixtureOptions {
            accept: Some("fresh-1".into()),
            refresh: vec![("r-good".into(), ("fresh-1".into(), Some("r-2".into())))],
            ..Default::default()
        })
        .await;
        let store = temp_store();
        // A server authorized in some previous session: registration and a stale pair.
        store
            .write(
                "linear",
                &Stored {
                    client: Some(ClientInfo {
                        client_id: "dyn-client".into(),
                        ..Default::default()
                    }),
                    tokens: Some(OAuthTokens {
                        access_token: "stale".into(),
                        token_type: "Bearer".into(),
                        refresh_token: Some("r-good".into()),
                        ..Default::default()
                    }),
                    expires_at: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();

        let client = McpRemoteClient::connect(RemoteConnectOptions {
            name: "linear".into(),
            url: fx.url.clone(),
            dir: Some(store.dir.clone()),
            prefill: Prefill::None,
            timeouts: fast(),
            ..Default::default()
        })
        .await
        .unwrap();
        // The caller sees a working connection; the 401 and the refresh happened
        // under it.
        let tools = client.list_tools().await.unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["echo", "boom"]
        );
        assert_eq!(
            client
                .call_tool("echo", json!({ "text": "yo" }))
                .await
                .unwrap()
                .structured_content,
            Some(json!({ "echoed": "yo" }))
        );
        client.close().await;

        // Exactly one refresh grant, and the new pair is what is persisted —
        // including the rotated refresh token, or the NEXT expiry starts the whole
        // flow over.
        assert_eq!(fx.seen.lock().unwrap().grants, vec!["refresh_token"]);
        let stored = store.load("linear").unwrap();
        assert_eq!(stored.tokens.as_ref().unwrap().access_token, "fresh-1");
        assert_eq!(
            stored.tokens.as_ref().unwrap().refresh_token.as_deref(),
            Some("r-2")
        );
        // Registration was reused rather than repeated.
        assert_eq!(fx.seen.lock().unwrap().registrations, 0);
        // The stale token was presented first, the fresh one after — the retry is
        // real.
        assert_eq!(
            fx.seen.lock().unwrap().bearers[..2].to_vec(),
            vec![
                Some("Bearer stale".to_string()),
                Some("Bearer fresh-1".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn an_expired_refresh_token_degrades_to_the_same_authorization_prompt() {
        // The token endpoint rejects the refresh with invalid_grant. The flow drops
        // the tokens and starts a fresh authorization, which is a REDIRECT — so the
        // human gets the same one-command prompt as a never-authorized server.
        let fx = start_fixture(FixtureOptions {
            accept: Some("never-issued".into()),
            ..Default::default()
        })
        .await;
        let store = temp_store();
        store
            .write(
                "linear",
                &Stored {
                    client: Some(ClientInfo {
                        client_id: "dyn-client".into(),
                        ..Default::default()
                    }),
                    tokens: Some(OAuthTokens {
                        access_token: "stale".into(),
                        token_type: "Bearer".into(),
                        refresh_token: Some("r-dead".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        let provider = Arc::new(
            BoughOAuthProvider::new(
                "linear",
                &ProviderOptions {
                    dir: Some(store.dir.clone()),
                    redirect_url: Some("http://127.0.0.1:4321/mcp/oauth/callback".into()),
                    ..Default::default()
                },
            )
            .unwrap(),
        );

        let (tools, error, auth_required) = catalog_entry(RemoteConnectOptions {
            name: "linear".into(),
            url: fx.url.clone(),
            dir: Some(store.dir.clone()),
            auth: RemoteAuth::Provider(provider.clone()),
            prefill: Prefill::None,
            timeouts: fast(),
            ..Default::default()
        })
        .await;
        assert!(tools.is_empty());
        assert!(auth_required);
        assert!(error.unwrap().contains(&auth_prompt("linear")));

        // The dead pair is gone, so nothing keeps re-presenting it, and there is a
        // URL for the human to open.
        assert_eq!(store.load("linear").unwrap().tokens, None);
        assert!(provider.authorization_url().is_some());
        // The registration survived the token clear — re-registering on every expiry
        // would leave a trail of dead clients on the authorization server.
        assert_eq!(
            store.load("linear").unwrap().client,
            Some(ClientInfo {
                client_id: "dyn-client".into(),
                ..Default::default()
            })
        );
        assert_eq!(fx.seen.lock().unwrap().grants, vec!["refresh_token"]);
    }

    // ---- bounded failure — never a hang -----------------------------------

    #[tokio::test]
    async fn a_server_that_accepts_and_never_answers_fails_on_the_deadline() {
        let fx = start_fixture(FixtureOptions {
            stall: true,
            ..Default::default()
        })
        .await;
        let error = McpRemoteClient::connect(RemoteConnectOptions {
            name: "wedged".into(),
            url: fx.url.clone(),
            auth: RemoteAuth::None,
            timeouts: Some(McpTimeouts {
                connect_ms: Some(400),
                request_ms: Some(300),
                call_ms: Some(300),
            }),
            ..Default::default()
        })
        .await
        .err()
        .expect("a deadline");
        assert!(
            !is_auth_required(&error),
            "a wedged server is a fault, not an auth prompt"
        );
        assert_eq!(error.status(), 504, "{error}");
        assert!(error.to_string().contains("\"wedged\""), "{error}");
    }

    #[tokio::test]
    async fn an_unreachable_server_fails_by_name_and_is_not_an_auth_prompt() {
        let error = McpRemoteClient::connect(RemoteConnectOptions {
            name: "dead".into(),
            url: "http://127.0.0.1:1/mcp".into(),
            auth: RemoteAuth::None,
            timeouts: fast(),
            ..Default::default()
        })
        .await
        .err()
        .expect("a failure");
        assert!(!is_auth_required(&error));
        assert!(error.to_string().contains("\"dead\""), "{error}");
        assert!(
            error.to_string().contains("http://127.0.0.1:1/mcp"),
            "{error}"
        );
        // AND IT SAYS IT ONCE. `note()` appends "last transport error: …" — the
        // diagnostic from BEFORE this failure, which is how TS fills it (only the
        // SDK's `onerror`, never the error being raised). Recording the detail
        // before formatting made every remote failure cite itself: `failed
        // connect: <detail> — last transport error: <detail>`. Caught by diffing
        // `bough mcp list` against the TS client on a registry with an
        // unreachable remote (G3).
        assert!(
            !error.to_string().contains("last transport error"),
            "the first failure has nothing earlier to cite: {error}"
        );
    }

    #[tokio::test]
    async fn an_unusable_url_is_refused_before_anything_is_opened() {
        let error = McpRemoteClient::connect(RemoteConnectOptions {
            name: "bad".into(),
            url: "not a url".into(),
            auth: RemoteAuth::None,
            ..Default::default()
        })
        .await
        .err()
        .expect("a refusal");
        assert_eq!(error.status(), 400);
        assert!(error.to_string().contains("unusable `url`"), "{error}");
    }

    #[tokio::test]
    async fn a_closed_connection_refuses_further_calls_instead_of_hanging() {
        let fx = start_fixture(FixtureOptions::default()).await;
        let client = McpRemoteClient::connect(RemoteConnectOptions {
            name: "fix".into(),
            url: fx.url.clone(),
            auth: RemoteAuth::None,
            timeouts: fast(),
            ..Default::default()
        })
        .await
        .unwrap();
        client.close().await;
        let error = client.list_tools().await.expect_err("a refusal");
        assert!(error.to_string().contains("disconnected"), "{error}");
    }

    // ---- pure helpers ------------------------------------------------------

    #[test]
    fn the_prompt_and_its_discriminator_agree() {
        let e = auth_required_error("slack", Some("registration_endpoint missing"));
        assert_eq!(e.status(), 401);
        assert!(is_auth_required(&e));
        assert!(e.to_string().contains(&auth_prompt("slack")));
        // The underlying reason is kept as a parenthetical: "not authorized" is the
        // move, but "registration_endpoint missing" is what a maintainer needs.
        assert!(
            e.to_string().contains("(registration_endpoint missing)"),
            "{e}"
        );
        // …and every other McpError is a fault, not a prompt.
        assert!(!is_auth_required(&mcp(502, "broken")));
    }

    #[test]
    fn an_unrecognized_call_result_becomes_structured_content_never_a_failure() {
        let out = call_result(&json!({ "whatever": 1 }));
        assert_eq!(out.structured_content, Some(json!({ "whatever": 1 })));
        assert_eq!(out.is_error, None);
    }

    #[test]
    fn a_tool_entry_with_no_usable_name_is_dropped_entirely() {
        assert!(tool_info(&json!({ "description": "no name" })).is_none());
        assert!(tool_info(&json!({ "name": "" })).is_none());
        // …but a schema missing `type: "object"` must NOT drop a callable tool.
        let t = tool_info(&json!({ "name": "ok", "inputSchema": { "properties": {} } })).unwrap();
        assert_eq!(t.name, "ok");
        assert!(t.input_schema.is_some());
    }

    #[test]
    fn sse_frames_are_read_out_of_an_event_stream_body() {
        let frames = sse_data_frames("event: message\ndata: {\"id\":1}\n\ndata: {\"id\":2}\n\n");
        assert_eq!(frames, vec![json!({"id": 1}), json!({"id": 2})]);
        // A multi-line data field joins with a newline, per the EventSource grammar.
        assert_eq!(
            sse_data_frames("data: {\"a\":\ndata: 1}\n\n"),
            vec![json!({"a": 1})]
        );
    }
}
