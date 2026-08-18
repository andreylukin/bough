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

use bough_core::hostfn::artifact::Artifact;
use bough_core::schema::parts::{
    AskQuestion, BackgroundJob, Message, Schedule, Session, TurnStatus,
};
use bough_core::schema::requests::{
    CreateSessionBody, ExtractBody, ForkBody, HandoffBody, MoveBody, PartPick, PatchSessionBody,
    PostMessageBody, PutModelSettingsBody, UnsendBody,
};
use bough_core::types::{Effort, UsageTotals};
use bough_core::workflow::saved::SavedWorkflow;

use crate::store::state::SessionChangeSet;

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
#[derive(Clone, Debug, Default)]
pub struct FetchRequest {
    pub method: String,
    pub url: String,
    /// JSON text. The transport sets `content-type: application/json` iff set.
    pub body: Option<String>,
    /// A RAW body and the content-type that describes it — the attachment
    /// upload, which posts image bytes rather than JSON (`POST /attachments`).
    /// Set instead of `body`, never beside it.
    pub binary: Option<(String, Vec<u8>)>,
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
            let method =
                reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|e| e.to_string())?;
            let mut builder = client.request(method, &req.url);
            if let Some(body) = req.body {
                builder = builder
                    .header("content-type", "application/json")
                    .body(body);
            }
            if let Some((content_type, bytes)) = req.binary {
                builder = builder.header("content-type", content_type).body(bytes);
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

/// What `POST /attachments` answers with (201): where the bytes landed, and
/// the label the composer shows for them. Field names verbatim from the
/// server's handler.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub path: String,
    pub media_type: String,
    pub name: String,
    pub size: i64,
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
    /// The files this block was merged from, when two near-identical rule
    /// files were folded into one. Empty for the ordinary single-file case.
    #[serde(default)]
    pub merged_from: Vec<String>,
}

/// One prompt section as the context tab reads it.
#[derive(Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptSection {
    /// The section id — `identity`, `skill-catalog`, `notes`, …
    pub id: String,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub bytes: usize,
}

/// The last turn's prompt shape. `None` on the wire when this server process
/// has not run a turn for the session — which is NOT an empty prompt.
#[derive(Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptShape {
    #[serde(default)]
    pub sections: Vec<PromptSection>,
    #[serde(default)]
    pub stable_bytes: usize,
    #[serde(default)]
    pub volatile_bytes: usize,
}

/// `GET /sessions/:id/prompt`.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptView {
    pub shape: Option<PromptShape>,
    #[serde(default)]
    pub project_rules: Vec<ProjectRuleSummary>,
    #[serde(default)]
    pub worked_in: Vec<String>,
    #[serde(default)]
    pub context_tokens: Option<i64>,
    #[serde(default)]
    pub cached_tokens: Option<i64>,
    #[serde(default)]
    pub context_limit: Option<i64>,
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

/// `POST /sessions/:id/unsend` — the take-back's answer.
///
/// `bough_core::history::ops::unsend::UnsendResult` is the same shape, but the
/// server only ever WRITES it (Serialize-only); this is the read side. Field
/// names verbatim.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UnsendResult {
    pub session_id: String,
    /// The retracted message's text, for the composer it is going back into.
    pub text: String,
    /// Every message id removed — the retracted one, then whatever followed it.
    pub removed: Vec<String>,
    /// True when a turn was running and has been signalled to stop.
    pub interrupted: bool,
}

/// What `fork` and `extract` answer with (201): the branch AND its thread. The
/// thread rides along for the same reason `GET /sessions/:id` carries it — the
/// client is about to switch to this branch and would otherwise fetch again to
/// render anything at all.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchResult {
    pub session: Session,
    #[serde(default)]
    pub thread: Vec<Message>,
}

/// `POST /sessions/:id/move-into` (200 — it creates no session). `appended` is
/// the server's count, not the caller's: duplicate picks of one message merge.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveResult {
    pub session: Session,
    #[serde(default)]
    pub thread: Vec<Message>,
    #[serde(default)]
    pub appended: usize,
}

/// `POST /sessions/:id/handoff` — the new root alone. No thread: a handoff
/// seeds no messages, so sending one would suggest there is something to read.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffResult {
    pub session: Session,
}

