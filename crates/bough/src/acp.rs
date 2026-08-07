//! `bough acp` — bough as an [Agent Client Protocol] agent, so an ACP client
//! (Zed, an editor plugin, anything that speaks the protocol) can drive bough
//! sessions and subscribe to their output.
//!
//! [Agent Client Protocol]: https://agentclientprotocol.com/protocol/v1/overview
//!
//! ACP is JSON-RPC 2.0 as newline-delimited JSON on stdio. There is no
//! "subscribe" call: a client is subscribed to a session by virtue of having
//! asked for the turn — the agent pushes `session/update` notifications for the
//! duration of `session/prompt` and answers the request with a `stopReason`.
//!
//! This adapter is a TRANSLATOR, not a second agent. It holds no turn state: it
//! speaks to the same loopback HTTP API `bough exec` uses (`POST /sessions`,
//! `GET /events?sessionId=`, `POST /sessions/:id/messages`) and maps bough's
//! SSE events onto ACP notifications. Everything that decides what a turn does
//! stays server-side, which is why a client connected here sees exactly what
//! the TUI sees.
//!
//! Scope is the ACP baseline: `initialize`, `session/new`, `session/prompt`,
//! `session/cancel`, and `session/update`. `session/load`, authentication,
//! permission requests, `fs/*` and `terminal/*` are deliberately absent and are
//! refused as unsupported rather than half-answered.
//!
//! ## stdout is the protocol
//!
//! Nothing but framed JSON-RPC may reach stdout — one stray line and the client
//! drops the connection. Every diagnostic in this file goes to stderr.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::exec::{ExecFetch, ExecRequest, SseFrame, SseReader};

pub const USAGE: &str = "usage: bough acp

  Speak the Agent Client Protocol on stdin/stdout, so an ACP client can drive
  bough sessions and receive streaming session/update notifications.

  This is a client of the bough server, not a second server: start `bough`
  (or `bough start`) first. The port comes from BOUGH_PORT (default 4321).

  stdout carries the protocol and nothing else; diagnostics go to stderr.

programs run as you, with your authority — there is no sandbox.";

/// The one ACP major version this adapter implements.
const PROTOCOL_VERSION: i64 = 1;

const DEFAULT_PORT: u32 = 4321;

// JSON-RPC 2.0 error codes. `INTERNAL` covers "the server was there and then
// wasn't" — the client can retry the prompt; the others are its own mistakes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

// ---- the injected effects ---------------------------------------------------

/// Everything [`run_acp`] touches that is not a pure function.
///
/// `out` is shared by every in-flight prompt task, so it must serialize whole
/// lines — a half-written notification interleaved with another is a protocol
/// error, not a cosmetic one.
#[derive(Clone)]
pub struct AcpDeps {
    pub fetch: ExecFetch,
    /// One framed JSON-RPC message, newline included. stdout, and only this.
    pub out: Arc<dyn Fn(&str) + Send + Sync>,
    /// stderr. Diagnostics only.
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
    /// stdin as a stream of lines (terminators stripped).
    pub lines: Arc<dyn Fn() -> BoxStream<'static, Result<String, String>> + Send + Sync>,
    pub env: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

// ---- pure protocol shapes ---------------------------------------------------

fn response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A `session/update` notification for one session.
fn session_update(session_id: &str, update: Value) -> Value {
    notification(
        "session/update",
        json!({ "sessionId": session_id, "update": update }),
    )
}

/// The `initialize` result. Capabilities are stated as they are: this adapter
/// advertises none of the optional ones, because a client that trusts a
/// capability we then refuse is worse off than one that never asked.
pub fn initialize_result(client_version: Option<i64>) -> Value {
    // Version negotiation: echo the client's version if we support it,
    // otherwise answer with ours and let the client decide to disconnect.
    let negotiated = match client_version {
        Some(v) if v == PROTOCOL_VERSION => v,
        _ => PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": negotiated,
        "agentCapabilities": {
            "loadSession": false,
            "promptCapabilities": {
                "image": false,
                "audio": false,
                "embeddedContext": true
            }
        },
        "agentInfo": {
            "name": "bough",
            "title": "bough",
            "version": env!("CARGO_PKG_VERSION")
        },
        "authMethods": []
    })
}

