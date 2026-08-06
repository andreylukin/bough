//! The TUI's typed HTTP client (port of `src/tui/api.ts`, wave-1 subset).
//!
//! THE INVARIANT THIS HOLDS: **no component talks HTTP, and no URL is written
//! twice.** Every server route reachable from the TUI is a method here; a
//! renderer that wants data takes it from the store, never from a raw request.
//!
//! Second invariant: **the client never re-declares a wire shape it can
//! import.** Response types are `bough_core::schema` types wherever they exist;
//! the shapes the server assembles inline in its handlers (`SessionRow`,
//! `SessionSnapshot`, …) are declared here beside the method that reads them,
//! field names verbatim.
//!
//! Third, and the reason [`Api::new`] takes a base and a fetch fn:
//! **everything is injected.** A test points the client at a fake fetch and
//! never touches the real port or the user's `~/.bough`.
//!
//! Wave-1 scope (PORT_PLAN 1.32 + tui-core.md §8): sessions / messages /
//! snapshot / usage / interrupt / draft, questions, jobs, model settings, and
//! the URL builders. The dropped families (workflows, MCP, artifacts, search,
//! theme, ghost) are later-wave additions to this same struct.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use bough_core::schema::parts::{
    AskQuestion, BackgroundJob, Message, Session, TurnStatus,
};
use bough_core::schema::requests::{
    CreateSessionBody, PatchSessionBody, PostMessageBody, PutModelSettingsBody,
};
use bough_core::types::{Effort, UsageTotals};

// ---- where the server is ----------------------------------------------------

pub const DEFAULT_PORT: u16 = 4321;

/// The loopback origin the server binds. `BOUGH_PORT` is how the rewrite runs
/// beside the live install, so it is read here rather than hard-coded — and an
/// unset variable degrades to the default rather than failing the client.
pub fn default_base() -> String {
    let port = std::env::var("BOUGH_PORT").ok().filter(|p| !p.is_empty());
    base_for(port.as_deref())
}

/// The pure half of [`default_base`], so tests need not race on the env.
pub fn base_for(port: Option<&str>) -> String {
    match port {
        Some(p) => format!("http://127.0.0.1:{p}"),
        None => format!("http://127.0.0.1:{DEFAULT_PORT}"),
    }
}

// ---- errors -----------------------------------------------------------------

/// What a request can fail with. Two real cases, exactly as the TS client has
/// two error classes — the remedy differs and the TUI branches on it.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ApiFailure {
    /// A request that reached the server and came back non-2xx. `message` is
    /// the server's own `{error}` text whenever there is one, because that
    /// text is a product surface: "select turns from this conversation" is an
    /// answer, and `POST /sessions/x/compact: 400` is not.
    #[error("{message}")]
    Api {
        status: u16,
        message: String,
        method: String,
        path: String,
    },
    /// The server could not be reached at all. The COMMAND comes before the
    /// address, because this line is rendered into a one-row notice that
    /// truncates: what is worth losing to a narrow terminal is the URL.
    #[error("bough server unreachable — run: bough start · {base}")]
    Offline { base: String, cause: String },
    /// The server answered 2xx with a body this client could not decode —
    /// a shape drift, surfaced rather than cast blindly.
    #[error("{method} {path}: unexpected response shape: {message}")]
    Decode {
        method: String,
        path: String,
        message: String,
    },
}

impl ApiFailure {
    pub fn is_offline(&self) -> bool {
        matches!(self, ApiFailure::Offline { .. })
    }
    /// HTTP status for the `Api` case, `None` otherwise.
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiFailure::Api { status, .. } => Some(*status),
            _ => None,
        }
    }
}

// ---- the injected transport --------------------------------------------------