/// `POST /sessions/:id/changes/revert` — what actually happened, per path.
/// Three lists and no summary: a path the server SKIPPED (not in this change
/// set) and one that FAILED are different outcomes, and the row the user reads
/// says which.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RevertOutcome {
    #[serde(default)]
    pub reverted: Vec<String>,
    /// Requested paths that are not the session's to revert.
    #[serde(default)]
    pub skipped: Vec<String>,
    /// The session's own paths that could not be reverted, with git's reason.
    #[serde(default)]
    pub failed: Vec<RevertFailure>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RevertFailure {
    pub path: String,
    pub error: String,
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
        format!(
            "{}/events{}",
            self.base,
            query(&[("sessionId", session_id)])
        )
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
            binary: None,
        };
        (self.fetch)(req)
            .await
            .map_err(|cause| ApiFailure::Offline {
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

    /// `POST /attachments` — the pasted image, raw, with its own media type as
    /// the content-type (row 2.26). NOT JSON and not multipart: the server
    /// stores the bytes and answers with where they landed, and that answer is
    /// what a message part then names.
    ///
    /// The failure text is the server's own sentence wherever it sends one, and
    /// `could not attach image` where it sends none — a paste that silently did
    /// nothing is the failure this replaces.
    pub async fn upload_image(
        &self,
        bytes: Vec<u8>,
        media_type: &str,
    ) -> Result<Attachment, ApiFailure> {
        let req = FetchRequest {
            method: "POST".to_string(),
            url: format!("{}/attachments", self.base),
            body: None,
            binary: Some((media_type.to_string(), bytes)),
        };
        let res = (self.fetch)(req)
            .await
            .map_err(|cause| ApiFailure::Offline {
                base: self.base.clone(),
                cause,
            })?;
        let parsed: Option<Value> = serde_json::from_str(&res.body).ok();
        if !(200..300).contains(&res.status) {
            let message = parsed
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|e| e.as_str())
                .map(str::to_string)
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| {
                    let trimmed = res.body.trim();
                    if trimmed.is_empty() {
                        "could not attach image".to_string()
                    } else {
                        trimmed.to_string()
                    }
                });
            return Err(ApiFailure::Api {
                status: res.status,
                message,
                method: "POST".to_string(),
                path: "/attachments".to_string(),
            });
        }
        serde_json::from_value(parsed.unwrap_or(Value::Null)).map_err(|e| ApiFailure::Decode {
            method: "POST".to_string(),
            path: "/attachments".to_string(),
            message: e.to_string(),
        })
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
        self.get(&format!(
            "/questions{}",
            query(&[("sessionId", session_id)])
        ))
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

    // -- history operations ---------------------------------------------------

    /// The take-back — the one history call that does NOT create a branch.
    ///
    /// Deletes the named message and everything after it from the session it
    /// was sent in, stopping the turn it started on the way, and hands the text
    /// back for the composer. Only ever called with the session's own LAST user
    /// message: the server refuses anything else, and everything else is a fork.
    pub async fn unsend(&self, id: &str, at_message_id: &str) -> Result<UnsendResult, ApiFailure> {
        self.post(
            &format!("/sessions/{}/unsend", seg(id)),
            Some(to_value(&UnsendBody {
                at_message_id: at_message_id.to_string(),
            })?),
        )
        .await
    }

    /// `POST /sessions/:id/fork` — branch at a turn. The thread rides along so
    /// the client that is about to switch does not need a second fetch.
    pub async fn fork(&self, id: &str, body: &ForkBody) -> Result<BranchResult, ApiFailure> {
        self.post(
            &format!("/sessions/{}/fork", seg(id)),
            Some(to_value(body)?),
        )
        .await
    }

    /// `POST /sessions/:id/extract` — the picked turns become a fresh ROOT.
    /// Nothing is destroyed: the source keeps every turn.
    pub async fn extract(&self, id: &str, picks: &[PartPick]) -> Result<BranchResult, ApiFailure> {
        self.post(
            &format!("/sessions/{}/extract", seg(id)),
            Some(to_value(&ExtractBody {
                picks: picks.to_vec(),
            })?),
        )
        .await
    }

    /// `POST /sessions/:id/move-into` — extract's mirror: copies land on the
    /// TAIL of `target_id`. The `:id` in the path is the TARGET; the source is
    /// the argument, which is why it travels in the body.
    pub async fn move_into(
        &self,
        target_id: &str,
        source_id: &str,
        picks: &[PartPick],
    ) -> Result<MoveResult, ApiFailure> {
        self.post(
            &format!("/sessions/{}/move-into", seg(target_id)),
            Some(to_value(&MoveBody {
                source_id: source_id.to_string(),
                picks: picks.to_vec(),
            })?),
        )
        .await
    }

    /// `POST /sessions/:id/handoff` — `/compact`'s route. A fresh ROOT with the
    /// distilled prompt attached as its DRAFT: nothing is sent, and the old
    /// thread is untouched. No `thread` comes back, because a handoff inherits
    /// none.
    pub async fn handoff(&self, id: &str, goal: &str) -> Result<HandoffResult, ApiFailure> {
        self.post(
            &format!("/sessions/{}/handoff", seg(id)),
            Some(to_value(&HandoffBody {
                goal: goal.to_string(),
            })?),
        )
        .await
    }

    // -- schedules ------------------------------------------------------------

    /// `GET /schedules` — a bare array, disabled rows included (that is how one
    /// is re-enabled). The rail filters to the enabled ones.
    pub async fn list_schedules(&self) -> Result<Vec<Schedule>, ApiFailure> {
        self.get("/schedules").await
    }

    /// `PATCH /schedules/:id` — the rail's stop is a DISABLE, not a delete: the
    /// row leaves the rail and the schedule keeps its spec and its prompt.
    pub async fn set_schedule_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<Schedule, ApiFailure> {
        self.patch(
            &format!("/schedules/{}", seg(id)),
            Some(serde_json::json!({ "enabled": enabled })),
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

    // -- the changes rail (row 2.20) -----------------------------------------

    /// What this session did to its checkout. `available: false` with a stated
    /// `reason` is a first-class ANSWER — "this workspace is not a repository"
    /// and "you changed nothing" are different facts — so a non-git workspace
    /// is a 200 here, never an error.
    pub async fn changes(&self, id: &str) -> Result<SessionChangeSet, ApiFailure> {
        self.get(&format!("/sessions/{}/changes", seg(id))).await
    }

    /// Put paths back. `None` is the whole change set; an EMPTY selection is
    /// refused by the server on purpose, so it is never sent — the two are
    /// different requests and conflating them reverts everything by accident.
    pub async fn revert_changes(
        &self,
        id: &str,
        paths: Option<&[String]>,
    ) -> Result<RevertOutcome, ApiFailure> {
        let body = match paths {
            Some(paths) => serde_json::json!({ "paths": paths }),
            None => serde_json::json!({}),
        };
        self.post(&format!("/sessions/{}/changes/revert", seg(id)), Some(body))
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

    // -- the composer's `@` and `/` candidates ---------------------------------

    /// The `@` completion's candidates for that session's workspace.
    pub async fn list_files(&self, session_id: &str) -> Result<FileList, ApiFailure> {
        self.get(&format!("/sessions/{}/files", seg(session_id)))
            .await
    }

    /// The same, for a conversation that has not started and so has no session
    /// — the screen where someone types `@` for the first time.
    pub async fn list_files_in(&self, workspace: &str) -> Result<FileList, ApiFailure> {
        self.get(&format!(
            "/files{}",
            query(&[("workspace", Some(workspace))])
        ))
        .await
    }

    /// The branch a workspace is checked out on, for the meter's `dir@branch`.
    ///
    /// Polled rather than fetched once: a checkout happens in ANOTHER terminal,
    /// so there is no event to hang it on, and a status bar naming the branch
    /// you left is worse than one naming none (App.tsx::BRANCH_POLL_MS).
    pub async fn branch(&self, dir: &str) -> Result<BranchName, ApiFailure> {
        self.get(&format!("/fs/branch{}", query(&[("dir", Some(dir))])))
            .await
    }

    /// One directory's entries, for an `@` path that leaves the workspace.
    ///
    /// `git ls-files` cannot name anything outside the repo, so `@~/` had
    /// nothing to offer; this is what fills the popup once the typed path looks
    /// absolute.
    pub async fn list_dir_entries(
        &self,
        dir: &str,
        base: Option<&str>,
    ) -> Result<EntryList, ApiFailure> {
        self.get(&format!(
            "/fs/entries{}",
            query(&[("dir", Some(dir)), ("base", base)])
        ))
        .await
    }

    /// What is installed, for the `/` popup's skill rows.
    pub async fn list_skills(&self) -> Result<SkillList, ApiFailure> {
        self.get("/skills").await
    }

    /// The same route, read as the skills TAB needs it: every field the server
    /// serves, `error` and `sources` included. Two readings of one payload
    /// rather than one widened row, because the composer ranks names and must
    /// not carry a broken skill's reason into a completion popup.
    pub async fn list_skill_rows(&self) -> Result<SkillTabList, ApiFailure> {
        self.get("/skills").await
    }

    // -- the context tab ------------------------------------------------------

    /// `GET /sessions/:id/prompt` — what the last turn actually put in the
    /// window, section by section.
    pub async fn session_prompt(&self, id: &str) -> Result<PromptView, ApiFailure> {
        self.get(&format!("/sessions/{id}/prompt")).await
    }

    // -- the config tab -------------------------------------------------------

    /// `GET /config` — every hook, skill and extension, grouped by where it
    /// came from, with the switch on each.
    pub async fn list_config(&self) -> Result<ConfigList, ApiFailure> {
        self.get("/config").await
    }

    /// `POST /config/:id` — turn one source, or one thing inside one, on or
    /// off. Answers with the whole listing, because a source's switch changes
    /// every row under it and a hook's rebuilds the interpreter.
    pub async fn toggle_config(&self, id: &str, enabled: bool) -> Result<ConfigList, ApiFailure> {
        self.post(
            &format!("/config/{}", seg(id)),
            Some(serde_json::json!({ "enabled": enabled })),
        )
        .await
    }

    // -- the model tab's catalog ----------------------------------------------

    /// `GET /models` — the picker's catalog, answered SERVER-SIDE because the
    /// server is the process that holds the credential. A TUI that discovered
    /// with its own environment would offer rows the server cannot bill.
    pub async fn list_models(&self) -> Result<ModelCatalog, ApiFailure> {
        self.get("/models").await
    }

    // -- the mcp tab ----------------------------------------------------------

    /// `GET /mcp/servers` — registry, grants, connections and stored
    /// credentials, for one scope. NEVER CACHED by the caller: grants and
    /// connections change between turns, and a panel showing last minute's MCP
    /// state is worse than one showing none.
    pub async fn mcp_status(&self, session_id: Option<&str>) -> Result<McpStatus, ApiFailure> {
        self.get(&format!(
            "/mcp/servers{}",
            query(&[("session", session_id)])
        ))
        .await
    }

    /// `POST /mcp/servers/:name/enable|disable` — the grant, which is the ⏎ of
    /// this tab. Install-wide, not per-turn.
    pub async fn set_mcp_enabled(
        &self,
        name: &str,
        enabled: bool,
        session_id: Option<&str>,
    ) -> Result<Value, ApiFailure> {
        let verb = if enabled { "enable" } else { "disable" };
        self.post(
            &format!(
                "/mcp/servers/{}/{verb}{}",
                seg(name),
                query(&[("session", session_id)])
            ),
            None,
        )
        .await
    }

    /// `PUT /mcp/servers/:name` — register a remote server by URL.
    pub async fn put_mcp_server(&self, name: &str, url: &str) -> Result<Value, ApiFailure> {
        self.put(
            &format!("/mcp/servers/{}", seg(name)),
            Some(serde_json::json!({ "url": url })),
        )
        .await
    }

    /// `DELETE /mcp/servers/:name` — drop the registration itself. `F` next door
    /// drops only the CREDENTIALS and keeps the entry.
    pub async fn delete_mcp_server(&self, name: &str) -> Result<Value, ApiFailure> {
        self.json("DELETE", &format!("/mcp/servers/{}", seg(name)), None)
            .await
    }

    /// `POST /mcp/servers/:name/connect` — the `c` test: names the tools, or the
    /// error, without spending a turn on a tool call.
    pub async fn connect_mcp_server(&self, name: &str) -> Result<Value, ApiFailure> {
        self.post(&format!("/mcp/servers/{}/connect", seg(name)), None)
            .await
    }

    /// `POST /mcp/servers/:name/restart` — bounce a stdio server's process.
    pub async fn restart_mcp_server(&self, name: &str) -> Result<Value, ApiFailure> {
        self.post(&format!("/mcp/servers/{}/restart", seg(name)), None)
            .await
    }

    /// `POST /mcp/servers/:name/auth` — begin the OAuth flow. The answer carries
    /// the URL the panel prints; nothing here opens a browser.
    pub async fn begin_mcp_auth(&self, name: &str) -> Result<Value, ApiFailure> {
        self.post(&format!("/mcp/servers/{}/auth", seg(name)), None)
            .await
    }

    /// `DELETE /mcp/servers/:name/auth` — forget the stored tokens.
    pub async fn clear_mcp_auth(&self, name: &str) -> Result<Value, ApiFailure> {
        self.json("DELETE", &format!("/mcp/servers/{}/auth", seg(name)), None)
            .await
    }

    // -- the workflows tab ----------------------------------------------------

    /// `GET /workflows[?session=]` — every run, newest first. Summaries: the
    /// script text is the largest field by far.
    pub async fn list_workflows(
        &self,
        session_id: Option<&str>,
    ) -> Result<WorkflowList, ApiFailure> {
        self.get(&format!("/workflows{}", query(&[("session", session_id)])))
            .await
    }

    /// `GET /workflows/:id` — the run, its agents, and the three accounting
    /// fields spec §8 requires of a run view.
    pub async fn get_workflow(&self, id: &str) -> Result<WorkflowDetail, ApiFailure> {
        self.get(&format!("/workflows/{}", seg(id))).await
    }

    pub async fn pause_workflow(&self, id: &str) -> Result<Value, ApiFailure> {
        self.post(&format!("/workflows/{}/pause", seg(id)), None)
            .await
    }

    pub async fn resume_workflow(&self, id: &str) -> Result<Value, ApiFailure> {
        self.post(&format!("/workflows/{}/resume", seg(id)), None)
            .await
    }

    pub async fn stop_workflow(&self, id: &str) -> Result<Value, ApiFailure> {
        self.post(&format!("/workflows/{}/stop", seg(id)), None)
            .await
    }

    /// `POST /workflows/:id/rerun` — a NEW run seeded from this one's journal.
    pub async fn rerun_workflow(&self, id: &str) -> Result<Value, ApiFailure> {
        self.post(&format!("/workflows/{}/rerun", seg(id)), None)
            .await
    }

    /// `POST /workflows/:id/save` — store the script to run again by name.
    pub async fn save_workflow_as(&self, id: &str, name: &str) -> Result<Value, ApiFailure> {
        self.post(
            &format!("/workflows/{}/save", seg(id)),
            Some(serde_json::json!({ "name": name })),
        )
        .await
    }

    /// `GET /saved-workflows` — the scripts saved by name, newest first as the
    /// server ordered them. The envelope is unwrapped here so no caller has to
    /// know the route answers `{saved: […]}`.
    pub async fn list_saved_workflows(&self) -> Result<Vec<SavedWorkflow>, ApiFailure> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            saved: Vec<SavedWorkflow>,
        }
        let wrapped: Envelope = self.get("/saved-workflows").await?;
        Ok(wrapped.saved)
    }

    // -- artifacts ------------------------------------------------------------

    /// `GET /sessions/:id/artifacts` — what this conversation has published.
    /// Answered from the filesystem, so a session with no row still lists the
    /// files that are demonstrably on disk.
    pub async fn list_artifacts(&self, session_id: &str) -> Result<Vec<Artifact>, ApiFailure> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            artifacts: Vec<Artifact>,
        }
        let wrapped: Envelope = self
            .get(&format!("/sessions/{}/artifacts", seg(session_id)))
            .await?;
        Ok(wrapped.artifacts)
    }

    // -- the cheap-tier cosmetics (row 3.21) ----------------------------------

    /// `POST /sessions/:id/ghost` — the cheap tier's guess at the next message.
    ///
    /// ALWAYS resolves for a session that exists: `{ghost: null}` covers a
    /// missing key, a provider error and an empty conversation alike, so the
    /// composer needs no error path. POST rather than GET because the
    /// half-typed prefix is user text with no business in a URL or a log.
    pub async fn ghost_text(&self, id: &str, prefix: &str) -> Result<GhostText, ApiFailure> {
        self.post(
            &format!("/sessions/{}/ghost", seg(id)),
            Some(serde_json::json!({ "prefix": prefix })),
        )
        .await
    }

    /// `POST /sessions/:id/sections` — topic headers over a conversation's own
    /// turns. Stateless: gists in, labeled index ranges out.
    pub async fn sections(&self, id: &str, gists: &[String]) -> Result<SectionsResult, ApiFailure> {
        let turns: Vec<Value> = gists
            .iter()
            .map(|g| serde_json::json!({ "gist": g }))
            .collect();
        self.post(
            &format!("/sessions/{}/sections", seg(id)),
            Some(serde_json::json!({ "turns": turns })),
        )
        .await
    }

    /// `GET /search` — full-text over every transcript. The tree's `/` filter
    /// is a search of every message, which is what the keymap has always said.
    pub async fn search(&self, q: &str, limit: Option<u32>) -> Result<SearchResult, ApiFailure> {
        let limit = limit.map(|l| l.to_string());
        self.get(&format!(
            "/search{}",
            query(&[("q", Some(q)), ("limit", limit.as_deref())])
        ))
        .await
    }

    // -- theme ----------------------------------------------------------------

    /// `GET /theme` — `{theme, defaults}`. Always 200: "no theme is set" is an
    /// ANSWER (the default palette), so this never has a not-found arm.
    pub async fn get_theme(&self) -> Result<crate::theme::ThemeState, ApiFailure> {
        self.get("/theme").await
    }

    /// Persist the browsed palette. The two verbs are NOT interchangeable —
    /// `theme == null` must DELETE, because a PUT of an empty map stores a
    /// *named* theme overriding nothing and the next boot reads it back as a
    /// custom palette (theme.rs::persist_request owns that decision).
    pub async fn write_theme(
        &self,
        write: &crate::theme::ThemeWrite,
    ) -> Result<crate::theme::ThemeState, ApiFailure> {
        match write {
            crate::theme::ThemeWrite::Delete => self.json("DELETE", "/theme", None).await,
            crate::theme::ThemeWrite::Put { name, colors } => {
                self.put(
                    "/theme",
                    Some(to_value(&serde_json::json!({
                        "name": name,
                        "colors": colors,
                    }))?),
                )
                .await
            }
        }
    }
}