/// Flatten an ACP prompt into the single string `POST /sessions/:id/messages`
/// takes.
///
/// bough's message API is text; ACP's prompt is a content-block list. Text and
/// embedded resources fold in directly, and a `resource_link` folds in as its
/// URI — the agent can open it, which is the whole point of the link. Image and
/// audio blocks are REFUSED rather than dropped: the capabilities say we do not
/// take them, and silently discarding the one block the user cared about is the
/// failure this returns an error to avoid.
pub fn prompt_to_text(blocks: &[Value]) -> Result<String, String> {
    let mut chunks: Vec<String> = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !text.is_empty() {
                    chunks.push(text.to_string());
                }
            }
            Some("resource_link") => {
                let uri = block.get("uri").and_then(Value::as_str).unwrap_or_default();
                if !uri.is_empty() {
                    chunks.push(uri.to_string());
                }
            }
            Some("resource") => {
                let resource = block.get("resource");
                let uri = resource
                    .and_then(|r| r.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = resource
                    .and_then(|r| r.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if text.is_empty() {
                    // A blob resource has no text side; the URI is what survives.
                    if !uri.is_empty() {
                        chunks.push(uri.to_string());
                    }
                } else if uri.is_empty() {
                    chunks.push(text.to_string());
                } else {
                    chunks.push(format!("{uri}:\n{text}"));
                }
            }
            Some(other) => {
                return Err(format!(
                    "bough does not accept {other} content in a prompt (see promptCapabilities)"
                ));
            }
            None => return Err("a prompt content block has no type".to_string()),
        }
    }
    let joined = chunks.join("\n\n");
    if joined.trim().is_empty() {
        return Err("the prompt is empty".to_string());
    }
    Ok(joined)
}

/// `turn.finished`'s status as an ACP `StopReason`.
///
/// `error` maps to `refusal` because ACP has no "the turn broke" reason and
/// `end_turn` would claim the answer is complete. The message itself is
/// reported separately, as text, so the reason is never the only thing the
/// user gets.
pub fn stop_reason_for(status: &str) -> &'static str {
    match status {
        "done" => "end_turn",
        "interrupted" => "cancelled",
        _ => "refusal",
    }
}

/// bough's two tools, as ACP tool-call kinds. `run_steps` executes a program;
/// `stop` is the model declaring the turn finished.
fn tool_kind(name: &str) -> &'static str {
    match name {
        "run_steps" => "execute",
        _ => "other",
    }
}

/// A one-line label for a tool card. For `run_steps` that is the program's
/// first meaningful line — "run_steps" repeated down a transcript tells the
/// reader nothing about what is running.
fn tool_title(name: &str, input: &Value) -> String {
    if name == "run_steps" {
        if let Some(code) = input.get("code").and_then(Value::as_str) {
            if let Some(line) = code
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with("//"))
            {
                return take_chars(line, 80);
            }
        }
    }
    name.to_string()
}

/// Tool output as display text. Strings pass through; anything else is its JSON.
fn render_output(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn take_chars(value: &str, n: usize) -> String {
    value.chars().take(n).collect()
}

/// What one bough SSE frame becomes on the ACP wire.
///
/// Split out from the loop because this mapping is the whole feature and it is
/// worth being able to test a frame at a time. Returns the notifications to
/// send, plus the turn's stop reason once `turn.finished` lands.
pub struct Mapped {
    pub notifications: Vec<Value>,
    /// `Some` exactly on `turn.finished` — the signal to answer the request.
    pub finished: Option<Finished>,
    /// A pending `ask()` this client cannot answer, to be declined by the caller.
    pub declined_question: Option<(String, String, String)>,
}

pub struct Finished {
    pub stop_reason: &'static str,
    pub error: Option<String>,
}

/// Map one frame. `session_id` is the ACP session the notifications belong to.
pub fn map_frame(session_id: &str, frame: &SseFrame) -> Mapped {
    let mut out = Mapped {
        notifications: Vec::new(),
        finished: None,
        declined_question: None,
    };
    let data = frame.data.get("data").cloned().unwrap_or(Value::Null);
    match frame.name.as_str() {
        "message.delta" => {
            let delta = data
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if delta.is_empty() {
                return out;
            }
            let mut update = json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": delta }
            });
            if let Some(id) = data.get("messageId").and_then(Value::as_str) {
                update["messageId"] = json!(id);
            }
            out.notifications.push(session_update(session_id, update));
        }
        "message.part" => {
            let part = data.get("part").cloned().unwrap_or(Value::Null);
            let message_id = data.get("messageId").and_then(Value::as_str);
            match part.get("type").and_then(Value::as_str) {
                // Prose already arrived delta by delta. Sending the finalized
                // Text part too would duplicate the entire answer in the
                // client's transcript.
                Some("text") => {}
                Some("reasoning") => {
                    let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                    if text.is_empty() {
                        return out;
                    }
                    let mut update = json!({
                        "sessionUpdate": "agent_thought_chunk",
                        "content": { "type": "text", "text": text }
                    });
                    if let Some(id) = message_id {
                        update["messageId"] = json!(id);
                    }
                    out.notifications.push(session_update(session_id, update));
                }
                Some("tool_call") => {
                    let id = part.get("id").and_then(Value::as_str).unwrap_or_default();
                    let name = part.get("name").and_then(Value::as_str).unwrap_or_default();
                    let input = part.get("input").cloned().unwrap_or(Value::Null);
                    // Reported `in_progress`, not `pending`: bough asks for no
                    // permission here, so by the time the part exists the call
                    // is already running.
                    out.notifications.push(session_update(
                        session_id,
                        json!({
                            "sessionUpdate": "tool_call",
                            "toolCallId": id,
                            "title": tool_title(name, &input),
                            "kind": tool_kind(name),
                            "status": "in_progress",
                            "rawInput": input
                        }),
                    ));
                }
                Some("tool_result") => {
                    let call_id = part
                        .get("callId")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let is_error = part
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let interrupted = part
                        .get("interrupted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let text = render_output(part.get("output").unwrap_or(&Value::Null));
                    let mut update = json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": call_id,
                        "status": if is_error || interrupted { "failed" } else { "completed" }
                    });
                    if !text.is_empty() {
                        update["content"] = json!([{
                            "type": "content",
                            "content": { "type": "text", "text": text }
                        }]);
                    }
                    out.notifications.push(session_update(session_id, update));
                }
                _ => {}
            }
        }
        "ask.question" => {
            // NOBODY IS HERE TO ANSWER — this adapter does not implement
            // `session/request_permission`, so a hold would sit until the
            // client gave up on a turn that was one answer from finishing.
            // Declining is the documented dismissal, exactly as `bough exec`
            // does it, and the question is surfaced as text so the user knows
            // what was refused on their behalf.
            if data.get("status").and_then(Value::as_str) != Some("pending") {
                return out;
            }
            let question = data
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id = data.get("id").and_then(Value::as_str).unwrap_or_default();
            let qsid = data
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or(session_id);
            out.notifications.push(session_update(
                session_id,
                json!({
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": format!(
                            "\n[declined a question — this ACP client cannot answer one: {}]\n",
                            take_chars(question.lines().next().unwrap_or_default(), 120)
                        )
                    }
                }),
            ));
            out.declined_question = Some((qsid.to_string(), id.to_string(), question.to_string()));
        }
        "turn.finished" => {
            let status = data
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("done")
                .to_string();
            let error = data
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(message) = &error {
                out.notifications.push(session_update(
                    session_id,
                    json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": format!("\n[turn {status}: {message}]\n") }
                    }),
                ));
            }
            out.finished = Some(Finished {
                stop_reason: stop_reason_for(&status),
                error,
            });
        }
        _ => {}
    }
    out
}

