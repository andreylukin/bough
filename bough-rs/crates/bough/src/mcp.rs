//! `bough mcp <verb>` — the headless client for the MCP registry (port of
//! `src/cli/mcp.ts`).
//!
//! WHY THIS EXISTS. Everything here was already reachable: the routes have been in
//! the table since T7, and the `/mcp` panel drives all of them. But the panel is the
//! ONLY thing that did, which made the whole surface unusable from a script, from a
//! remote shell, and from an agent working on this repo — and unusable is where the
//! bugs hid.
//!
//! THE VERB THAT MATTERS IS `doctor`. `list` says what is registered and `test` says
//! whether one server answers, but the real question is never about one server — it
//! is "why is none of this working", and answering it means connecting everything and
//! saying, per server, which of the handful of distinct causes applies. Those causes
//! are knowable and few: not granted, no credential, a credential another client owns
//! that has gone stale, a credential that was never there, or an endpoint that
//! refuses. Each has a different fix and the errors alone do not sort them.
//!
//! Conventions are `exec.rs`'s: argument parsing is pure and total, every effect is
//! injected, and `run_mcp` RETURNS an exit code.
//!
//!   0  the verb did what it says — including `doctor` finding everything healthy
//!   1  the operation ran and the answer is bad
//!   2  usage problem, or no server on the port
//!
//! The 0/1 split is what makes this usable in CI.

use std::sync::Arc;

use bough_core::mcp::status::McpStatus;
use futures::future::BoxFuture;
use serde_json::{json, Value};

/// Verbs, in the order the help lists them.
const VERBS: [&str; 10] = [
    "list", "test", "auth", "logout", "grant", "revoke", "add", "remove", "doctor", "call",
];

/// Verbs that name a server. `add` needs a URL beside it.
const NEEDS_NAME: [&str; 8] = [
    "call", "test", "auth", "logout", "grant", "revoke", "add", "remove",
];

#[derive(Debug, Clone, PartialEq)]
pub struct McpArgs {
    pub verb: String,
    /// The server the verb acts on. Absent for `list` and `doctor`.
    pub name: Option<String>,
    /// `add` only: the remote endpoint. `call` only: the tool name.
    pub url: Option<String>,
    /// `call` only: the tool's arguments as JSON. Absent = no arguments.
    pub args_json: Option<String>,
    pub json: bool,
    pub port: Option<u32>,
    /// `auth` only: seconds to wait for the browser round trip.
    pub timeout: f64,
    /// `test`/`doctor` only: the conversation a LOCAL server's subprocess runs in.
    ///
    /// A stdio entry is a command spawned in a checkout, so the route refuses a
    /// scopeless connect — there is no "the" workspace for a CLI. Absent means local
    /// servers are reported as untested rather than as broken.
    pub session: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Args(McpArgs),
    UsageError(String),
}

pub const USAGE: &str = "usage: bough mcp <verb> [name] [--json] [--port N]

  list                    every server: grant, connection, credential
  doctor                  connect them all and say what to do about each
  test NAME               connect one server now and report its tools
  auth NAME               authorize: prints a URL, waits, then connects
  logout NAME             forget the credentials bough stored for NAME
  grant NAME              let every conversation call it
  revoke NAME             take that back, everywhere
  call NAME TOOL [JSON]   call one of NAME's tools; JSON may come on stdin
  add NAME URL            register a remote server
  remove NAME             drop the registration and any grants it holds

  --json                  machine-readable output
  --session ID            test/doctor: conversation to run LOCAL servers in
  --port N                server port (default BOUGH_PORT, else 4321)
  --timeout SECS          auth only: how long to wait for the browser (default 180)

exit: 0 fine · 1 something is broken · 2 usage or no server";

/// Parse `bough mcp`'s arguments. Pure and total.
///
/// No verb at all is `list`, because the question people actually arrive with is
/// "what have I got" and making them type it is friction over the common case.
pub fn parse_mcp_args(argv: &[String]) -> Parsed {
    let mut positional: Vec<String> = Vec::new();
    let mut json = false;
    let mut port: Option<u32> = None;
    let mut timeout = 180.0f64;
    let mut session: Option<String> = None;
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "-h" || a == "--help" {
            return Parsed::UsageError(USAGE.to_string());
        }
        if a == "--json" {
            json = true;
            i += 1;
            continue;
        }
        if a == "--session" {
            let Some(raw) = argv.get(i + 1) else {
                return Parsed::UsageError(format!("--session needs a value\n{USAGE}"));
            };
            session = Some(raw.clone());
            i += 2;
            continue;
        }
        if a == "--port" || a == "--timeout" {
            let Some(raw) = argv.get(i + 1) else {
                return Parsed::UsageError(format!("{a} needs a value\n{USAGE}"));
            };
            let n: f64 = raw.trim().parse().unwrap_or(f64::NAN);
            if !n.is_finite() || n <= 0.0 {
                return Parsed::UsageError(format!(
                    "{a} needs a positive number, got \"{raw}\"\n{USAGE}"
                ));
            }
            if a == "--port" {
                port = Some(n as u32);
            } else {
                timeout = n;
            }
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            return Parsed::UsageError(format!("unknown flag {a}\n{USAGE}"));
        }
        positional.push(a.to_string());
        i += 1;
    }
    let Some(verb) = positional.first().cloned() else {
        return Parsed::Args(McpArgs {
            verb: "list".into(),
            name: None,
            url: None,
            args_json: None,
            json,
            port,
            timeout,
            session,
        });
    };
    if !VERBS.contains(&verb.as_str()) {
        return Parsed::UsageError(format!(
            "unknown verb \"{verb}\" — one of {}\n{USAGE}",
            VERBS.join(", ")
        ));
    }
    let name = positional.get(1).cloned().filter(|s| !s.is_empty());
    let url = positional.get(2).cloned().filter(|s| !s.is_empty());
    if NEEDS_NAME.contains(&verb.as_str()) && name.is_none() {
        return Parsed::UsageError(format!("{verb} needs a server name\n{USAGE}"));
    }
    if verb == "add" && url.is_none() {
        return Parsed::UsageError(format!(
            "add needs a name and a URL: bough mcp add notion https://…\n{USAGE}"
        ));
    }
    if verb == "call" && url.is_none() {
        return Parsed::UsageError(format!(
            "call needs a server and a tool: bough mcp call notion search '{{\"q\":\"x\"}}'\n{USAGE}"
        ));
    }
    Parsed::Args(McpArgs {
        verb,
        name,
        url,
        args_json: positional.get(3).cloned(),
        json,
        port,
        timeout,
        session,
    })
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

pub struct McpRequest {
    pub method: String,
    pub url: String,
    pub body: Option<String>,
}

pub struct McpResponse {
    pub status: u16,
    pub text: String,
}

pub type McpFetch =
    Arc<dyn Fn(McpRequest) -> BoxFuture<'static, Result<McpResponse, String>> + Send + Sync>;

pub struct McpDeps {
    pub fetch: McpFetch,
    pub out: Arc<dyn Fn(&str) + Send + Sync>,
    pub err: Arc<dyn Fn(&str) + Send + Sync>,
    pub env: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    /// Injected so the auth poll does not sleep in tests.
    pub sleep: Arc<dyn Fn(u64) -> BoxFuture<'static, ()> + Send + Sync>,
    /// `call` only: reads the tool's arguments when they are not an argv word.
    /// Only INVOKED when `call` asks — a verb that blocked on stdin would hang
    /// every other invocation.
    pub stdin: Option<Arc<dyn Fn() -> BoxFuture<'static, String> + Send + Sync>>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

/// One server's connect outcome, as the route reports it.
#[derive(Debug, Clone, Default)]
struct ConnectResult {
    connected: bool,
    error: Option<String>,
    tools: Vec<String>,
}

/// How a row reads. The glyphs are the panel's, deliberately — one vocabulary.
fn glyph(status: &Value, name: &str) -> &'static str {
    let alive = connection(status, name)
        .and_then(|c| c["alive"].as_bool())
        .unwrap_or(false);
    if alive {
        return "●";
    }
    if is_active(status, name) {
        "◐"
    } else {
        "○"
    }
}