/// `GET /sessions/:id/files` and `GET /files?workspace=` — gitignore-filtered
/// by construction (the server runs `git ls-files`), which is the contract the
/// popup relies on and cannot enforce itself.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileList {
    pub files: Vec<String>,
}

/// `GET /fs/branch?dir=` — the checked-out branch, for the meter.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BranchName {
    pub branch: String,
}

/// `GET /fs/entries?dir=` — one directory, one level deep.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EntryList {
    pub entries: Vec<String>,
}

/// One installed skill, as the `/` popup reads it. The listing carries more
/// (source, dir, mcp, error); the composer needs the name and the sentence.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRow {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// `POST /sessions/:id/ghost` — `{ghost: string|null}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GhostText {
    pub ghost: Option<String>,
}

/// `POST /sessions/:id/sections` — index ranges over the session's OWN turns.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SectionsResult {
    pub sections: Vec<crate::forest::SectionRange>,
}

/// One `GET /search` hit. Only the fields the tree reads are modelled: a hit in
/// a COLLAPSED session (a subagent, a workflow agent) is attributed to its
/// spawner, because that is the row the tree can actually show.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub message_id: String,
    pub session_id: String,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub origin_id: Option<String>,
}

/// `GET /search`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchResult {
    #[serde(default)]
    pub hits: Vec<SearchHit>,
}