/// One raw HTTP exchange, as the fetch seam sees it.
#[derive(Clone, Debug)]
pub struct FetchRequest {
    pub method: String,
    pub url: String,
    /// JSON text. The transport sets `content-type: application/json` iff set.
    pub body: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub type FetchFuture = Pin<Box<dyn Future<Output = Result<HttpResponse, String>> + Send>>;
/// The seam tests fake. The `Err` string is the transport's own failure text
/// (connection refused, …) — it becomes [`ApiFailure::Offline`]'s cause.
pub type FetchFn = Arc<dyn Fn(FetchRequest) -> FetchFuture + Send + Sync>;

/// The production transport: reqwest over loopback, no TLS, no timeout beyond
/// reqwest's defaults (the server answers or the connection refuses).
fn reqwest_fetch() -> FetchFn {
    let client = reqwest::Client::new();
    Arc::new(move |req: FetchRequest| {
        let client = client.clone();
        Box::pin(async move {
            let method = reqwest::Method::from_bytes(req.method.as_bytes())
                .map_err(|e| e.to_string())?;
            let mut builder = client.request(method, &req.url);
            if let Some(body) = req.body {
                builder = builder
                    .header("content-type", "application/json")
                    .body(body);
            }
            let res = builder.send().await.map_err(|e| e.to_string())?;
            let status = res.status().as_u16();
            let body = res.text().await.map_err(|e| e.to_string())?;
            Ok(HttpResponse { status, body })
        })
    })
}

// ---- shapes the server assembles inline -------------------------------------

/// A row of `GET /sessions`. Mirrors the server's `SessionListItem` — the three
/// extras are DERIVED server-side at read time; none is a column. Optional
/// fields absent from an older server degrade, never break.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    #[serde(flatten)]
    pub session: Session,
    /// A turn is in flight right now. Live-updated from events after this read.
    pub busy: bool,
    /// How the most recent turn ended. Absent when the session never ran one.
    #[serde(default)]
    pub last_turn_status: Option<TurnStatus>,
    /// This session's own spend. Omitted when zero.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// This session's own tokens (input + output + reasoning). Omitted when zero.
    #[serde(default)]
    pub tokens: Option<i64>,
}

/// One injected `AGENTS.md`, as `GET /sessions/:id` reports it.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRuleSummary {
    /// Workspace-relative where it sits inside the workspace, else absolute.
    pub label: String,
    pub path: String,
    /// Characters that went into the prompt — what the change note compares.
    pub bytes: i64,
}

/// The `usage` object of a snapshot: totals plus the tree rollup, flattened.
#[derive(Deserialize, Clone, Debug)]
pub struct SnapshotUsage {
    #[serde(flatten)]
    pub totals: UsageTotals,
    /// This session plus every branch collapsed under it.
    pub tree: UsageTotals,
}

/// `GET /sessions/:id` — the reconnect payload.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session: Session,
    /// Ancestors root→parent, then own. Assembled, never stored.
    pub thread: Vec<Message>,
    pub usage: SnapshotUsage,
    /// The model the next turn will actually call — what the meter names.
    /// Absent from a server older than the field.
    #[serde(default)]
    pub effective_model: Option<String>,
    /// The effective model's context window. `None`/null = unknown.
    #[serde(default)]
    pub context_limit: Option<i64>,
    /// Ranked command-history tags, rendered as the dim `#` row. Absent or
    /// empty both render as nothing.
    #[serde(default)]
    pub primed_tags: Option<Vec<String>>,
    /// The `AGENTS.md` files the next turn will inject, in prompt order.
    #[serde(default)]
    pub project_rules: Option<Vec<ProjectRuleSummary>>,
}

/// `GET /sessions/:id/usage` — the spend meter, live between rounds.
#[derive(Deserialize, Clone, Debug)]
pub struct SessionUsage {
    pub usage: UsageTotals,
    /// This session plus every branch collapsed under it.
    pub tree: UsageTotals,
}