fn connection<'a>(status: &'a Value, name: &str) -> Option<&'a Value> {
    status["connections"]
        .as_array()?
        .iter()
        .find(|c| c["server"].as_str() == Some(name))
}

fn is_active(status: &Value, name: &str) -> bool {
    status["active"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(name)))
}

fn is_remote(status: &Value, name: &str) -> bool {
    status["registry"]["servers"][name]["url"].is_string()
}

fn is_authorized(status: &Value, name: &str) -> bool {
    status["auth"][name]["authorized"]
        .as_bool()
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ok,
    Bad,
    Unknown,
}

/// Every distinct reason a server is not usable, and what to do about it.
///
/// THIS IS THE POINT OF `doctor`. The connect error alone does not sort these: "has
/// no string at #mcpOAuth…" and "expired at …" and a 401 are three different jobs for
/// the user, and two of them are not bough's to fix. Ordered by what has to be true
/// first — a server nobody granted will never connect, so saying anything about its
/// credential would be advice about a step that has not been reached.
fn diagnose(
    status: &Value,
    name: &str,
    conn: Option<&ConnectResult>,
    session: Option<&str>,
) -> (State, String) {
    if let Some(c) = conn {
        if c.connected {
            let n = c.tools.len();
            return (
                State::Ok,
                format!("{n} tool{}", if n == 1 { "" } else { "s" }),
            );
        }
    }
    if !is_active(status, name) {
        return (State::Bad, format!("not granted — bough mcp grant {name}"));
    }
    // LOCAL SERVERS CANNOT BE TESTED WITHOUT A CONVERSATION, and that is not a fault.
    // Reported as UNKNOWN rather than broken: counting an untested server as a failure
    // would make `doctor` exit 1 on a perfectly good setup, and the exit code is the
    // part of this verb a script depends on.
    let remote = is_remote(status, name);
    if !remote && session.is_none() {
        return (
            State::Unknown,
            "local command — not tested; needs a conversation: bough mcp doctor --session ID"
                .to_string(),
        );
    }
    let error = conn
        .and_then(|c| c.error.clone())
        .unwrap_or_else(|| "did not connect".to_string());
    // A credential this machine's OTHER client owns. bough deliberately never
    // refreshes one it did not obtain, so the fix is always in that client and saying
    // so beats repeating the error.
    if error.contains("expired at") {
        return (
            State::Bad,
            format!(
                "its Claude Code grant expired — use that server in Claude Code once, or: \
                 bough mcp auth {name}"
            ),
        );
    }
    if error.contains("has no string at") {
        return (
            State::Bad,
            format!(
                "Claude Code's grant for it is empty — re-authorize it there, or authorize \
                 bough separately: bough mcp auth {name}"
            ),
        );
    }
    // ONLY REMOTE SERVERS HAVE CREDENTIALS. `status.auth` is populated for `url`
    // entries alone, so a local command always reads as unauthorized — and telling
    // someone to run `bough mcp auth` on a stdio server sends them to a flow that
    // cannot exist.
    if remote && !is_authorized(status, name) {
        return (State::Bad, format!("no credential — bough mcp auth {name}"));
    }
    (State::Bad, error)
}

/// The conversation this command belongs to.
///
/// `--session` wins; otherwise `$BOUGH_SESSION`, which every shell a turn spawns
/// carries. That default is what makes `bough mcp call` behave like the host function
/// it replaced: the grant enforced is the one belonging to the turn that ran the
/// command, without the model having to know its own session id or being trusted to
/// report it honestly.
fn session_of(args: &McpArgs, deps: &McpDeps) -> Option<String> {
    args.session
        .clone()
        .or_else(|| (deps.env)("BOUGH_SESSION"))
        .filter(|s| !s.is_empty())
}

fn base(args: &McpArgs, deps: &McpDeps) -> String {
    let port = args.port.unwrap_or_else(|| {
        (deps.env)("BOUGH_PORT")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(4321)
    });
    format!("http://127.0.0.1:{port}")
}

struct Answer {
    status: u16,
    body: Value,
}

/// The error sentence a route returned, or a fallback naming the status.
fn error_of(r: &Answer) -> String {
    r.body["error"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("HTTP {}", r.status))
}

/// A request, with the server-is-not-running case turned into exit code 2.
async fn call_route(deps: &McpDeps, req: McpRequest) -> Option<Answer> {
    let host = host_of(&req.url);
    match (deps.fetch)(req).await {
        Ok(res) => {
            let body = if res.text.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&res.text).unwrap_or_else(|_| json!({ "error": res.text }))
            };
            Some(Answer {
                status: res.status,
                body,
            })
        }
        Err(e) => {
            (deps.err)(&format!(
                "no bough server at {host} ({e}). Start one: bough start"
            ));
            None
        }
    }
}

fn host_of(url: &str) -> String {
    url.find("://")
        .map(|i| &url[i + 3..])
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .unwrap_or(url)
        .to_string()
}

fn get(url: String) -> McpRequest {
    McpRequest {
        method: "GET".into(),
        url,
        body: None,
    }
}

fn post(url: String, body: Option<Value>) -> McpRequest {
    McpRequest {
        method: "POST".into(),
        url,
        body: body.map(|v| v.to_string()),
    }
}