// ---- the run ----------------------------------------------------------------

/// Resolve the API base from `BOUGH_PORT`, the same way `bough exec` does.
fn api_base(deps: &AcpDeps) -> Result<String, String> {
    let raw = (deps.env)("BOUGH_PORT");
    let port: u32 = match &raw {
        None => DEFAULT_PORT,
        Some(text) => match text.trim().parse::<u32>() {
            Ok(n) if (1..=65535).contains(&n) => n,
            _ => return Err(format!("BOUGH_PORT is not a port number: {text}")),
        },
    };
    Ok(format!("http://127.0.0.1:{port}"))
}

/// State the connection carries between messages. Small on purpose: the server
/// owns sessions, this only remembers that the client was told about one, so a
/// prompt for a session it never created is refused instead of silently
/// creating work in a session the client cannot see.
#[derive(Default)]
struct Connection {
    initialized: bool,
    sessions: HashMap<String, ()>,
}

/// The whole adapter. Returns the process exit code; never exits, never touches
/// a real stream, never reads a global.
pub async fn run_acp(argv: &[String], deps: &AcpDeps) -> i32 {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        (deps.out)(&format!("{USAGE}\n"));
        return 0;
    }
    if let Some(unexpected) = argv.iter().find(|a| !a.is_empty()) {
        (deps.warn)(&format!(
            "bough acp takes no arguments (got \"{unexpected}\").\n{USAGE}"
        ));
        return 2;
    }

    let api = match api_base(deps) {
        Ok(api) => api,
        Err(message) => {
            (deps.warn)(&message);
            return 2;
        }
    };

    let state = Arc::new(Mutex::new(Connection::default()));
    let mut lines = (deps.lines)();
    // Prompt turns run concurrently with the read loop — that is what makes
    // `session/cancel` reachable at all, since it arrives on the same stdin
    // while the prompt it cancels is still open.
    let mut turns: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    while let Some(next) = lines.next().await {
        let line = match next {
            Ok(line) => line,
            Err(message) => {
                (deps.warn)(&format!("stdin failed: {message}"));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                // A parse error has no id to answer against; JSON-RPC says to
                // reply with a null id rather than stay silent.
                (deps.out)(&framed(&error_response(
                    &Value::Null,
                    PARSE_ERROR,
                    &format!("not JSON: {err}"),
                )));
                continue;
            }
        };
        let id = message.get("id").cloned();
        let method = match message.get("method").and_then(Value::as_str) {
            Some(method) => method.to_string(),
            // A response to a request we never sent. Ignored, per JSON-RPC.
            None => continue,
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match method.as_str() {
            "initialize" => {
                let Some(id) = &id else { continue };
                let client_version = params.get("protocolVersion").and_then(Value::as_i64);
                state.lock().expect("acp state").initialized = true;
                (deps.out)(&framed(&response(id, initialize_result(client_version))));
            }
            "session/new" => {
                let Some(id) = &id else { continue };
                if !state.lock().expect("acp state").initialized {
                    (deps.out)(&framed(&error_response(
                        id,
                        INVALID_REQUEST,
                        "initialize first",
                    )));
                    continue;
                }
                let cwd = params
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if cwd.is_empty() {
                    (deps.out)(&framed(&error_response(
                        id,
                        INVALID_PARAMS,
                        "session/new needs an absolute cwd",
                    )));
                    continue;
                }
                // bough connects to the MCP servers in its OWN registry
                // (`bough mcp`), not to a per-session list. Saying so beats
                // accepting the field and quietly ignoring it.
                if params
                    .get("mcpServers")
                    .and_then(Value::as_array)
                    .is_some_and(|servers| !servers.is_empty())
                {
                    (deps.warn)(
                        "ignoring session/new mcpServers — bough uses its own MCP registry (see `bough mcp`)",
                    );
                }
                match create_session(&api, cwd, deps).await {
                    Ok(session_id) => {
                        state
                            .lock()
                            .expect("acp state")
                            .sessions
                            .insert(session_id.clone(), ());
                        (deps.out)(&framed(&response(id, json!({ "sessionId": session_id }))));
                    }
                    Err(message) => {
                        (deps.out)(&framed(&error_response(id, INTERNAL_ERROR, &message)));
                    }
                }
            }
            "session/prompt" => {
                let Some(id) = &id else { continue };
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !state
                    .lock()
                    .expect("acp state")
                    .sessions
                    .contains_key(&session_id)
                {
                    (deps.out)(&framed(&error_response(
                        id,
                        INVALID_PARAMS,
                        "unknown sessionId — call session/new first",
                    )));
                    continue;
                }
                let blocks = params
                    .get("prompt")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let text = match prompt_to_text(&blocks) {
                    Ok(text) => text,
                    Err(message) => {
                        (deps.out)(&framed(&error_response(id, INVALID_PARAMS, &message)));
                        continue;
                    }
                };
                let deps = deps.clone();
                let api = api.clone();
                let id = id.clone();
                turns.push(tokio::spawn(async move {
                    run_prompt(&api, &session_id, &text, &id, &deps).await;
                }));
            }
            "session/cancel" => {
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if session_id.is_empty() {
                    continue;
                }
                // Fire and forget: the prompt task answers the request when the
                // interrupt lands as `turn.finished { status: interrupted }`,
                // which is the only place a stopReason may come from.
                let deps = deps.clone();
                let api = api.clone();
                tokio::spawn(async move {
                    let _ = (deps.fetch)(ExecRequest {
                        method: "POST".into(),
                        url: format!("{api}/sessions/{}/interrupt", encode_component(&session_id)),
                        body: None,
                    })
                    .await;
                });
            }
            // Refused, not faked. Each of these has a capability that says
            // `false`, so a conforming client never gets here.
            "authenticate" | "session/load" | "session/resume" | "session/close"
            | "session/delete" | "session/set_mode" | "logout" => {
                if let Some(id) = &id {
                    (deps.out)(&framed(&error_response(
                        id,
                        METHOD_NOT_FOUND,
                        &format!("bough does not support {method}"),
                    )));
                }
            }
            other => {
                if let Some(id) = &id {
                    (deps.out)(&framed(&error_response(
                        id,
                        METHOD_NOT_FOUND,
                        &format!("unknown method {other}"),
                    )));
                }
            }
        }
    }

    // stdin closed: the client is gone. Let the turns already answering finish
    // writing rather than truncating a transcript mid-notification.
    for turn in turns {
        let _ = turn.await;
    }
    0
}