/// `GET /model-settings` — what a NEW conversation runs on, both tiers.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettings {
    pub default_model: String,
    /// The cheap tier's model. Null only if the install genuinely has none.
    #[serde(default)]
    pub cheap_model: Option<String>,
    /// The default thinking depth, or null for "the provider decides".
    #[serde(default)]
    pub default_effort: Option<Effort>,
}

/// `POST /sessions/:id/messages` — 202. `queued` = a turn was already running.
#[derive(Deserialize, Clone, Debug)]
pub struct PostedMessage {
    pub message: Message,
    pub queued: bool,
}

/// `PUT /sessions/:id/draft` — `null` clears the prefilled composer text.
#[derive(Deserialize, Clone, Debug)]
pub struct DraftResult {
    pub ok: bool,
    pub draft: Option<String>,
}

/// `POST /sessions/:id/interrupt`. Always resolves for a session that exists —
/// `interrupted: false` is the answer when the turn had already ended, so the
/// caller needs no race-condition branch for a button whose job is to be safe
/// to press.
#[derive(Deserialize, Clone, Debug)]
pub struct InterruptResult {
    pub interrupted: bool,
    #[serde(default)]
    pub message: Option<String>,
}

/// `POST /sessions/:sid/questions/:qid` acknowledgement (answer or decline).
#[derive(Deserialize, Clone, Debug)]
pub struct QuestionAck {
    pub ok: bool,
    pub id: String,
    pub status: String,
}

/// A row of `GET /sessions/:id/jobs`: the job plus a short, non-destructive
/// tail of what it printed — what the transcript's job card renders.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JobListRow {
    #[serde(flatten)]
    pub job: BackgroundJob,
    #[serde(default)]
    pub tail: Option<Vec<String>>,
    #[serde(default)]
    pub output_lines: Option<i64>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct JobsList {
    pub jobs: Vec<JobListRow>,
}

/// `POST /sessions/:id/jobs` — the user's own `!command`.
#[derive(Deserialize, Clone, Debug)]
pub struct RunShellAck {
    pub id: String,
    pub name: String,
    pub pid: i64,
}

/// `POST /sessions/:id/jobs/:jobId/kill`.
#[derive(Deserialize, Clone, Debug)]
pub struct KillAck {
    pub message: String,
}

/// `GET /sessions/:id/jobs/:jobId/output` — the whole retained buffer,
/// non-destructively (never moves the model's cursor).
#[derive(Deserialize, Clone, Debug)]
pub struct JobOutput {
    pub output: String,
    pub job: BackgroundJob,
}

// ---- URL helpers ------------------------------------------------------------

/// `application/x-www-form-urlencoded` component encoding, matching
/// `URLSearchParams`: `[A-Za-z0-9*\-._]` verbatim, space → `+`, rest `%XX`.
fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Query-string builder that omits absent values, so no URL ends in a bare `?`.
fn query(params: &[(&str, Option<&str>)]) -> String {
    let mut pairs: Vec<String> = Vec::new();
    for (key, value) in params {
        if let Some(v) = value {
            if !v.is_empty() {
                pairs.push(format!("{}={}", encode_query(key), encode_query(v)));
            }
        }
    }
    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    }
}