/// `encodeURIComponent` for a path segment.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub async fn run_mcp(argv: &[String], deps: &McpDeps) -> i32 {
    let args = match parse_mcp_args(argv) {
        Parsed::UsageError(message) => {
            (deps.err)(&message);
            return 2;
        }
        Parsed::Args(a) => a,
    };
    let root = base(&args, deps);
    let session = session_of(&args, deps);

    macro_rules! answer {
        ($req:expr) => {
            match call_route(deps, $req).await {
                Some(a) => a,
                None => return 2,
            }
        };
    }

    // The whole state document, or `None` when the port answered badly.
    async fn status_doc(deps: &McpDeps, root: &str) -> Option<Value> {
        let r = call_route(deps, get(format!("{root}/mcp/servers"))).await?;
        if r.status != 200 {
            (deps.err)(&error_of(&r));
            return None;
        }
        Some(r.body)
    }

    async fn connect(
        deps: &McpDeps,
        root: &str,
        name: &str,
        session: Option<&str>,
    ) -> Option<ConnectResult> {
        let q = session
            .map(|s| format!("?session={}", enc(s)))
            .unwrap_or_default();
        let r = call_route(
            deps,
            post(format!("{root}/mcp/servers/{}/connect{q}", enc(name)), None),
        )
        .await?;
        // A route-level refusal (an unknown name) is not a connect result; report it
        // as one so every caller has a single shape to read.
        if r.status >= 400 && r.body.get("connected").is_none() {
            return Some(ConnectResult {
                connected: false,
                error: Some(error_of(&r)),
                tools: vec![],
            });
        }
        Some(ConnectResult {
            connected: r.body["connected"].as_bool().unwrap_or(false),
            error: r.body["error"].as_str().map(|s| s.to_string()),
            tools: r.body["tools"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    match args.verb.as_str() {
        "list" => {
            let Some(s) = status_doc(deps, &root).await else {
                return 2;
            };
            if args.json {
                // Through the TYPED document, not the raw `Value`. TS prints
                // `JSON.stringify(status, null, 2)` over the parsed response, so
                // its keys come out in the order the route wrote them; a
                // `serde_json::Value` re-serializes its BTreeMap alphabetically
                // and would silently reorder every key. `McpStatus` declares the
                // four keys in the route's order, so round-tripping through it
                // reproduces the TS bytes. A body this cannot type is still
                // worth printing — falling back keeps `--json` from going empty
                // on an unexpected server.
                let text = serde_json::from_value::<McpStatus>(s.clone())
                    .ok()
                    .and_then(|typed| serde_json::to_string_pretty(&typed).ok())
                    .unwrap_or_else(|| serde_json::to_string_pretty(&s).unwrap_or_default());
                (deps.out)(&text);
                return 0;
            }
            let names = server_names(&s);
            if names.is_empty() {
                (deps.out)("no MCP servers registered — bough mcp add NAME URL, or bough sync-mcp");
                return 0;
            }
            for name in &names {
                let conn = connection(&s, name);
                let alive = conn.and_then(|c| c["alive"].as_bool()).unwrap_or(false);
                let mut bits: Vec<String> = Vec::new();
                bits.push(
                    if is_active(&s, name) {
                        "granted"
                    } else {
                        "not granted"
                    }
                    .to_string(),
                );
                if alive {
                    let n = conn.and_then(|c| c["toolCount"].as_u64()).unwrap_or(0);
                    bits.push(format!("{n} tools"));
                }
                if is_authorized(&s, name) {
                    bits.push("authed".to_string());
                }
                if let Some(e) = conn.and_then(|c| c["error"].as_str()) {
                    bits.push(e.to_string());
                }
                (deps.out)(&format!("{} {name}  {}", glyph(&s, name), bits.join(" · ")));
            }
            // The glyph legend, for the same reason the panel grew one: three marks
            // carrying the whole state of a row, explained nowhere, is how "it stays
            // a half circle" becomes a bug report.
            (deps.out)("");
            (deps.out)("● connected · ◐ granted, not connected · ○ not granted");
            0
        }

        "doctor" => {
            let Some(s) = status_doc(deps, &root).await else {
                return 2;
            };
            let names = server_names(&s);
            if names.is_empty() {
                (deps.out)("no MCP servers registered — bough mcp add NAME URL, or bough sync-mcp");
                return 0;
            }
            // Sequential ON PURPOSE. These connect to third-party endpoints and some
            // of them spawn subprocesses; a burst of parallel handshakes makes a slow
            // server look like a broken one, and the output is read top to bottom.
            let mut rows: Vec<(String, State, String)> = Vec::new();
            for name in &names {
                // Do not spawn a connect that is already known to be refused: a local
                // server with no session gets its answer without a round trip.
                let testable = is_active(&s, name) && (is_remote(&s, name) || session.is_some());
                let conn = if testable {
                    match connect(deps, &root, name, session.as_deref()).await {
                        Some(c) => Some(c),
                        None => return 2,
                    }
                } else {
                    None
                };
                let (state, note) = diagnose(&s, name, conn.as_ref(), session.as_deref());
                rows.push((name.clone(), state, note));
            }
            if args.json {
                let doc: Vec<Value> = rows
                    .iter()
                    .map(|(name, state, note)| {
                        json!({ "name": name, "state": state_word(*state), "note": note })
                    })
                    .collect();
                (deps.out)(&serde_json::to_string_pretty(&doc).unwrap_or_default());
            } else {
                for (name, state, note) in &rows {
                    let mark = match state {
                        State::Ok => "✓",
                        State::Bad => "✗",
                        State::Unknown => "?",
                    };
                    (deps.out)(&format!("{mark} {name}  {note}"));
                }
                let bad = rows.iter().filter(|r| r.1 == State::Bad).count();
                let unknown = rows.iter().filter(|r| r.1 == State::Unknown).count();
                let tail = if unknown > 0 {
                    format!(" · {unknown} not tested")
                } else {
                    String::new()
                };
                (deps.out)("");
                (deps.out)(&if bad == 0 {
                    let tested = rows.len() - unknown;
                    format!(
                        "all {tested} tested server{} working{tail}",
                        if tested == 1 { "" } else { "s" }
                    )
                } else {
                    format!(
                        "{bad} of {} need{} attention{tail}",
                        rows.len(),
                        if bad == 1 { "s" } else { "" }
                    )
                });
            }
            i32::from(rows.iter().any(|r| r.1 == State::Bad))
        }

        "test" => {
            let name = args.name.clone().unwrap_or_default();
            let Some(r) = connect(deps, &root, &name, session.as_deref()).await else {
                return 2;
            };
            if args.json {
                (deps.out)(
                    &serde_json::to_string_pretty(&json!({
                        "server": name,
                        "connected": r.connected,
                        "error": r.error,
                        "tools": r.tools.iter().map(|t| json!({"name": t})).collect::<Vec<_>>(),
                    }))
                    .unwrap_or_default(),
                );
                return i32::from(!r.connected);
            }
            if r.connected {
                let n = r.tools.len();
                let mut line = format!(
                    "✓ {name} connected · {n} tool{}",
                    if n == 1 { "" } else { "s" }
                );
                if n > 0 {
                    line.push_str(&format!("\n  {}", r.tools.join(", ")));
                }
                (deps.out)(&line);
                return 0;
            }
            (deps.err)(&format!(
                "✗ {name} did not connect — {}",
                r.error.unwrap_or_else(|| "no reason given".into())
            ));
            1
        }

        "auth" => {
            let name = args.name.clone().unwrap_or_default();
            let begun = answer!(post(
                format!("{root}/mcp/servers/{}/auth", enc(&name)),
                None
            ));
            if begun.status >= 400 {
                (deps.err)(&error_of(&begun));
                return 1;
            }
            if begun.body["status"].as_str() == Some("authorized") {
                (deps.out)(&format!("{name} was already authorized"));
            } else {
                let Some(url) = begun.body["authorizationUrl"]
                    .as_str()
                    .map(|s| s.to_string())
                else {
                    (deps.err)(&format!(
                        "{name}: the server asked for authorization but sent no URL"
                    ));
                    return 1;
                };
                if let Some(corrected) = begun.body["correctedUrl"].as_str() {
                    // The registry was rewritten on the way through: the published
                    // endpoint is often not the one the flow wants, and a silent
                    // rewrite is a surprise the next reader of the registry has to
                    // solve.
                    (deps.out)(&format!("note: its endpoint was corrected to {corrected}"));
                }
                // PRINTED, never opened. This client is used over SSH and in CI as
                // often as on a desktop, and shelling out to a browser hangs where
                // there is none.
                (deps.out)(&format!(
                    "open this to authorize {name}, then come back — it finishes on its own:"
                ));
                (deps.out)(&format!("  {url}"));
                let deadline = (deps.now)() + (args.timeout * 1000.0) as i64;
                let mut authorized = false;
                while (deps.now)() < deadline {
                    (deps.sleep)(1000).await;
                    let st = answer!(get(format!("{root}/mcp/servers/{}/auth", enc(&name))));
                    if st.body["authorized"].as_bool().unwrap_or(false) {
                        authorized = true;
                        break;
                    }
                }
                if !authorized {
                    (deps.err)(&format!(
                        "{name}: still waiting on the browser after {}s — run auth again",
                        fmt_num(args.timeout)
                    ));
                    return 1;
                }
                (deps.out)(&format!("{name} is authorized"));
            }
            // CONNECT, do not stop at "authorized". Storing tokens changes no
            // observable state — the panel's `◐` is about a CONNECTION — and a flow
            // whose success is invisible reads as a flow that failed.
            let Some(r) = connect(deps, &root, &name, None).await else {
                return 2;
            };
            if r.connected {
                let n = r.tools.len();
                (deps.out)(&format!(
                    "✓ {name} connected · {n} tool{}",
                    if n == 1 { "" } else { "s" }
                ));
                let granted = match status_doc(deps, &root).await {
                    Some(s) => is_active(&s, &name),
                    None => false,
                };
                if !granted {
                    (deps.out)(&format!("  not granted yet — bough mcp grant {name}"));
                }
                return 0;
            }
            (deps.err)(&format!(
                "{name} is authorized but did not connect — {}",
                r.error.unwrap_or_else(|| "no reason given".into())
            ));
            1
        }

        "logout" => {
            let name = args.name.clone().unwrap_or_default();
            let r = answer!(McpRequest {
                method: "DELETE".into(),
                url: format!("{root}/mcp/servers/{}/auth", enc(&name)),
                body: None,
            });
            if r.status >= 400 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            (deps.out)(&format!(
                "forgot bough's credentials for {name} — the registration is untouched"
            ));
            0
        }

        "grant" | "revoke" => {
            let name = args.name.clone().unwrap_or_default();
            let on = args.verb == "grant";
            let verb = if on { "enable" } else { "disable" };
            // The GLOBAL scope: every conversation. A per-session grant is a thing
            // the panel does because it has a session on screen; a CLI does not, and
            // inventing one here would make the verb mean something different from
            // what it says.
            let r = answer!(post(
                format!("{root}/mcp/servers/{}/{verb}", enc(&name)),
                Some(json!({ "sessionId": "" }))
            ));
            if r.status >= 400 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            (deps.out)(&if on {
                format!("{name} is granted in every conversation")
            } else {
                format!("{name} is revoked everywhere")
            });
            0
        }

        "add" => {
            let name = args.name.clone().unwrap_or_default();
            let r = answer!(McpRequest {
                method: "PUT".into(),
                url: format!("{root}/mcp/servers/{}", enc(&name)),
                body: Some(json!({ "url": args.url.clone().unwrap_or_default() }).to_string()),
            });
            if r.status >= 400 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            (deps.out)(&format!(
                "{name} registered — bough mcp auth {name}, then grant it"
            ));
            0
        }

        "call" => {
            let server = args.name.clone().unwrap_or_default();
            let tool = args.url.clone().unwrap_or_default();
            // Arguments may arrive as an argv word or on stdin. Stdin is what makes
            // this usable from a program: a tool's parameters are frequently larger
            // and more quote-hostile than a shell word wants to be.
            let mut raw = args.args_json.clone();
            if raw.is_none() {
                if let Some(read) = &deps.stdin {
                    let text = read().await.trim().to_string();
                    raw = (!text.is_empty()).then_some(text);
                }
            }
            let parsed = match raw {
                None => json!({}),
                Some(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(v) => v,
                    Err(_) => {
                        (deps.err)(
                            "the arguments were not valid JSON. Pass a plain object matching \
                             the tool's parameters, e.g. '{\"query\":\"x\"}'.",
                        );
                        return 2;
                    }
                },
            };
            let q = session
                .as_deref()
                .map(|s| format!("?session={}", enc(s)))
                .unwrap_or_default();
            let r = answer!(post(
                format!(
                    "{root}/mcp/servers/{}/tools/{}{q}",
                    enc(&server),
                    enc(&tool)
                ),
                Some(parsed)
            ));
            if r.status >= 400 {
                // The route's own sentence, verbatim: an ungranted server names what
                // IS granted and says a human grants more, and a bad tool name lists
                // the real ones. Rewriting either would lose the part that resolves it.
                (deps.err)(&error_of(&r));
                return 1;
            }
            // The RESULT, not the envelope. A program parses this, and making it dig
            // the payload out of a wrapper it did not ask for is friction with no
            // benefit.
            let result = r.body.get("result").cloned().unwrap_or(Value::Null);
            (deps.out)(&if args.json {
                serde_json::to_string_pretty(&result).unwrap_or_default()
            } else {
                serde_json::to_string(&result).unwrap_or_default()
            });
            0
        }

        "remove" => {
            let name = args.name.clone().unwrap_or_default();
            let r = answer!(McpRequest {
                method: "DELETE".into(),
                url: format!("{root}/mcp/servers/{}", enc(&name)),
                body: None,
            });
            if r.status >= 400 {
                (deps.err)(&error_of(&r));
                return 1;
            }
            (deps.out)(&format!("{name} removed, along with any grants it held"));
            0
        }

        other => {
            (deps.err)(&format!(
                "unknown verb \"{other}\" — one of {}\n{USAGE}",
                VERBS.join(", ")
            ));
            2
        }
    }
}

fn state_word(s: State) -> &'static str {
    match s {
        State::Ok => "ok",
        State::Bad => "bad",
        State::Unknown => "unknown",
    }
}

/// The registry's names, sorted — the order every row-producing verb walks.
fn server_names(status: &Value) -> Vec<String> {
    let mut names: Vec<String> = status["registry"]["servers"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// `--timeout 180` prints as `180`, not `180.0` — the message is product surface.
fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The real process: loopback HTTP, stdout/stderr, the environment, a real clock.
pub fn real_deps() -> McpDeps {
    let client = reqwest::Client::new();
    McpDeps {
        fetch: Arc::new(move |req: McpRequest| {
            let client = client.clone();
            Box::pin(async move {
                let method = reqwest::Method::from_bytes(req.method.as_bytes())
                    .map_err(|e| e.to_string())?;
                let mut b = client.request(method, &req.url);
                if let Some(body) = req.body {
                    b = b.header("content-type", "application/json").body(body);
                }
                let res = b.send().await.map_err(|e| e.to_string())?;
                let status = res.status().as_u16();
                let text = res.text().await.map_err(|e| e.to_string())?;
                Ok(McpResponse { status, text })
            })
        }),
        out: Arc::new(|line| println!("{line}")),
        err: Arc::new(|line| eprintln!("{line}")),
        env: Arc::new(|name| std::env::var(name).ok()),
        sleep: Arc::new(|ms| Box::pin(tokio::time::sleep(std::time::Duration::from_millis(ms)))),
        // Only read when `call` asks for it, and only when no argv word carried the
        // arguments — a verb that blocked on stdin would hang every other invocation.
        stdin: Some(Arc::new(|| {
            Box::pin(async {
                use tokio::io::AsyncReadExt;
                let mut buf = String::new();
                let _ = tokio::io::stdin().read_to_string(&mut buf).await;
                buf
            })
        })),
        now: Arc::new(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        }),
    }
}

#[cfg(test)]
mod tests {
    //! Two harnesses, on purpose.
    //!
    //! The verbs whose routes have landed are driven end to end against the REAL
    //! route table (`create_handler` over an in-memory database — the actual
    //! dispatcher, the actual error sentences, no socket bound), because a test
    //! against a hand-written fake asserts that the mock matches the mock.
    //!
    //! The rest are driven against a scripted transport whose shapes come from
    //! `specs/mcp.md` §2 — the same thing the TS suite does for `auth`, `call` and
    //! `doctor`, since those need a server that is authorized, connected or broken on
    //! command. When row 3.3's registry routes land, those move to the real table too.

    use super::*;
    use std::sync::Mutex;

    struct Fixture {
        deps: McpDeps,
        out: Arc<Mutex<String>>,
        err: Arc<Mutex<String>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Fixture {
        fn out(&self) -> String {
            self.out.lock().unwrap().clone()
        }
        fn err(&self) -> String {
            self.err.lock().unwrap().clone()
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    /// A transport scripted by `route`, plus recorded calls and captured output.
    fn scripted(
        route: impl Fn(&str, &str, Option<&str>) -> (u16, String) + Send + Sync + 'static,
    ) -> Fixture {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let out = Arc::new(Mutex::new(String::new()));
        let err = Arc::new(Mutex::new(String::new()));
        let recorded = calls.clone();
        let route = Arc::new(route);
        let fetch: McpFetch = Arc::new(move |req: McpRequest| {
            let recorded = recorded.clone();
            let route = route.clone();
            Box::pin(async move {
                let path = req
                    .url
                    .find("://")
                    .and_then(|i| req.url[i + 3..].find('/').map(|j| &req.url[i + 3 + j..]))
                    .unwrap_or("/");
                let bare = path.split('?').next().unwrap_or(path);
                recorded
                    .lock()
                    .unwrap()
                    .push(format!("{} {}", req.method, bare));
                let (status, text) = route(&req.method, path, req.body.as_deref());
                Ok(McpResponse { status, text })
            })
        });
        let o = out.clone();
        let e = err.clone();
        Fixture {
            deps: McpDeps {
                fetch,
                out: Arc::new(move |l| {
                    let mut s = o.lock().unwrap();
                    s.push_str(l);
                    s.push('\n');
                }),
                err: Arc::new(move |l| {
                    let mut s = e.lock().unwrap();
                    s.push_str(l);
                    s.push('\n');
                }),
                env: Arc::new(|n| (n == "BOUGH_PORT").then(|| "4321".to_string())),
                // No real waiting: the auth poll would otherwise cost a second per
                // iteration.
                sleep: Arc::new(|_| Box::pin(async {})),
                stdin: None,
                now: Arc::new(|| 0),
            },
            out,
            err,
            calls,
        }
    }

    /// The four-key status document, as `specs/mcp.md` §2 pins it.
    fn status_doc(servers: Value, active: Vec<&str>, auth: Value, connections: Value) -> String {
        json!({
            "registry": { "servers": servers },
            "auth": auth,
            "active": active,
            "connections": connections,
        })
        .to_string()
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    // ---- parsing ----------------------------------------------------------

    #[test]
    fn parsing_is_pure_and_total_and_bare_mcp_is_list() {
        // The question people arrive with is "what have I got", so making them type
        // the verb is friction over the common case.
        match parse_mcp_args(&[]) {
            Parsed::Args(a) => assert_eq!(a.verb, "list"),
            other => panic!("{other:?}"),
        }
        match parse_mcp_args(&argv(&["doctor", "--json", "--port", "5000"])) {
            Parsed::Args(a) => {
                assert_eq!(
                    (a.verb.as_str(), a.json, a.port),
                    ("doctor", true, Some(5000))
                );
            }
            other => panic!("{other:?}"),
        }
        // A verb that acts on a server must name one — the alternative is a command
        // that silently acts on whichever server sorts first.
        for verb in ["test", "auth", "logout", "grant", "revoke", "remove"] {
            match parse_mcp_args(&argv(&[verb])) {
                Parsed::UsageError(m) => assert!(m.contains("needs a server name"), "{verb}: {m}"),
                other => panic!("{verb}: {other:?}"),
            }
        }
        match parse_mcp_args(&argv(&["add", "notion"])) {
            Parsed::UsageError(m) => assert!(m.contains("needs a name and a URL"), "{m}"),
            other => panic!("{other:?}"),
        }
        let msg = |p: Parsed| match p {
            Parsed::UsageError(m) => m,
            other => panic!("{other:?}"),
        };
        assert!(msg(parse_mcp_args(&argv(&["wat"]))).contains("unknown verb \"wat\""));
        assert!(msg(parse_mcp_args(&argv(&["--nope"]))).contains("unknown flag"));
        assert!(msg(parse_mcp_args(&argv(&["--port"]))).contains("needs a value"));
        assert!(msg(parse_mcp_args(&argv(&["--port", "x"]))).contains("positive"));
        assert_eq!(msg(parse_mcp_args(&argv(&["--help"]))), USAGE);
    }

    // ---- list -------------------------------------------------------------

    #[tokio::test]
    async fn list_reports_each_servers_state_and_explains_its_glyphs() {
        let f = scripted(|_m, _p, _b| {
            (
                200,
                status_doc(
                    json!({ "notion": { "url": "https://mcp.notion.com/mcp" } }),
                    vec![],
                    json!({}),
                    json!([]),
                ),
            )
        });
        assert_eq!(run_mcp(&argv(&["list"]), &f.deps).await, 0);
        assert!(f.out().contains("○ notion"), "{}", f.out());
        assert!(f.out().contains("not granted"), "{}", f.out());
        // The legend, for the same reason the panel grew one: three marks carrying
        // the whole state of a row, documented nowhere, is how "it stays a half
        // circle" becomes a bug report instead of a glance.
        assert!(
            f.out()
                .contains("● connected · ◐ granted, not connected · ○ not granted"),
            "{}",
            f.out()
        );
    }

    #[tokio::test]
    async fn a_granted_but_unconnected_server_is_a_half_circle() {
        let f = scripted(|_m, _p, _b| {
            (
                200,
                status_doc(
                    json!({ "notion": { "url": "https://mcp.notion.com/mcp" } }),
                    vec!["notion"],
                    json!({ "notion": { "authorized": true } }),
                    json!([]),
                ),
            )
        });
        assert_eq!(run_mcp(&argv(&["list"]), &f.deps).await, 0);
        assert!(f.out().contains("◐ notion"), "{}", f.out());
        assert!(f.out().contains("granted · authed"), "{}", f.out());
    }

    #[tokio::test]
    async fn a_connected_server_is_a_full_circle_with_its_tool_count() {
        let f = scripted(|_m, _p, _b| {
            (
                200,
                status_doc(
                    json!({ "notion": { "url": "https://mcp.notion.com/mcp" } }),
                    vec!["notion"],
                    json!({}),
                    json!([{ "server": "notion", "alive": true, "toolCount": 3 }]),
                ),
            )
        });
        assert_eq!(run_mcp(&argv(&["list"]), &f.deps).await, 0);
        assert!(
            f.out().contains("● notion  granted · 3 tools"),
            "{}",
            f.out()
        );
    }

    #[tokio::test]
    async fn list_json_keeps_the_routes_key_order_rather_than_sorting_it() {
        // `--json` is what a script reads, and the TS client prints
        // `JSON.stringify(status, null, 2)` over the PARSED response — so its keys
        // come out in the order the route wrote them: registry, auth, active,
        // connections, and within an entry command before args. Printing the
        // `serde_json::Value` instead re-serializes a BTreeMap, which sorts every
        // key alphabetically; the document stays semantically equal and every
        // textual diff against the TS output fails. Caught by running both
        // clients against both servers on the real registry (G3).
        let f = scripted(|_m, _p, _b| {
            (
                200,
                status_doc(
                    json!({ "chrome-devtools": { "command": "npx", "args": ["chrome-devtools-mcp@latest"], "env": {}, "headers": {} } }),
                    vec![],
                    json!({}),
                    json!([]),
                ),
            )
        });
        assert_eq!(run_mcp(&argv(&["list", "--json"]), &f.deps).await, 0);
        let out = f.out();
        let key_order: Vec<&str> = ["registry", "auth", "active", "connections"]
            .into_iter()
            .filter(|k| out.contains(&format!("\"{k}\"")))
            .collect();
        assert_eq!(
            key_order,
            ["registry", "auth", "active", "connections"],
            "{out}"
        );
        let at = |k: &str| out.find(k).unwrap_or(usize::MAX);
        assert!(at("\"registry\"") < at("\"auth\""), "{out}");
        assert!(at("\"auth\"") < at("\"active\""), "{out}");
        assert!(at("\"active\"") < at("\"connections\""), "{out}");
        // Alphabetical would have put `args` first.
        assert!(at("\"command\"") < at("\"args\""), "{out}");
    }

    // ---- doctor -----------------------------------------------------------

    #[tokio::test]
    async fn doctor_sorts_the_causes_apart_and_exits_non_zero_when_one_needs_a_human() {
        // The whole point of the verb. A connect error alone does not tell you
        // whether the job is yours, Claude Code's, or nobody's.
        let f = scripted(|_m, _p, _b| {
            (
                200,
                status_doc(
                    json!({ "notion": { "url": "https://mcp.notion.com/mcp" } }),
                    vec![],
                    json!({}),
                    json!([]),
                ),
            )
        });
        assert_eq!(run_mcp(&argv(&["doctor"]), &f.deps).await, 1, "{}", f.out());
        assert!(f.out().contains("✗ notion"), "{}", f.out());
        // Ungranted is reported as ungranted, not as a credential problem.
        assert!(
            f.out().contains("not granted — bough mcp grant notion"),
            "{}",
            f.out()
        );
        assert!(f.out().contains("1 of 1 needs attention"), "{}", f.out());
        // …and it never spent a round trip on a connect it knew would be refused.
        assert!(
            !f.calls().iter().any(|c| c.ends_with("/connect")),
            "{:?}",
            f.calls()
        );
    }

    #[tokio::test]
    async fn doctor_names_the_credential_causes_apart() {
        for (error, want) in [
            (
                "the token in keychain item \"x\" expired at 2026-01-01",
                "its Claude Code grant expired",
            ),
            (
                "the keychain item \"x\" has no string at #mcpOAuth.a.b",
                "grant for it is empty",
            ),
        ] {
            let error = error.to_string();
            let f = scripted(move |method, path, _b| {
                if method == "POST" && path.contains("/connect") {
                    return (
                        200,
                        json!({ "server": "notion", "connected": false, "error": error })
                            .to_string(),
                    );
                }
                (
                    200,
                    status_doc(
                        json!({ "notion": { "url": "https://mcp.notion.com/mcp" } }),
                        vec!["notion"],
                        json!({ "notion": { "authorized": true } }),
                        json!([]),
                    ),
                )
            });
            assert_eq!(run_mcp(&argv(&["doctor"]), &f.deps).await, 1);
            assert!(f.out().contains(want), "{}", f.out());
        }
    }

    #[tokio::test]
    async fn a_local_server_is_not_told_to_run_an_oauth_flow_that_cannot_exist() {
        // `status.auth` is populated for `url` entries alone, so a stdio server always
        // reads as unauthorized — and "no credential — bough mcp auth bigquery" sends
        // someone to a flow a local command does not have.
        let f = scripted(|_m, _p, _b| {
            (
                200,
                status_doc(
                    json!({ "bigquery": { "command": "bq-mcp", "args": [] } }),
                    vec!["bigquery"],
                    json!({}),
                    json!([]),
                ),
            )
        });
        // UNTESTED IS NOT BROKEN. Counting it as a failure would make `doctor` exit 1
        // on a healthy setup, and the exit code is the part a script depends on.
        assert_eq!(run_mcp(&argv(&["doctor"]), &f.deps).await, 0, "{}", f.out());
        assert!(
            f.out().contains("local command — not tested"),
            "{}",
            f.out()
        );
        assert!(f.out().contains("--session ID"), "{}", f.out());
        assert!(!f.out().contains("bough mcp auth bigquery"), "{}", f.out());
        assert!(f.out().contains("not tested"), "{}", f.out());
        // …and it did not spend a round trip on a connect it knows will be refused.
        assert!(
            !f.calls().iter().any(|c| c.ends_with("/connect")),
            "{:?}",
            f.calls()
        );
    }

    #[tokio::test]
    async fn doctor_on_an_empty_registry_is_a_true_answer_not_a_failure() {
        let f = scripted(|_m, _p, _b| (200, status_doc(json!({}), vec![], json!({}), json!([]))));
        assert_eq!(run_mcp(&argv(&["doctor"]), &f.deps).await, 0);
        assert!(f.out().contains("no MCP servers registered"), "{}", f.out());
    }

    // ---- grant / revoke / add / remove -------------------------------------

    #[tokio::test]
    async fn grant_and_revoke_act_on_the_global_scope_and_say_so() {
        // A per-session grant is a thing the panel can offer because it has a session
        // on screen. A CLI does not, and inventing one would make the verb mean
        // something other than what it says.
        let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        let f = scripted(move |_m, _p, body| {
            if let Some(b) = body {
                seen.lock().unwrap().push(b.to_string());
            }
            (200, "{}".to_string())
        });
        assert_eq!(
            run_mcp(&argv(&["grant", "notion"]), &f.deps).await,
            0,
            "{}",
            f.err()
        );
        assert!(
            f.out().contains("granted in every conversation"),
            "{}",
            f.out()
        );
        assert_eq!(
            run_mcp(&argv(&["revoke", "notion"]), &f.deps).await,
            0,
            "{}",
            f.err()
        );
        assert!(f.out().contains("revoked everywhere"), "{}", f.out());
        assert_eq!(
            *bodies.lock().unwrap(),
            vec![
                r#"{"sessionId":""}"#.to_string(),
                r#"{"sessionId":""}"#.to_string()
            ]
        );
        assert_eq!(
            f.calls(),
            vec![
                "POST /mcp/servers/notion/enable".to_string(),
                "POST /mcp/servers/notion/disable".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn add_registers_and_points_at_the_next_step_remove_says_what_it_took() {
        let f = scripted(|_m, _p, _b| (200, "{}".to_string()));
        assert_eq!(
            run_mcp(
                &argv(&["add", "notion", "https://mcp.notion.com/mcp"]),
                &f.deps
            )
            .await,
            0
        );
        // The next step, named. "Registered" alone leaves you with a server that
        // cannot be called and no indication that two more verbs stand between you
        // and using it.
        assert!(
            f.out().contains("bough mcp auth notion, then grant it"),
            "{}",
            f.out()
        );
        assert_eq!(run_mcp(&argv(&["remove", "notion"]), &f.deps).await, 0);
        // The scope, out loud: removing an entry also revokes the grants it orphans.
        assert!(
            f.out().contains("along with any grants it held"),
            "{}",
            f.out()
        );
        assert_eq!(f.calls()[0], "PUT /mcp/servers/notion");
        assert_eq!(f.calls()[1], "DELETE /mcp/servers/notion");
    }

    #[tokio::test]
    async fn a_verb_naming_an_unregistered_server_fails_with_the_routes_own_sentence() {
        let f = scripted(|_m, _p, _b| {
            (
                404,
                json!({ "error": "no MCP server named \"ghost\" is registered." }).to_string(),
            )
        });
        assert_eq!(run_mcp(&argv(&["remove", "ghost"]), &f.deps).await, 1);
        assert!(
            f.err().contains("no MCP server named \"ghost\""),
            "{}",
            f.err()
        );
    }

    // ---- the server being down --------------------------------------------

    #[tokio::test]
    async fn no_server_on_the_port_is_exit_2_and_says_how_to_start_one() {
        // Distinct from exit 1 ON PURPOSE: "your MCP setup is broken" and "bough is
        // not running" are different problems, and a CI job branching on this needs
        // to tell them apart.
        let mut f = scripted(|_m, _p, _b| (200, "{}".into()));
        f.deps.fetch = Arc::new(|_req| Box::pin(async { Err("connection refused".to_string()) }));
        assert_eq!(run_mcp(&argv(&["list"]), &f.deps).await, 2);
        assert!(
            f.err().contains("no bough server at 127.0.0.1:4321"),
            "{}",
            f.err()
        );
        assert!(f.err().contains("bough start"), "{}", f.err());
    }

    // ---- auth --------------------------------------------------------------

    #[tokio::test]
    async fn auth_prints_the_url_rather_than_opening_a_browser_and_gives_up_out_loud() {
        // PRINTED, never opened: this client runs over SSH and in CI at least as
        // often as on a desktop, and shelling out to a browser hangs where there is
        // none.
        let f = scripted(|method, path, _b| {
            if method == "POST" && path.ends_with("/auth") {
                return (
                    200,
                    json!({ "status": "pending", "authorizationUrl": "https://auth.example/go" })
                        .to_string(),
                );
            }
            if method == "GET" && path.ends_with("/auth") {
                return (200, json!({ "authorized": false }).to_string());
            }
            (200, "{}".to_string())
        });
        // A deadline that has already passed by the second check, so the loop is
        // bounded by the clock rather than by how fast the fake answers.
        let tick = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let mut deps = f.deps;
        deps.now = Arc::new(move || tick.fetch_add(100_000, std::sync::atomic::Ordering::SeqCst));
        let code = run_mcp(&argv(&["auth", "notion", "--timeout", "1"]), &deps).await;
        assert_eq!(code, 1);
        assert!(f
            .out
            .lock()
            .unwrap()
            .contains("open this to authorize notion"));
        assert!(f.out.lock().unwrap().contains("https://auth.example/go"));
        assert!(
            f.err
                .lock()
                .unwrap()
                .contains("still waiting on the browser after 1s"),
            "{}",
            f.err.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn a_completed_authorization_connects_because_storing_tokens_moves_nothing() {
        // The bug this verb exists downstream of: authorization and connection are
        // different states, and a flow whose success is invisible reads as one that
        // failed.
        let f = scripted(|method, path, _b| {
            if method == "POST" && path.ends_with("/auth") {
                return (200, json!({ "status": "authorized" }).to_string());
            }
            if path.contains("/connect") {
                return (
                    200,
                    json!({ "server": "notion", "connected": true, "tools": [{ "name": "search" }] })
                        .to_string(),
                );
            }
            (
                200,
                status_doc(json!({}), vec!["notion"], json!({}), json!([])),
            )
        });
        assert_eq!(
            run_mcp(&argv(&["auth", "notion"]), &f.deps).await,
            0,
            "{}",
            f.err()
        );
        assert!(
            f.calls().iter().any(|c| c.ends_with("/connect")),
            "{:?}",
            f.calls()
        );
        assert!(
            f.out().contains("✓ notion connected · 1 tool"),
            "{}",
            f.out()
        );
    }

    #[tokio::test]
    async fn authorized_but_ungranted_says_the_last_step() {
        let f = scripted(|method, path, _b| {
            if method == "POST" && path.ends_with("/auth") {
                return (200, json!({ "status": "authorized" }).to_string());
            }
            if path.contains("/connect") {
                return (
                    200,
                    json!({ "server": "notion", "connected": true, "tools": [] }).to_string(),
                );
            }
            // Granted list is EMPTY — connected, authorized, and still uncallable.
            (200, status_doc(json!({}), vec![], json!({}), json!([])))
        });
        assert_eq!(
            run_mcp(&argv(&["auth", "notion"]), &f.deps).await,
            0,
            "{}",
            f.err()
        );
        assert!(
            f.out().contains("not granted yet — bough mcp grant notion"),
            "{}",
            f.out()
        );
    }

    #[tokio::test]
    async fn a_corrected_endpoint_is_said_out_loud() {
        // The registry was rewritten on the way through; a silent rewrite is a
        // surprise the next reader of the registry has to solve.
        let f = scripted(|method, path, _b| {
            if method == "POST" && path.ends_with("/auth") {
                return (
                    200,
                    json!({
                        "status": "redirect",
                        "authorizationUrl": "https://auth.example/go",
                        "correctedUrl": "https://mcp.linear.app/mcp",
                    })
                    .to_string(),
                );
            }
            if method == "GET" && path.ends_with("/auth") {
                return (200, json!({ "authorized": true }).to_string());
            }
            if path.contains("/connect") {
                return (200, json!({ "connected": true, "tools": [] }).to_string());
            }
            (
                200,
                status_doc(json!({}), vec!["linear"], json!({}), json!([])),
            )
        });
        let mut deps = f.deps;
        deps.now = Arc::new(|| 0);
        assert_eq!(run_mcp(&argv(&["auth", "linear"]), &deps).await, 0);
        assert!(
            f.out
                .lock()
                .unwrap()
                .contains("note: its endpoint was corrected to https://mcp.linear.app/mcp"),
            "{}",
            f.out.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn logout_states_the_scope_of_what_it_forgot() {
        let f = scripted(|_m, _p, _b| {
            (
                200,
                json!({ "server": "notion", "cleared": true }).to_string(),
            )
        });
        assert_eq!(run_mcp(&argv(&["logout", "notion"]), &f.deps).await, 0);
        assert!(
            f.out()
                .contains("forgot bough's credentials for notion — the registration is untouched"),
            "{}",
            f.out()
        );
        assert_eq!(
            f.calls(),
            vec!["DELETE /mcp/servers/notion/auth".to_string()]
        );
    }

    // ---- test / call --------------------------------------------------------

    #[tokio::test]
    async fn test_names_the_tools_it_found_and_a_refusal_is_exit_1() {
        let f = scripted(|_m, _p, _b| {
            (
                200,
                json!({ "connected": true, "tools": [{ "name": "search" }, { "name": "fetch" }] })
                    .to_string(),
            )
        });
        assert_eq!(run_mcp(&argv(&["test", "notion"]), &f.deps).await, 0);
        assert!(
            f.out().contains("✓ notion connected · 2 tools"),
            "{}",
            f.out()
        );
        assert!(f.out().contains("  search, fetch"), "{}", f.out());

        // A failed connect answers 200 with `connected: false` — "the request
        // succeeded, and 'this server is broken, here is why' is the answer it asked
        // for" — so the CLI, not the status code, decides the exit.
        let g = scripted(|_m, _p, _b| {
            (
                200,
                json!({ "connected": false, "error": "connection refused" }).to_string(),
            )
        });
        assert_eq!(run_mcp(&argv(&["test", "notion"]), &g.deps).await, 1);
        assert!(
            g.err()
                .contains("✗ notion did not connect — connection refused"),
            "{}",
            g.err()
        );
    }

    #[tokio::test]
    async fn call_rejects_malformed_arguments_before_it_reaches_the_server() {
        // Nothing should be spawned or connected to find out that a shell word was
        // not JSON.
        let f = scripted(|_m, _p, _b| (200, "{}".to_string()));
        assert_eq!(
            run_mcp(&argv(&["call", "notion", "search", "{not json"]), &f.deps).await,
            2
        );
        assert!(f.err().contains("not valid JSON"), "{}", f.err());
        assert!(f.calls().is_empty(), "{:?}", f.calls());
    }

    #[tokio::test]
    async fn call_prints_the_tools_own_result_and_relays_a_refusal_verbatim() {
        let f = scripted(|_m, path, _b| {
            if path.contains("/tools/") {
                return (
                    200,
                    json!({ "server": "notion", "tool": "search", "result": { "hits": 2 } })
                        .to_string(),
                );
            }
            (200, "{}".to_string())
        });
        assert_eq!(
            run_mcp(
                &argv(&["call", "notion", "search", "{\"q\":\"x\"}"]),
                &f.deps
            )
            .await,
            0,
            "{}",
            f.err()
        );
        // The RESULT, not the envelope: a program parses this and should not have to
        // dig its payload out of a wrapper it did not ask for.
        assert_eq!(f.out().trim(), "{\"hits\":2}");

        let g = scripted(|_m, _p, _b| {
            (
                403,
                json!({ "error": "\"notion\" is registered but not granted here." }).to_string(),
            )
        });
        assert_eq!(
            run_mcp(&argv(&["call", "notion", "search"]), &g.deps).await,
            1
        );
        assert!(
            g.err().contains("registered but not granted here"),
            "{}",
            g.err()
        );
    }

    #[tokio::test]
    async fn call_carries_the_turns_session_so_the_grant_enforced_is_the_callers() {
        // `$BOUGH_SESSION` is exported into every shell a turn spawns. That default
        // is what makes this behave like the host function it replaced: the model
        // does not know its own session id and is not trusted to report one.
        let seen: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let recorder = seen.clone();
        let f = scripted(move |_m, path, _b| {
            *recorder.lock().unwrap() = path.to_string();
            (200, json!({ "result": null }).to_string())
        });
        let mut deps = f.deps;
        deps.env = Arc::new(|n| match n {
            "BOUGH_PORT" => Some("4321".into()),
            "BOUGH_SESSION" => Some("sess-42".into()),
            _ => None,
        });
        run_mcp(&argv(&["call", "notion", "search"]), &deps).await;
        assert!(
            seen.lock().unwrap().contains("session=sess-42"),
            "{}",
            seen.lock().unwrap()
        );
        // An explicit --session still wins, for a human driving it by hand.
        run_mcp(
            &argv(&["call", "notion", "search", "--session", "other"]),
            &deps,
        )
        .await;
        assert!(
            seen.lock().unwrap().contains("session=other"),
            "{}",
            seen.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn call_reads_its_arguments_from_stdin_only_when_no_argv_word_carried_them() {
        // A verb that blocked on stdin would hang every other invocation.
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = bodies.clone();
        let f = scripted(move |_m, _p, body| {
            recorder
                .lock()
                .unwrap()
                .push(body.unwrap_or("").to_string());
            (200, json!({ "result": 1 }).to_string())
        });
        let counted = reads.clone();
        let mut deps = f.deps;
        deps.stdin = Some(Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { "  {\"q\":\"from stdin\"}  ".to_string() })
        }));
        run_mcp(
            &argv(&["call", "notion", "search", "{\"q\":\"argv\"}"]),
            &deps,
        )
        .await;
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "stdin must not be read"
        );
        run_mcp(&argv(&["call", "notion", "search"]), &deps).await;
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            *bodies.lock().unwrap(),
            vec![
                r#"{"q":"argv"}"#.to_string(),
                r#"{"q":"from stdin"}"#.to_string()
            ]
        );
    }

    // ---- against the REAL route table --------------------------------------

    /// ISOLATION IS `BOUGH_HOME`, AND IT IS NOT OPTIONAL. The registry is a file
    /// whose path derives from `paths.rs`, which reads the environment at call time
    /// — not from anything injectable. Without this the routes answer from the
    /// developer's own `~/.bough/mcp.json`, and the first version of this test did
    /// exactly that: it listed the machine's real servers. `deps.env` reaches the
    /// CLI's PORT lookup and nothing else.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    struct ScopedHome {
        prior: Option<String>,
        dir: std::path::PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl ScopedHome {
        fn new() -> ScopedHome {
            let guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir()
                .join(format!("bough-mcp-cli-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            let prior = std::env::var("BOUGH_HOME").ok();
            std::env::set_var("BOUGH_HOME", &dir);
            ScopedHome {
                prior,
                dir,
                _guard: guard,
            }
        }
    }

    impl Drop for ScopedHome {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var("BOUGH_HOME", v),
                None => std::env::remove_var("BOUGH_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The client's whole job is to speak to those routes, so at least the shape it
    /// parses must come from the dispatcher rather than from a fixture agreeing with
    /// itself. `GET /mcp/servers` and the OAuth verbs are wired today; the registry
    /// mutations join this test when row 3.3 lands them.
    #[tokio::test]
    async fn the_status_document_the_real_server_emits_is_the_one_list_reads() {
        let _home = ScopedHome::new();
        use bough_core::bus::Bus;
        use bough_core::db::sqlite_db::{DbOptions, SqliteDb};
        use bough_core::turn::queue::TurnRegistry;
        use bough_core::types::{system_clock, AppCtx, HostState, SharedDb};
        use bough_server::app::{create_handler, CreateHandlerOptions, Dispatcher};

        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let ctx = AppCtx {
            db,
            bus: Arc::new(Bus::new(system_clock())),
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now: system_clock(),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(std::sync::RwLock::new(None)),
            turn_registry: Arc::new(TurnRegistry::new()),
            model_defaults_path: Some(
                std::env::temp_dir()
                    .join(format!("bough-mcp-cli-{}", uuid::Uuid::new_v4()))
                    .join("model.json"),
            ),
        };
        let handler: Arc<Dispatcher> =
            Arc::new(create_handler(ctx, CreateHandlerOptions::default()));
        let out = Arc::new(Mutex::new(String::new()));
        let err = Arc::new(Mutex::new(String::new()));
        let (o, e) = (out.clone(), err.clone());
        let deps = McpDeps {
            fetch: Arc::new(move |req: McpRequest| {
                let handler = handler.clone();
                Box::pin(async move {
                    let url: reqwest::Url = req.url.parse().map_err(|e| format!("{e}"))?;
                    let target = match url.query() {
                        Some(q) => format!("{}?{}", url.path(), q),
                        None => url.path().to_string(),
                    };
                    let request = axum::extract::Request::builder()
                        .method(req.method.as_str())
                        .uri(target)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(req.body.unwrap_or_default()))
                        .expect("a well-formed request");
                    let res = handler.call(request).await;
                    let status = res.status().as_u16();
                    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(McpResponse {
                        status,
                        text: String::from_utf8_lossy(&bytes).into_owned(),
                    })
                })
            }),
            out: Arc::new(move |l| {
                let mut s = o.lock().unwrap();
                s.push_str(l);
                s.push('\n');
            }),
            err: Arc::new(move |l| {
                let mut s = e.lock().unwrap();
                s.push_str(l);
                s.push('\n');
            }),
            env: Arc::new(|n| (n == "BOUGH_PORT").then(|| "4321".to_string())),
            sleep: Arc::new(|_| Box::pin(async {})),
            stdin: None,
            now: Arc::new(|| 0),
        };

        // An empty registry: a true answer, not an error, and it names the two ways
        // out of it.
        assert_eq!(
            run_mcp(&argv(&["list"]), &deps).await,
            0,
            "{}",
            err.lock().unwrap()
        );
        let text = out.lock().unwrap().clone();
        assert!(text.contains("no MCP servers registered"), "{text}");
        assert!(text.contains("bough sync-mcp"), "{text}");

        // …and `logout` reaches the OAuth route this row wired, not a stub.
        out.lock().unwrap().clear();
        assert_eq!(
            run_mcp(&argv(&["logout", "ghost"]), &deps).await,
            0,
            "{}",
            err.lock().unwrap()
        );
        assert!(
            out.lock()
                .unwrap()
                .contains("forgot bough's credentials for ghost"),
            "{}",
            out.lock().unwrap()
        );
    }
}