/// One framed message: compact JSON on a line of its own.
fn framed(message: &Value) -> String {
    format!("{}\n", serde_json::to_string(message).unwrap_or_default())
}

async fn create_session(api: &str, cwd: &str, deps: &AcpDeps) -> Result<String, String> {
    let body = json!({ "title": "acp", "workspace": cwd });
    let res = (deps.fetch)(ExecRequest {
        method: "POST".into(),
        url: format!("{api}/sessions"),
        body: Some(body.to_string()),
    })
    .await
    .map_err(|message| format!("cannot reach bough ({message}) — is the server running?"))?;
    if !res.ok() {
        let status = res.status;
        let text = res.text().await;
        return Err(format!(
            "bough refused the session: {status} {}",
            text.trim()
        ));
    }
    let text = res.text().await;
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| "bough refused the session: the response carried no id".to_string())
}

/// Run one prompt turn: stream the session's events as `session/update`
/// notifications, then answer the `session/prompt` request with a stop reason.
async fn run_prompt(api: &str, session_id: &str, text: &str, id: &Value, deps: &AcpDeps) {
    // THE ORDERING, inherited from `bough exec`: the stream is opened, and its
    // bus subscription is live, before the prompt exists server-side. A turn
    // that finishes inside the POST is already queued on this stream by the
    // time it is read.
    let mut events = match (deps.fetch)(ExecRequest {
        method: "GET".into(),
        url: format!("{api}/events?sessionId={}", encode_component(session_id)),
        body: None,
    })
    .await
    {
        Ok(res) if res.ok() => res,
        Ok(res) => {
            (deps.out)(&framed(&error_response(
                id,
                INTERNAL_ERROR,
                &format!("bough refused the event stream: {}", res.status),
            )));
            return;
        }
        Err(message) => {
            (deps.out)(&framed(&error_response(
                id,
                INTERNAL_ERROR,
                &format!("cannot open the bough event stream ({message})"),
            )));
            return;
        }
    };

    match (deps.fetch)(ExecRequest {
        method: "POST".into(),
        url: format!("{api}/sessions/{}/messages", encode_component(session_id)),
        body: Some(json!({ "text": text }).to_string()),
    })
    .await
    {
        Ok(res) if res.ok() => {}
        Ok(res) => {
            let status = res.status;
            let body = res.text().await;
            (deps.out)(&framed(&error_response(
                id,
                INTERNAL_ERROR,
                &format!("bough refused the message: {status} {}", body.trim()),
            )));
            return;
        }
        Err(message) => {
            (deps.out)(&framed(&error_response(
                id,
                INTERNAL_ERROR,
                &format!("cannot post the prompt to bough ({message})"),
            )));
            return;
        }
    }

    let mut feed = SseReader::new();
    let mut finished: Option<Finished> = None;
    'outer: while let Some(next) = events.chunks.next().await {
        let Ok(chunk) = next else { break };
        for frame in feed.push(&chunk) {
            let mapped = map_frame(session_id, &frame);
            for note in &mapped.notifications {
                (deps.out)(&framed(note));
            }
            if let Some((qsid, qid, _)) = mapped.declined_question {
                let deps = deps.clone();
                let api = api.to_string();
                tokio::spawn(async move {
                    let _ = (deps.fetch)(ExecRequest {
                        method: "POST".into(),
                        url: format!(
                            "{api}/sessions/{}/questions/{}",
                            encode_component(&qsid),
                            encode_component(&qid)
                        ),
                        body: Some(json!({ "decline": true }).to_string()),
                    })
                    .await;
                });
            }
            if let Some(end) = mapped.finished {
                finished = Some(end);
                break 'outer;
            }
        }
    }
    drop(events);

    match finished {
        Some(end) => {
            if let Some(message) = &end.error {
                (deps.warn)(&format!("turn ended: {message}"));
            }
            (deps.out)(&framed(&response(
                id,
                json!({ "stopReason": end.stop_reason }),
            )));
        }
        None => {
            // The stream died before the turn ended. Answering `end_turn` here
            // would claim a complete answer we never saw, so this is an error —
            // the turn may still be running server-side, which the message says.
            (deps.out)(&framed(&error_response(
                id,
                INTERNAL_ERROR,
                "the bough event stream closed before the turn finished",
            )));
        }
    }
}