/// Percent-encode one path segment, matching `encodeURIComponent`:
/// `[A-Za-z0-9\-_.!~*'()]` verbatim, everything else `%XX` (UTF-8 bytes).
fn seg(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*'
            | b'\'' | b'(' | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---- the client -------------------------------------------------------------

#[derive(Default)]
pub struct ApiOptions {
    /// Absent = [`default_base`].
    pub base: Option<String>,
    /// Absent = the reqwest transport. Injected by tests.
    pub fetch_fn: Option<FetchFn>,
}

/// The one client. Cheap to clone; the transport is shared.
#[derive(Clone)]
pub struct Api {
    base: String,
    fetch: FetchFn,
}

impl Api {
    pub fn new(options: ApiOptions) -> Self {
        Api {
            base: options.base.unwrap_or_else(default_base),
            fetch: options.fetch_fn.unwrap_or_else(reqwest_fetch),
        }
    }

    /// The origin every path below is relative to. Read by `events.rs`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// `GET /events[?sessionId=]`. Built here so the SSE client owns no URL either.
    pub fn events_url(&self, session_id: Option<&str>) -> String {
        format!("{}/events{}", self.base, query(&[("sessionId", session_id)]))
    }

    /// Same-origin link to a published artifact — what the agent prints for the
    /// user. Path separators inside the name survive; the segments around them
    /// do not get to smuggle one in.
    pub fn artifact_url(&self, session_id: &str, name: &str) -> String {
        let path: Vec<String> = name.split('/').map(seg).collect();
        format!(
            "{}/artifacts/{}/{}",
            self.base,
            seg(session_id),
            path.join("/")
        )
    }

    /// One request. Every method goes through here, which is what makes "a dead
    /// server says so in one sentence" a property of the client rather than of
    /// each call site.
    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<HttpResponse, ApiFailure> {
        let body_text = match body {
            Some(v) => Some(serde_json::to_string(&v).map_err(|e| ApiFailure::Decode {
                method: method.to_string(),
                path: path.to_string(),
                message: e.to_string(),
            })?),
            None => None,
        };
        let req = FetchRequest {
            method: method.to_string(),
            url: format!("{}{}", self.base, path),
            body: body_text,
        };
        (self.fetch)(req).await.map_err(|cause| ApiFailure::Offline {
            base: self.base.clone(),
            cause,
        })
    }

    /// Request → parsed JSON, with the server's `{error}` message preserved on
    /// failure. There is deliberately only ONE of these — a second funnel that
    /// drops the server's sentence is how "select turns from this conversation"
    /// once turned into "400".
    async fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, ApiFailure> {
        let res = self.send(method, path, body).await?;
        let text = res.body;
        let parsed: Option<Value> = if text.is_empty() {
            None
        } else {
            serde_json::from_str(&text).ok()
        };
        if !(200..300).contains(&res.status) {
            let message = parsed
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|e| e.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        format!("{method} {path}: {}", res.status)
                    } else {
                        trimmed.to_string()
                    }
                });
            return Err(ApiFailure::Api {
                status: res.status,
                message,
                method: method.to_string(),
                path: path.to_string(),
            });
        }
        serde_json::from_value(parsed.unwrap_or(Value::Null)).map_err(|e| ApiFailure::Decode {
            method: method.to_string(),
            path: path.to_string(),
            message: e.to_string(),
        })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiFailure> {
        self.json("GET", path, None).await
    }
    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, ApiFailure> {
        self.json("POST", path, body).await
    }
    async fn put<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, ApiFailure> {
        self.json("PUT", path, body).await
    }
    async fn patch<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, ApiFailure> {
        self.json("PATCH", path, body).await
    }

    // -- preflight ------------------------------------------------------------

    /// One `listSessions`, before the screen is taken. On failure the caller
    /// prints `bough tui: <message>` to stderr and exits 2 — a sentence, not a
    /// stack trace.
    pub async fn preflight(&self) -> Result<(), ApiFailure> {
        self.list_sessions(None).await.map(|_| ())
    }

    // -- sessions and messages ------------------------------------------------

    /// Top level, collapsed kinds excluded. With `origin_id`: the drill-in.
    pub async fn list_sessions(
        &self,
        origin_id: Option<&str>,
    ) -> Result<Vec<SessionRow>, ApiFailure> {
        self.get(&format!("/sessions{}", query(&[("originId", origin_id)])))
            .await
    }

    pub async fn create_session(&self, body: &CreateSessionBody) -> Result<Session, ApiFailure> {
        self.post("/sessions", Some(to_value(body)?)).await
    }

    /// The reconnect fetch: `{session, thread, usage}`, reconciled by message id.
    pub async fn get_session(&self, id: &str) -> Result<SessionSnapshot, ApiFailure> {
        self.get(&format!("/sessions/{}", seg(id))).await
    }

    /// The per-session `model`/`effort` pin. Absent field = leave alone,
    /// explicit `null` = clear the pin; they are different requests.
    pub async fn patch_session(
        &self,
        id: &str,
        body: &PatchSessionBody,
    ) -> Result<Session, ApiFailure> {
        self.patch(&format!("/sessions/{}", seg(id)), Some(to_value(body)?))
            .await
    }

    /// Usage without the thread — cheap enough to poll while a turn runs.
    pub async fn session_usage(&self, id: &str) -> Result<SessionUsage, ApiFailure> {
        self.get(&format!("/sessions/{}/usage", seg(id))).await
    }

    pub async fn post_message(
        &self,
        id: &str,
        body: &PostMessageBody,
    ) -> Result<PostedMessage, ApiFailure> {
        self.post(
            &format!("/sessions/{}/messages", seg(id)),
            Some(to_value(body)?),
        )
        .await
    }

    /// `None` clears the prefilled composer text. No event — the writer is this
    /// client.
    pub async fn put_draft(
        &self,
        id: &str,
        draft: Option<&str>,
    ) -> Result<DraftResult, ApiFailure> {
        self.put(
            &format!("/sessions/{}/draft", seg(id)),
            Some(serde_json::json!({ "draft": draft })),
        )
        .await
    }

    /// Stop the running turn. The response says whether there was one; the turn
    /// actually ending arrives as `turn.finished` on the stream.
    pub async fn interrupt(&self, id: &str) -> Result<InterruptResult, ApiFailure> {
        self.post(&format!("/sessions/{}/interrupt", seg(id)), None)
            .await
    }

    // -- ask() holds ----------------------------------------------------------

    /// Memory-only server-side, so this is how a freshly-attached client
    /// rebuilds the card.
    pub async fn list_questions(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<AskQuestion>, ApiFailure> {
        self.get(&format!("/questions{}", query(&[("sessionId", session_id)])))
            .await
    }

    pub async fn answer_question(
        &self,
        session_id: &str,
        qid: &str,
        answer: &str,
    ) -> Result<QuestionAck, ApiFailure> {
        self.post(
            &format!("/sessions/{}/questions/{}", seg(session_id), seg(qid)),
            Some(serde_json::json!({ "answer": answer })),
        )
        .await
    }

    /// The program's `ask()` rejects catchably with "user declined".
    pub async fn decline_question(
        &self,
        session_id: &str,
        qid: &str,
    ) -> Result<QuestionAck, ApiFailure> {
        self.post(
            &format!("/sessions/{}/questions/{}", seg(session_id), seg(qid)),
            Some(serde_json::json!({ "decline": true })),
        )
        .await
    }

    // -- background jobs ------------------------------------------------------

    /// The session AND its subagents — the work running on its behalf. Rows
    /// carry a short non-destructive `tail`, which is what the cards render.
    pub async fn list_jobs(&self, id: &str) -> Result<JobsList, ApiFailure> {
        self.get(&format!("/sessions/{}/jobs", seg(id))).await
    }

    /// The user's own `!command` — a background shell, not a turn.
    pub async fn run_shell(&self, id: &str, command: &str) -> Result<RunShellAck, ApiFailure> {
        self.post(
            &format!("/sessions/{}/jobs", seg(id)),
            Some(serde_json::json!({ "command": command })),
        )
        .await
    }

    /// The human's kill switch, so stopping a runaway shell costs no LLM round-trip.
    pub async fn kill_job(&self, id: &str, job_id: &str) -> Result<KillAck, ApiFailure> {
        self.post(
            &format!("/sessions/{}/jobs/{}/kill", seg(id), seg(job_id)),
            None,
        )
        .await
    }

    pub async fn job_output(&self, id: &str, job_id: &str) -> Result<JobOutput, ApiFailure> {
        self.get(&format!(
            "/sessions/{}/jobs/{}/output",
            seg(id),
            seg(job_id)
        ))
        .await
    }

    // -- model settings -------------------------------------------------------

    /// What a NEW conversation runs on, for the picker's ● before any session
    /// exists.
    pub async fn get_model_settings(&self) -> Result<ModelSettings, ApiFailure> {
        self.get("/model-settings").await
    }

    /// Pin what a NEW conversation runs on, for the whole install. Absent field
    /// = leave alone, explicit `null` = unpin.
    pub async fn put_model_settings(
        &self,
        body: &PutModelSettingsBody,
    ) -> Result<ModelSettings, ApiFailure> {
        self.put("/model-settings", Some(to_value(body)?)).await
    }
}