/// `GET /skills`. `sources` rides along because "why is my skill not listed?"
/// is almost always answered by naming the directory that was walked — a client
/// that only ever sees an empty array cannot tell "nothing installed" from
/// "looking in the wrong place".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillList {
    pub skills: Vec<SkillRow>,
    #[serde(default)]
    pub sources: Vec<SkillSourceRow>,
}

/// Where the listing was read from.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct SkillSourceRow {
    pub source: String,
    pub dir: String,
}

/// `GET /skills` as the skills TAB reads it — the full rows, `error` included.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillTabList {
    pub skills: Vec<crate::components::panel::skills::SkillRow>,
    #[serde(default)]
    pub sources: Vec<SkillSourceRow>,
}

/// `GET /config` — the config tab's rows: every source and everything under it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConfigList {
    pub groups: Vec<crate::components::panel::config::ConfigGroupRow>,
}

/// `GET /models` — the picker's catalog.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelCatalog {
    pub models: Vec<ModelRow>,
}

/// One catalog row. An id is a PROVIDER ROUTING DECISION, so the table lives in
/// `llm/routing.rs` and the picker takes these as data — no provider name is
/// written outside `llm/`.
pub use bough_core::llm::routing::ModelRow;

/// `GET /mcp/servers` — the whole MCP state for one scope, as `mcp/status.rs`
/// serialises it.
pub use bough_core::mcp::status::McpStatus;