/// `encodeURIComponent` for the ids that go into a URL path or query.
fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
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
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---- the process ------------------------------------------------------------

/// The real process, wired up once. The only impure thing in this file.
pub fn real_deps() -> AcpDeps {
    let exec = crate::exec::real_deps();
    AcpDeps {
        fetch: exec.fetch.clone(),
        out: Arc::new(|text: &str| {
            use std::io::Write;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = lock.write_all(text.as_bytes());
            // Flushed per message: a client blocks on the response, so a
            // buffered reply is a hang, not a delay.
            let _ = lock.flush();
        }),
        warn: Arc::new(|text: &str| eprintln!("{text}")),
        lines: Arc::new(|| {
            use tokio::io::AsyncBufReadExt;
            let reader = tokio::io::BufReader::new(tokio::io::stdin());
            Box::pin(futures::stream::unfold(
                reader.lines(),
                |mut lines| async move {
                    match lines.next_line().await {
                        Ok(Some(line)) => Some((Ok(line), lines)),
                        Ok(None) => None,
                        Err(err) => Some((Err(err.to_string()), lines)),
                    }
                },
            )) as BoxStream<'static, Result<String, String>>
        }),
        env: exec.env.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{ExecFuture, ExecResponse};
    use std::sync::Mutex as StdMutex;

    fn frame(name: &str, data: Value) -> SseFrame {
        SseFrame {
            name: name.to_string(),
            // The SSE payload is the whole stamped envelope, not the bare data.
            data: json!({ "type": name, "sessionId": "s1", "seq": 1, "ts": 0, "data": data }),
        }
    }

    #[test]
    fn initialize_advertises_only_what_is_implemented() {
        let result = initialize_result(Some(1));
        assert_eq!(result["protocolVersion"], json!(1));
        assert_eq!(result["agentCapabilities"]["loadSession"], json!(false));
        assert_eq!(result["agentInfo"]["name"], json!("bough"));
        assert_eq!(result["authMethods"], json!([]));
    }

    #[test]
    fn an_unsupported_client_version_still_gets_our_version() {
        assert_eq!(initialize_result(Some(99))["protocolVersion"], json!(1));
        assert_eq!(initialize_result(None)["protocolVersion"], json!(1));
    }

    #[test]
    fn text_resource_links_and_embedded_resources_flatten_into_the_prompt() {
        let blocks = vec![
            json!({ "type": "text", "text": "fix this" }),
            json!({ "type": "resource_link", "uri": "file:///a/b.rs" }),
            json!({ "type": "resource", "resource": { "uri": "file:///c.rs", "text": "fn c() {}" } }),
        ];
        let text = prompt_to_text(&blocks).unwrap();
        assert_eq!(
            text,
            "fix this\n\nfile:///a/b.rs\n\nfile:///c.rs:\nfn c() {}"
        );
    }

    // The capability says image: false. Dropping the block would answer a
    // different question than the one asked.
    #[test]
    fn an_image_block_is_refused_rather_than_dropped() {
        let blocks = vec![json!({ "type": "image", "data": "…", "mimeType": "image/png" })];
        let message = prompt_to_text(&blocks).unwrap_err();
        assert!(message.contains("image"), "{message}");
        assert!(prompt_to_text(&[]).is_err());
    }

    #[test]
    fn deltas_become_agent_message_chunks() {
        let mapped = map_frame(
            "s1",
            &frame("message.delta", json!({ "messageId": "m1", "delta": "hi" })),
        );
        assert_eq!(mapped.notifications.len(), 1);
        let note = &mapped.notifications[0];
        assert_eq!(note["method"], json!("session/update"));
        assert_eq!(note["params"]["sessionId"], json!("s1"));
        assert_eq!(
            note["params"]["update"]["sessionUpdate"],
            json!("agent_message_chunk")
        );
        assert_eq!(note["params"]["update"]["content"]["text"], json!("hi"));
        assert_eq!(note["params"]["update"]["messageId"], json!("m1"));
    }

    // The finalized Text part repeats what the deltas already carried. Sending
    // both duplicates the entire answer in the client's transcript.
    #[test]
    fn the_finalized_text_part_is_not_sent_again() {
        let mapped = map_frame(
            "s1",
            &frame(
                "message.part",
                json!({ "messageId": "m1", "part": { "type": "text", "text": "hi" } }),
            ),
        );
        assert!(mapped.notifications.is_empty());
    }

    #[test]
    fn reasoning_becomes_a_thought_chunk() {
        let mapped = map_frame(
            "s1",
            &frame(
                "message.part",
                json!({ "messageId": "m1", "part": { "type": "reasoning", "text": "hmm" } }),
            ),
        );
        assert_eq!(
            mapped.notifications[0]["params"]["update"]["sessionUpdate"],
            json!("agent_thought_chunk")
        );
    }

    #[test]
    fn a_run_steps_call_is_titled_by_its_first_real_line() {
        let mapped = map_frame(
            "s1",
            &frame(
                "message.part",
                json!({
                    "messageId": "m1",
                    "part": {
                        "type": "tool_call",
                        "id": "c1",
                        "name": "run_steps",
                        "input": { "code": "// a comment\n\nawait bash('ls')" }
                    }
                }),
            ),
        );
        let update = &mapped.notifications[0]["params"]["update"];
        assert_eq!(update["sessionUpdate"], json!("tool_call"));
        assert_eq!(update["toolCallId"], json!("c1"));
        assert_eq!(update["kind"], json!("execute"));
        assert_eq!(update["status"], json!("in_progress"));
        assert_eq!(update["title"], json!("await bash('ls')"));
        assert_eq!(
            update["rawInput"]["code"],
            json!("// a comment\n\nawait bash('ls')")
        );
    }

    #[test]
    fn a_tool_result_completes_or_fails_the_call() {
        let done = map_frame(
            "s1",
            &frame(
                "message.part",
                json!({
                    "messageId": "m1",
                    "part": { "type": "tool_result", "callId": "c1", "output": "ok", "isError": false }
                }),
            ),
        );
        let update = &done.notifications[0]["params"]["update"];
        assert_eq!(update["sessionUpdate"], json!("tool_call_update"));
        assert_eq!(update["toolCallId"], json!("c1"));
        assert_eq!(update["status"], json!("completed"));
        assert_eq!(update["content"][0]["content"]["text"], json!("ok"));

        let failed = map_frame(
            "s1",
            &frame(
                "message.part",
                json!({
                    "messageId": "m1",
                    "part": { "type": "tool_result", "callId": "c1", "output": "boom", "isError": true }
                }),
            ),
        );
        assert_eq!(
            failed.notifications[0]["params"]["update"]["status"],
            json!("failed")
        );

        // An interrupted call did not complete either, however clean its output.
        let stopped = map_frame(
            "s1",
            &frame(
                "message.part",
                json!({
                    "messageId": "m1",
                    "part": { "type": "tool_result", "callId": "c1", "output": "half", "isError": false, "interrupted": true }
                }),
            ),
        );
        assert_eq!(
            stopped.notifications[0]["params"]["update"]["status"],
            json!("failed")
        );
    }

    #[test]
    fn turn_status_becomes_a_stop_reason() {
        assert_eq!(stop_reason_for("done"), "end_turn");
        assert_eq!(stop_reason_for("interrupted"), "cancelled");
        assert_eq!(stop_reason_for("error"), "refusal");
        assert_eq!(stop_reason_for("orphaned"), "refusal");
    }

    // The reason alone does not say what broke; the message has to travel too.
    #[test]
    fn an_errored_turn_reports_its_message_as_text() {
        let mapped = map_frame(
            "s1",
            &frame(
                "turn.finished",
                json!({ "turnId": "t1", "sessionId": "s1", "status": "error", "error": "context overflow" }),
            ),
        );
        assert_eq!(mapped.finished.as_ref().unwrap().stop_reason, "refusal");
        let text = mapped.notifications[0]["params"]["update"]["content"]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("context overflow"), "{text}");
    }

    // A pending ask() would park the turn forever under a client that has no
    // way to answer it.
    #[test]
    fn a_pending_question_is_declined_and_announced() {
        let mapped = map_frame(
            "s1",
            &frame(
                "ask.question",
                json!({ "id": "q1", "sessionId": "s1", "question": "ship it?", "status": "pending" }),
            ),
        );
        let (sid, qid, _) = mapped.declined_question.expect("declined");
        assert_eq!((sid.as_str(), qid.as_str()), ("s1", "q1"));
        assert!(
            mapped.notifications[0]["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap()
                .contains("ship it?")
        );

        let settled = map_frame(
            "s1",
            &frame(
                "ask.question",
                json!({ "id": "q1", "sessionId": "s1", "question": "ship it?", "status": "answered" }),
            ),
        );
        assert!(settled.declined_question.is_none());
    }

    // ---- the loop, over a fake server ---------------------------------------

    /// A transport that answers the three endpoints a turn touches, with a
    /// canned SSE body. Records every request so the ordering can be asserted.
    fn fake_server(sse: &'static str) -> (ExecFetch, Arc<StdMutex<Vec<String>>>) {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let recorder = seen.clone();
        let fetch: ExecFetch = Arc::new(move |req: ExecRequest| {
            recorder
                .lock()
                .unwrap()
                .push(format!("{} {}", req.method, req.url));
            let body = if req.url.contains("/events") {
                sse.to_string()
            } else if req.url.ends_with("/sessions") {
                json!({ "id": "s1" }).to_string()
            } else {
                "{}".to_string()
            };
            Box::pin(async move {
                Ok(ExecResponse {
                    status: 200,
                    chunks: Box::pin(futures::stream::once(async move { Ok(body) })),
                })
            }) as ExecFuture
        });
        (fetch, seen)
    }

    fn deps_over(
        fetch: ExecFetch,
        input: Vec<String>,
    ) -> (AcpDeps, Arc<StdMutex<String>>, Arc<StdMutex<Vec<String>>>) {
        let out = Arc::new(StdMutex::new(String::new()));
        let warn = Arc::new(StdMutex::new(Vec::new()));
        let sink = out.clone();
        let warns = warn.clone();
        let deps = AcpDeps {
            fetch,
            out: Arc::new(move |text: &str| sink.lock().unwrap().push_str(text)),
            warn: Arc::new(move |text: &str| warns.lock().unwrap().push(text.to_string())),
            lines: Arc::new(move || {
                Box::pin(futures::stream::iter(
                    input.clone().into_iter().map(Ok).collect::<Vec<_>>(),
                )) as BoxStream<'static, Result<String, String>>
            }),
            env: Arc::new(|_| None),
        };
        (deps, out, warn)
    }

    fn sent(out: &Arc<StdMutex<String>>) -> Vec<Value> {
        out.lock()
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line is JSON"))
            .collect()
    }

    const TURN: &str = concat!(
        "event: message.delta\n",
        "data: {\"type\":\"message.delta\",\"seq\":1,\"ts\":0,\"data\":{\"messageId\":\"m1\",\"delta\":\"hello\"}}\n",
        "\n",
        "event: turn.finished\n",
        "data: {\"type\":\"turn.finished\",\"seq\":2,\"ts\":0,\"data\":{\"turnId\":\"t1\",\"sessionId\":\"s1\",\"status\":\"done\"}}\n",
        "\n",
    );

    #[tokio::test]
    async fn a_whole_turn_initializes_creates_streams_and_stops() {
        let (fetch, seen) = fake_server(TURN);
        let (deps, out, _) = deps_over(
            fetch,
            vec![
                json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1}})
                    .to_string(),
                json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp"}})
                    .to_string(),
                json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{
                    "sessionId":"s1",
                    "prompt":[{"type":"text","text":"hi"}]
                }})
                .to_string(),
            ],
        );
        assert_eq!(run_acp(&[], &deps).await, 0);

        let messages = sent(&out);
        assert_eq!(messages[0]["result"]["protocolVersion"], json!(1));
        assert_eq!(messages[1]["result"]["sessionId"], json!("s1"));
        assert_eq!(
            messages[2]["params"]["update"]["content"]["text"],
            json!("hello")
        );
        assert_eq!(messages[3]["id"], json!(2));
        assert_eq!(messages[3]["result"]["stopReason"], json!("end_turn"));

        // THE ORDERING: the event stream is subscribed before the prompt is
        // posted, or a turn that finishes fast is never seen at all.
        let seen = seen.lock().unwrap().clone();
        let events = seen.iter().position(|r| r.contains("/events")).unwrap();
        let post = seen.iter().position(|r| r.contains("/messages")).unwrap();
        assert!(events < post, "{seen:?}");
    }

    #[tokio::test]
    async fn a_prompt_for_an_unknown_session_is_refused() {
        let (fetch, _) = fake_server(TURN);
        let (deps, out, _) = deps_over(
            fetch,
            vec![
                json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1}})
                    .to_string(),
                json!({"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{
                    "sessionId":"nope","prompt":[{"type":"text","text":"hi"}]
                }})
                .to_string(),
            ],
        );
        run_acp(&[], &deps).await;
        let messages = sent(&out);
        assert_eq!(messages[1]["error"]["code"], json!(INVALID_PARAMS));
    }

    #[tokio::test]
    async fn session_new_before_initialize_is_refused() {
        let (fetch, _) = fake_server(TURN);
        let (deps, out, _) = deps_over(
            fetch,
            vec![
                json!({"jsonrpc":"2.0","id":0,"method":"session/new","params":{"cwd":"/tmp"}})
                    .to_string(),
            ],
        );
        run_acp(&[], &deps).await;
        assert_eq!(sent(&out)[0]["error"]["code"], json!(INVALID_REQUEST));
    }

    // Unsupported is said out loud. A client that gets silence hangs.
    #[tokio::test]
    async fn unsupported_methods_answer_with_method_not_found() {
        let (fetch, _) = fake_server(TURN);
        let (deps, out, _) = deps_over(
            fetch,
            vec![
                json!({"jsonrpc":"2.0","id":0,"method":"session/load","params":{}}).to_string(),
                json!({"jsonrpc":"2.0","id":1,"method":"nonsense","params":{}}).to_string(),
                "not json at all".to_string(),
            ],
        );
        run_acp(&[], &deps).await;
        let messages = sent(&out);
        assert_eq!(messages[0]["error"]["code"], json!(METHOD_NOT_FOUND));
        assert_eq!(messages[1]["error"]["code"], json!(METHOD_NOT_FOUND));
        assert_eq!(messages[2]["error"]["code"], json!(PARSE_ERROR));
        assert_eq!(messages[2]["id"], Value::Null);
    }

    // A notification carries no id; answering one is a protocol error.
    #[tokio::test]
    async fn a_cancel_notification_interrupts_and_answers_nothing() {
        let (fetch, seen) = fake_server(TURN);
        let (deps, out, _) = deps_over(
            fetch,
            vec![
                json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}})
                    .to_string(),
            ],
        );
        run_acp(&[], &deps).await;
        // The interrupt is spawned; give it a tick to land.
        tokio::task::yield_now().await;
        assert!(out.lock().unwrap().is_empty());
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|r| r.contains("/interrupt")),
            "{:?}",
            seen.lock().unwrap()
        );
    }

    // Claiming `end_turn` on a dead stream would report an answer we never saw.
    #[tokio::test]
    async fn a_stream_that_dies_mid_turn_is_an_error_not_an_end_turn() {
        let (fetch, _) =
            fake_server("event: message.delta\ndata: {\"data\":{\"delta\":\"x\"}}\n\n");
        let (deps, out, _) = deps_over(
            fetch,
            vec![
                json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1}})
                    .to_string(),
                json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp"}})
                    .to_string(),
                json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{
                    "sessionId":"s1","prompt":[{"type":"text","text":"hi"}]
                }})
                .to_string(),
            ],
        );
        run_acp(&[], &deps).await;
        let messages = sent(&out);
        let last = messages.last().unwrap();
        assert_eq!(last["id"], json!(2));
        assert_eq!(last["error"]["code"], json!(INTERNAL_ERROR));
    }

    #[tokio::test]
    async fn help_is_stdout_and_a_stray_argument_is_a_usage_error() {
        let (fetch, _) = fake_server(TURN);
        let (deps, out, warn) = deps_over(fetch.clone(), vec![]);
        assert_eq!(run_acp(&["--help".to_string()], &deps).await, 0);
        assert!(out.lock().unwrap().contains("usage: bough acp"));
        assert!(warn.lock().unwrap().is_empty());

        let (deps, out, warn) = deps_over(fetch, vec![]);
        assert_eq!(run_acp(&["oops".to_string()], &deps).await, 2);
        assert!(out.lock().unwrap().is_empty());
        assert!(warn.lock().unwrap()[0].contains("takes no arguments"));
    }
}