fn to_value<T: serde::Serialize>(body: &T) -> Result<Value, ApiFailure> {
    serde_json::to_value(body).map_err(|e| ApiFailure::Decode {
        method: String::new(),
        path: String::new(),
        message: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A fake transport: records every request, answers from a script.
    fn scripted(
        responses: Vec<Result<HttpResponse, String>>,
    ) -> (FetchFn, Arc<Mutex<Vec<FetchRequest>>>) {
        let seen: Arc<Mutex<Vec<FetchRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(responses));
        let seen2 = seen.clone();
        let fetch: FetchFn = Arc::new(move |req| {
            seen2.lock().unwrap().push(req);
            let next = queue.lock().unwrap().remove(0);
            Box::pin(async move { next })
        });
        (fetch, seen)
    }

    fn api_with(fetch: FetchFn) -> Api {
        Api::new(ApiOptions {
            base: Some("http://127.0.0.1:4321".into()),
            fetch_fn: Some(fetch),
        })
    }

    #[test]
    fn the_base_tracks_bough_port_because_the_rewrite_runs_beside_the_live_install() {
        assert_eq!(base_for(Some("4399")), "http://127.0.0.1:4399");
        assert_eq!(base_for(None), "http://127.0.0.1:4321");
    }

    #[tokio::test]
    async fn a_dead_server_says_so_in_one_sentence_with_the_command_that_fixes_it() {
        let (fetch, _seen) = scripted(vec![Err(
            "error sending request: Connection refused".into()
        )]);
        let api = api_with(fetch);
        let err = api.list_sessions(None).await.unwrap_err();
        assert!(err.is_offline());
        let message = err.to_string();
        assert!(message.contains("unreachable"), "{message}");
        assert!(message.contains("http://127.0.0.1:4321"), "{message}");
        // The REMEDY must come before the address: this line is rendered into a
        // one-row notice that truncates, and with the command last an 80-column
        // terminal clipped it to "bough st…" — the only part that mattered.
        assert!(
            message.find("bough start").unwrap() < message.find("127.0.0.1").unwrap(),
            "the command must precede the address: {message}"
        );
        assert!(message.len() <= 80, "too long for one row: {message}");
    }

    #[tokio::test]
    async fn a_server_error_arrives_as_its_own_sentence_not_as_a_status_code() {
        let (fetch, _seen) = scripted(vec![Ok(HttpResponse {
            status: 404,
            body: r#"{"error":"session nope not found"}"#.into(),
        })]);
        let api = api_with(fetch);
        let err = api.get_session("nope").await.unwrap_err();
        match &err {
            ApiFailure::Api { status, message, .. } => {
                assert_eq!(*status, 404);
                // Error text is a product surface: the message names the id.
                assert!(message.contains("session nope not found"), "{message}");
            }
            other => panic!("expected ApiFailure::Api, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_bodyless_or_non_json_failure_still_yields_a_readable_message() {
        // Non-JSON body: the trimmed text is the message.
        let (fetch, _) = scripted(vec![Ok(HttpResponse {
            status: 500,
            body: "  boom  ".into(),
        })]);
        let err = api_with(fetch).list_sessions(None).await.unwrap_err();
        assert_eq!(err.to_string(), "boom");
        // Empty body: METHOD path: status.
        let (fetch, _) = scripted(vec![Ok(HttpResponse {
            status: 500,
            body: String::new(),
        })]);
        let err = api_with(fetch).list_sessions(None).await.unwrap_err();
        assert_eq!(err.to_string(), "GET /sessions: 500");
    }

    #[test]
    fn urls_are_built_in_one_place_and_segments_are_encoded() {
        let api = api_with(scripted(vec![]).0);
        assert_eq!(api.events_url(None), "http://127.0.0.1:4321/events");
        assert_eq!(
            api.events_url(Some("a b")),
            "http://127.0.0.1:4321/events?sessionId=a+b"
        );
        // Path separators inside an artifact name survive; the segments around
        // them do not get to smuggle one in.
        assert_eq!(
            api.artifact_url("s 1", "assets/app js.html"),
            "http://127.0.0.1:4321/artifacts/s%201/assets/app%20js.html"
        );
    }

    #[tokio::test]
    async fn the_drill_in_query_rides_origin_id_and_rows_decode_with_their_extras() {
        let row = r#"[{
            "id": "s1", "title": "worker", "kind": "subagent", "createdAt": 5,
            "parentId": null, "originId": "root-1",
            "busy": true, "lastTurnStatus": "running", "tokens": 1200
        }]"#;
        let (fetch, seen) = scripted(vec![Ok(HttpResponse {
            status: 200,
            body: row.into(),
        })]);
        let api = api_with(fetch);
        let rows = api.list_sessions(Some("root-1")).await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].url,
            "http://127.0.0.1:4321/sessions?originId=root-1"
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session.id, "s1");
        assert!(rows[0].busy);
        assert_eq!(rows[0].last_turn_status, Some(TurnStatus::Running));
        assert_eq!(rows[0].tokens, Some(1200));
        // `costUsd` omitted when zero — absent must degrade, not break.
        assert_eq!(rows[0].cost_usd, None);
    }

    #[tokio::test]
    async fn bodies_are_json_with_the_right_verbs_and_decline_sends_the_flag() {
        let ack = r#"{"ok":true,"id":"q1","status":"declined"}"#;
        let (fetch, seen) = scripted(vec![Ok(HttpResponse {
            status: 200,
            body: ack.into(),
        })]);
        let api = api_with(fetch);
        api.decline_question("s 1", "q1").await.unwrap();
        let req = &seen.lock().unwrap()[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "http://127.0.0.1:4321/sessions/s%201/questions/q1");
        assert_eq!(req.body.as_deref(), Some(r#"{"decline":true}"#));
    }

    #[tokio::test]
    async fn preflight_surfaces_the_offline_sentence_for_the_exit_2_path() {
        // The composition root prints `bough tui: <message>` and exits 2 —
        // preflight's job is to fail with the sentence, not a stack trace.
        let (fetch, _) = scripted(vec![Err("Connection refused".into())]);
        let api = api_with(fetch);
        let err = api.preflight().await.unwrap_err();
        assert!(err.is_offline());
        assert_eq!(
            err.to_string(),
            "bough server unreachable — run: bough start · http://127.0.0.1:4321"
        );
    }

    #[tokio::test]
    async fn a_2xx_with_an_undecodable_body_is_a_decode_error_not_a_panic() {
        let (fetch, _) = scripted(vec![Ok(HttpResponse {
            status: 200,
            body: r#"{"unexpected": true}"#.into(),
        })]);
        let err = api_with(fetch).get_model_settings().await.unwrap_err();
        match err {
            ApiFailure::Decode { path, .. } => assert_eq!(path, "/model-settings"),
            other => panic!("expected Decode, got {other:?}"),
        }
    }
}