// ---------------------------------------------------------------------------
// Workflows
// ---------------------------------------------------------------------------

/// `GET /workflows` — every run in this conversation, newest first.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkflowList {
    pub workflows: Vec<WorkflowSummary>,
}

/// A run's per-status agent counts. `cached` is broken out from `done` because
/// a replay and a live call are different news about the same green number.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct WorkflowAgentCounts {
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub done: usize,
    #[serde(default)]
    pub cached: usize,
    #[serde(default)]
    pub running: usize,
    #[serde(default)]
    pub queued: usize,
    #[serde(default)]
    pub failed: usize,
}

/// A run trimmed for the list: no script text, which is the largest field by
/// far. `status` is read as a STRING, not the enum — the glyph table keys on the
/// wire spelling, and a status this build has never heard of must render as
/// `⚠ orphaned` rather than fail the whole fetch.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub status: String,
    #[serde(default)]
    pub current_phase: Option<String>,
    #[serde(default)]
    pub agents: WorkflowAgentCounts,
    pub created_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
}

/// `GET /workflows/:id` — the run view's whole body.
///
/// `replay`, `cost` and `warning` are the three accounting fields spec §8
/// requires; `replay` is REQUIRED rather than optional here on purpose, because
/// a client that can decode a run without it is a client that can render one
/// without it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDetail {
    pub workflow: bough_core::schema::parts::WorkflowRun,
    #[serde(default)]
    pub agents: Vec<bough_core::workflow::control::WorkflowAgentView>,
    /// `~/.bough/workflows/<id>.js` — the file the steering loop edits.
    pub script_file: String,
    /// Is this run held by a worker in the process that answered? A run left
    /// `running` by a dead process is reconciled to `orphaned` at boot, and a
    /// client that cannot tell the two apart shows a fan-out that will never
    /// advance.
    #[serde(default)]
    pub live: bool,
    pub replay: bough_core::workflow::report::ReplaySummary,
    pub cost: bough_core::workflow::report::RunCost,
    #[serde(default)]
    pub warning: Option<bough_core::workflow::report::LargeRunFlag>,
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
        let (fetch, _seen) = scripted(vec![
            Err("error sending request: Connection refused".into()),
        ]);
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
            ApiFailure::Api {
                status, message, ..
            } => {
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
    async fn a_non_repository_change_set_is_a_200_answer_not_an_error() {
        // `available: false` with a stated reason is a first-class answer:
        // "not a repository" and "you changed nothing" are different facts.
        let body = r#"{"available":false,"reason":"this workspace is not a git repository","files":[],"workspace":"/tmp/x"}"#;
        let (fetch, seen) = scripted(vec![Ok(HttpResponse {
            status: 200,
            body: body.into(),
        })]);
        let set = api_with(fetch).changes("s 1").await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].url,
            "http://127.0.0.1:4321/sessions/s%201/changes"
        );
        assert!(!set.available);
        assert_eq!(
            set.reason.as_deref(),
            Some("this workspace is not a git repository")
        );
    }

    #[tokio::test]
    async fn revert_sends_no_paths_key_at_all_for_the_whole_set() {
        // An explicit `paths: []` is REFUSED by the server on purpose, and
        // "the whole change set" is the absence of the key — conflating the
        // two reverts everything by accident.
        let ack = r#"{"reverted":["a.ts"],"skipped":[],"failed":[]}"#;
        let (fetch, seen) = scripted(vec![Ok(HttpResponse {
            status: 200,
            body: ack.into(),
        })]);
        let api = api_with(fetch);
        let outcome = api.revert_changes("s1", None).await.unwrap();
        assert_eq!(outcome.reverted, vec!["a.ts"]);
        {
            let seen = seen.lock().unwrap();
            let req = &seen[0];
            assert_eq!(req.method, "POST");
            assert_eq!(req.url, "http://127.0.0.1:4321/sessions/s1/changes/revert");
            assert_eq!(req.body.as_deref(), Some("{}"));
        }

        let (fetch, seen) = scripted(vec![Ok(HttpResponse {
            status: 200,
            body: ack.into(),
        })]);
        api_with(fetch)
            .revert_changes("s1", Some(&["a.ts".to_string()]))
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].body.as_deref(),
            Some(r#"{"paths":["a.ts"]}"#)
        );
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
    async fn a_pasted_image_posts_raw_bytes_under_its_own_media_type_and_reads_the_201() {
        let (fetch, seen) = scripted(vec![Ok(HttpResponse {
            status: 201,
            body: r#"{"path":"/home/dev/.bough/attachments/a.png","mediaType":"image/png","name":"clipboard.png","size":4}"#.into(),
        })]);
        let part = api_with(fetch)
            .upload_image(vec![137, 80, 78, 71], "image/png")
            .await
            .expect("201 is a success");
        assert_eq!(
            part,
            Attachment {
                path: "/home/dev/.bough/attachments/a.png".into(),
                media_type: "image/png".into(),
                name: "clipboard.png".into(),
                size: 4,
            }
        );
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].url, "http://127.0.0.1:4321/attachments");
        // RAW, not JSON and not multipart: the bytes go up as themselves.
        assert!(reqs[0].body.is_none(), "no JSON body");
        assert_eq!(
            reqs[0].binary.as_ref().map(|(t, b)| (t.as_str(), b.len())),
            Some(("image/png", 4))
        );
    }

    #[tokio::test]
    async fn a_refused_attachment_keeps_the_servers_own_sentence() {
        let (fetch, _) = scripted(vec![Ok(HttpResponse {
            status: 400,
            body: r#"{"error":"could not save clipboard image"}"#.into(),
        })]);
        let err = api_with(fetch)
            .upload_image(vec![1, 2], "image/png")
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "could not save clipboard image");
        assert_eq!(err.status(), Some(400));
    }

    #[tokio::test]
    async fn an_attachment_failure_with_no_sentence_still_says_something_useful() {
        let (fetch, _) = scripted(vec![Ok(HttpResponse {
            status: 500,
            body: String::new(),
        })]);
        let err = api_with(fetch)
            .upload_image(vec![1], "image/png")
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "could not attach image");
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

    /// The composer's candidate routes, with the path characters that break a
    /// naive query string (`~`, `/`) actually encoded — the server decodes
    /// `%xx`, and a bare `~/` in a query is what made `@~/` answer nothing.
    #[tokio::test]
    async fn the_completion_routes_carry_encoded_paths() {
        let ok = |body: &str| {
            Ok(HttpResponse {
                status: 200,
                body: body.to_string(),
            })
        };
        let (fetch, seen) = scripted(vec![
            ok(r#"{"files":["src/app.rs"]}"#),
            ok(r#"{"files":[]}"#),
            ok(r#"{"entries":["repos/"]}"#),
            ok(r#"{"skills":[{"name":"prewalk","description":"plan first"}],"sources":[]}"#),
        ]);
        let api = api_with(fetch);
        assert_eq!(
            api.list_files("s1").await.unwrap().files,
            vec!["src/app.rs"]
        );
        api.list_files_in("/w/demo").await.unwrap();
        assert_eq!(
            api.list_dir_entries("~/", Some("/w/demo"))
                .await
                .unwrap()
                .entries,
            vec!["repos/"]
        );
        let skills = api.list_skills().await.unwrap().skills;
        assert_eq!(skills[0].name, "prewalk");
        assert_eq!(skills[0].description, "plan first");
        let urls: Vec<String> = seen.lock().unwrap().iter().map(|r| r.url.clone()).collect();
        assert_eq!(urls[0], "http://127.0.0.1:4321/sessions/s1/files");
        assert_eq!(urls[1], "http://127.0.0.1:4321/files?workspace=%2Fw%2Fdemo");
        assert_eq!(
            urls[2],
            "http://127.0.0.1:4321/fs/entries?dir=%7E%2F&base=%2Fw%2Fdemo"
        );
        assert_eq!(urls[3], "http://127.0.0.1:4321/skills");
    }
}

// ---------------------------------------------------------------------------
// Wire-decode tests — the payloads are built by the SERVER'S OWN serializers,
// not typed out here. A client type that has drifted from the route it reads is
// the failure mode these exist for, and a fixture string would only prove this
// file agrees with itself.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod wire_tests {
    use super::*;
    use bough_core::db::sqlite_db::{DbOptions, SqliteDb};
    use bough_core::schema::parts::{
        Session, SessionKind, WorkflowAgent, WorkflowAgentStatus, WorkflowPhase, WorkflowRun,
        WorkflowStatus,
    };
    use bough_core::types::SharedDb;
    use bough_core::workflow::control::workflow_detail;
    use bough_core::workflow::engine::workflow_summary;
    use std::sync::{Arc, Mutex};

    fn db() -> SharedDb {
        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        db.lock()
            .unwrap()
            .create_session(Session {
                id: "sess-owner".into(),
                title: "s".into(),
                kind: SessionKind::Root,
                created_at: 1,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some("/tmp/w".into()),
                origin_dir: Some("/tmp/w".into()),
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
            .unwrap();
        db.lock()
            .unwrap()
            .create_workflow(WorkflowRun {
                id: "run-2".into(),
                session_id: "sess-owner".into(),
                name: "audit-handlers".into(),
                description: "Review every handler".into(),
                script: "phase('Review')\n".into(),
                phases: vec![WorkflowPhase {
                    title: "Review".into(),
                    detail: None,
                }],
                status: WorkflowStatus::Running,
                current_phase: Some("Review".into()),
                result: None,
                error: None,
                args: None,
                // No source run: the FK points at `workflows(id)`, and what this
                // test is pinning is the DECODE, not the relaunch chain.
                resume_of: None,
                created_at: 10,
                finished_at: None,
            })
            .unwrap();
        db.lock()
            .unwrap()
            .create_workflow_agent(WorkflowAgent {
                id: "a1".into(),
                run_id: "run-2".into(),
                idx: 0,
                key: "k1".into(),
                label: "review app.rs".into(),
                phase: Some("Review".into()),
                prompt: "Review src/server/app.rs".into(),
                model: Some("sonnet".into()),
                status: WorkflowAgentStatus::Cached,
                result: Some("no findings".into()),
                error: None,
                session_id: None,
                started_at: 10,
                finished_at: Some(30),
            })
            .unwrap();
        db
    }

    #[test]
    fn a_run_summary_off_the_route_decodes_into_the_list_row() {
        let db = db();
        let run = db.lock().unwrap().get_workflow("run-2").unwrap().unwrap();
        let body = workflow_summary(&db, &run);
        let row: WorkflowSummary =
            serde_json::from_value(body.clone()).unwrap_or_else(|e| panic!("{e}: {body}"));
        assert_eq!(row.id, "run-2");
        assert_eq!(row.name, "audit-handlers");
        // The wire spells the status; the glyph table keys on that spelling.
        assert_eq!(row.status, "running");
        assert_eq!(row.current_phase.as_deref(), Some("Review"));
        // The counts the list row prints — `cached` broken out from `done`,
        // because a replay and a live call are different news.
        assert_eq!(row.agents.total, 1);
        assert_eq!(row.agents.done, 1);
        assert_eq!(row.agents.cached, 1);
        assert_eq!(row.created_at, 10);
    }

    #[test]
    fn a_run_detail_off_the_route_decodes_with_its_replay_accounting_intact() {
        let db = db();
        let run = db.lock().unwrap().get_workflow("run-2").unwrap().unwrap();
        let body = workflow_detail(&db, &run, &[], 100).unwrap();
        let detail: WorkflowDetail =
            serde_json::from_value(body.clone()).unwrap_or_else(|e| panic!("{e}: {body}"));
        assert_eq!(detail.workflow.id, "run-2");
        assert_eq!(detail.agents.len(), 1);
        assert_eq!(detail.agents[0].agent.label, "review app.rs");
        assert!(detail.script_file.ends_with("run-2.js"));
        // SPEC §8: the accounting is REQUIRED, and a client that can decode a
        // run without it is a client that can render one without it — hence a
        // non-Option field, and hence this assertion.
        assert_eq!(detail.replay.total, 1);
        assert_eq!(detail.replay.replayed, 1);
        assert!(
            !detail.replay.line.is_empty(),
            "the canonical sentence rides the wire"
        );
        // `final` is a Rust keyword and the wire does not care: the field must
        // survive the round trip under its real name.
        assert!(
            body.get("replay").and_then(|r| r.get("final")).is_some(),
            "{body}"
        );
        assert_eq!(detail.cost.agents, 1);
    }

    #[test]
    fn the_skills_route_decodes_both_ways_the_client_reads_it() {
        // The composer's narrow row and the tab's full row are two readings of
        // ONE payload; a widened row would carry a broken skill's reason into a
        // completion popup.
        let body = serde_json::json!({
            "skills": [
                {"name": "history", "description": "query the db", "source": "user", "dir": "/s"},
                {"name": "broken", "description": "", "source": "user", "dir": "/s",
                 "error": "SKILL.md has no front matter", "mcp": ["todoist"]}
            ],
            "sources": [{"source": "user", "dir": "/home/u/.bough/skills"}]
        });
        let narrow: SkillList = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(narrow.skills.len(), 2);
        assert_eq!(narrow.sources[0].dir, "/home/u/.bough/skills");
        let full: SkillTabList = serde_json::from_value(body).unwrap();
        assert_eq!(
            full.skills[1].error.as_deref(),
            Some("SKILL.md has no front matter")
        );
        assert_eq!(full.skills[1].mcp, vec!["todoist".to_string()]);
        // A skill with neither key decodes rather than failing the whole list.
        assert!(full.skills[0].error.is_none());
        assert!(full.skills[0].mcp.is_empty());
    }

    #[test]
    fn the_model_catalog_decodes_the_rows_the_router_serves() {
        let models: &[bough_core::llm::routing::ModelRow] = &bough_core::llm::routing::MODELS;
        let body = serde_json::json!({ "models": models });
        let catalog: ModelCatalog = serde_json::from_value(body).unwrap();
        assert_eq!(catalog.models.len(), models.len());
        assert!(
            !catalog.models.is_empty(),
            "the compiled-in table is never empty"
        );
    }

    #[test]
    fn the_mcp_status_decodes_the_four_keys_the_route_documents() {
        let status = bough_core::mcp::status::mcp_status_for(&Default::default());
        let body = serde_json::to_value(&status).unwrap();
        for key in ["registry", "auth", "active", "connections"] {
            assert!(body.get(key).is_some(), "{key} missing: {body}");
        }
        let decoded: McpStatus = serde_json::from_value(body).unwrap();
        assert_eq!(decoded.active.len(), status.active.len());
    }
}
