//! The TUI's state shape (port of `src/tui/store.ts` — the data half).
//!
//! THE INVARIANT: **state lives here, rendering lives in components, and the
//! reducer touches neither a terminal nor a server.** Everything in this file is
//! plain data; `reduce.rs` is the only writer and it is pure.
//!
//! Wire response shapes the TUI reads (`SessionRow`, `SessionSnapshot`, …) are
//! declared here for now — they are the TS `tui/api.ts` response types, and the
//! api.rs port (row 1.32) should re-export or absorb them when it lands. They are
//! serde-shaped exactly as the server writes them (camelCase, optionals omitted).

use std::collections::HashMap;

use bough_core::schema::events::BoughEvent;
use bough_core::schema::parts::{
    AskQuestion, BackgroundJob, Message, Schedule, Session, TurnStatus,
};
use bough_core::schema::parts::{WorkflowPhase, WorkflowStatus};
use bough_core::types::UsageTotals;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Wire response shapes (tui/api.ts) — see module header.
// ---------------------------------------------------------------------------

/// `SessionRow = Session & {busy, lastTurnStatus?, costUsd?, tokens?}` — the
/// server's derived listing extras. Optional fields absent from older servers
/// must degrade, not break.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    #[serde(flatten)]
    pub session: Session,
    pub busy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_status: Option<TurnStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
}

impl std::ops::Deref for SessionRow {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.session
    }
}

/// A listed session plus the one fact only the client knows.
///
/// `unseen` is deliberately NOT a wire field: it means "this session finished a
/// turn while you were looking at another one". Three states, like the TS
/// optional: `None` (never set), `Some(true)`, `Some(false)` (cleared by open).
#[derive(Clone, Debug, PartialEq)]
pub struct TuiSessionRow {
    pub row: SessionRow,
    pub unseen: Option<bool>,
}

impl std::ops::Deref for TuiSessionRow {
    type Target = SessionRow;
    fn deref(&self) -> &SessionRow {
        &self.row
    }
}

impl From<SessionRow> for TuiSessionRow {
    fn from(row: SessionRow) -> Self {
        TuiSessionRow { row, unseen: None }
    }
}

/// `UsageTotals & {tree: UsageTotals}` — the session's own totals plus the
/// subtree's, as `GET /sessions/:id` and `/usage` both return them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotUsage {
    #[serde(flatten)]
    pub totals: UsageTotals,
    pub tree: UsageTotals,
}

impl std::ops::Deref for SnapshotUsage {
    type Target = UsageTotals;
    fn deref(&self) -> &UsageTotals {
        &self.totals
    }
}

/// One injected `AGENTS.md`, as the snapshot names it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRuleSummary {
    pub label: String,
    pub path: String,
    pub bytes: i64,
}

/// `GET /sessions/:id`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session: Session,
    pub thread: Vec<Message>,
    pub usage: SnapshotUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primed_tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_rules: Option<Vec<ProjectRuleSummary>>,
}

/// `GET /sessions/:id/changes` — always 200; "not a repository" is an answer.
/// File rows are kept untyped until the changes tab (row 2.20) needs them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionChangeSet {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub base: Option<String>,
    #[serde(default)]
    pub files: Vec<Value>,
    pub workspace: Option<String>,
}

/// `JobListRow = BackgroundJob & {tail?, outputLines?}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JobListRow {
    #[serde(flatten)]
    pub job: BackgroundJob,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_lines: Option<i64>,
}

impl std::ops::Deref for JobListRow {
    type Target = BackgroundJob;
    fn deref(&self) -> &BackgroundJob {
        &self.job
    }
}

/// The per-status agent counts a run summary carries.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgentCounts {
    pub total: i64,
    pub done: i64,
    pub cached: i64,
    pub running: i64,
    pub queued: i64,
    pub failed: i64,
}

/// One row of `GET /workflows` (tui-core.md §3).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: WorkflowStatus,
    pub current_phase: Option<String>,
    #[serde(default)]
    pub phases: Vec<WorkflowPhase>,
    pub agents: WorkflowAgentCounts,
    #[serde(default)]
    pub result: Option<Value>,
    pub error: Option<String>,
    pub resume_of: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub script_file: String,
}

/// `ReplayReport = RelaunchReport & {line}` — stored and displayed, never
/// interpreted by the store, so it stays an opaque document until the
/// workflows surface (wave 2) types it.
pub type ReplayReport = Value;

// ---------------------------------------------------------------------------
// Client-only state
// ---------------------------------------------------------------------------

/// How many event identities the dedupe window keeps.
pub const DEDUPE_WINDOW: usize = 256;

/// How many sessions' snapshot watermarks are kept (layer 2 of the dedupe story).
pub const RECONCILED_LIMIT: usize = 64;

/// How many marks are kept. A ledger of a session, not of a lifetime.
pub const MARK_LIMIT: usize = 500;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MarkKind {
    /// A revert, a kill: something that cannot be undone.
    Destructive,
    /// How a turn settled: elapsed, tokens. Written by `turn.settle`.
    Turn,
}

/// A fact about this conversation that the SERVER does not store and the
/// transcript must not lose. Memory-only; marks never expire.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptMark {
    /// Unique and stable, so a renderer can key rows by it.
    pub id: String,
    pub session_id: String,
    /// When it happened. What the transcript orders marks by.
    pub at: i64,
    pub kind: MarkKind,
    /// The whole line, already worded. Rendered as one row.
    pub text: String,
}

/// The turn in flight in the OPEN session, and what it has cost so far.
/// `base*` is the session total at the moment the turn started; the turn's own
/// numbers are the delta.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnMeter {
    pub session_id: String,
    pub started_at: i64,
    pub base_tokens: i64,
    pub base_cost_usd: f64,
    /// This turn's own tokens and spend, refreshed while it runs.
    pub tokens: i64,
    pub cost_usd: f64,
    /// Set by `turn.finished`; the settle that follows it reads the final usage.
    pub ended_at: Option<i64>,
    pub status: Option<TurnStatus>,
}

/// One background job, opened for reading. `output` is the WHOLE retained buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct JobViewState {
    pub id: String,
    /// The session that owns the shell — a subagent's job is not the open session's.
    pub session_id: String,
    /// The row, re-read with the output so the header's status cannot go stale.
    pub job: Option<BackgroundJob>,
    pub output: String,
    /// Why the buffer is not on screen. None once one has been read.
    pub error: Option<String>,
}

/// A NON-open session finished a turn. `seq` makes repeat finishes distinct.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundToast {
    pub session_id: String,
    pub title: String,
    pub seq: u64,
}

/// The whole client state. See the TS source for the field-by-field rationale;
/// every container here is either bounded by a documented cap or freed by a
/// documented event (`retention` tests).
#[derive(Clone, Debug, PartialEq)]
pub struct TuiState {
    /// Is the event stream up? False means the view may be stale, not that work stopped.
    pub connected: bool,
    /// Top-level sessions, newest first. Collapsed kinds never appear here.
    pub sessions: Vec<TuiSessionRow>,
    pub current_id: Option<String>,
    pub session: Option<Session>,
    /// Ancestors root→parent, then own — as the server assembled it.
    pub thread: Vec<Message>,
    /// messageId → text accumulated from `message.delta`, until the text part lands.
    pub streaming: HashMap<String, String>,
    /// callId → live `console.*` lines from the running program.
    pub tool_logs: HashMap<String, Vec<String>>,
    /// Every unsettled `ask()` hold, oldest first. The card shows `asks[0]`.
    pub asks: Vec<AskQuestion>,
    /// Typed while a turn was running, held locally until it ends.
    pub queued: Vec<String>,
    /// When this client last sent a message — what arms the take-back window.
    pub last_send_at: Option<i64>,
    pub notice: Option<String>,
    /// Cheap-tier blurb for the open session. Fails silently by construction.
    pub activity: Option<String>,
    pub usage: Option<SnapshotUsage>,
    /// The model the next turn will call.
    pub effective_model: Option<String>,
    /// The effective model's context window. None = the catalog does not know it.
    pub context_limit: Option<i64>,
    /// Command-history tags this session was primed with — the transcript's `#` row.
    pub primed_tags: Vec<String>,
    /// The `AGENTS.md` files the next turn injects — the other `#` row, and `/rules`.
    pub project_rules: Vec<ProjectRuleSummary>,
    /// Messages the server deleted for a take-back, kept as TOMBSTONES.
    pub dropped_ids: Vec<String>,
    /// None until fetched. `available: false` is an ANSWER, not an error.
    pub changes: Option<SessionChangeSet>,
    /// The open session's background shells AND its subagents'.
    pub jobs: Vec<JobListRow>,
    /// The job the user has OPENED, with its whole retained buffer.
    pub job_view: Option<JobViewState>,
    pub workflows: Vec<WorkflowSummary>,
    /// Every schedule, verbatim from `GET /schedules`. GLOBAL — survives switches.
    pub schedules: Vec<Schedule>,
    /// runId → the last narrator `log()` line. Memory-only, like the run's chip.
    pub workflow_logs: HashMap<String, String>,
    /// Bumped on every `workflow.*` event — a detail view refetches on the change.
    pub workflow_seq: u64,
    /// The open run's replay accounting. Replay is ALWAYS reported.
    pub replay: Option<ReplayReport>,
    pub background: Option<BackgroundToast>,
    /// The permanent record of what was destroyed and what each turn cost.
    /// Never cleared by a session switch.
    pub marks: Vec<TranscriptMark>,
    /// The open session's turn accounting, live. None between turns.
    pub turn: Option<TurnMeter>,
    /// sessionId → when its snapshot was requested. The watermark of layer 2.
    pub reconciled_at: HashMap<String, i64>,
    /// The dedupe window of layer 1, oldest first.
    pub seen: Vec<String>,
}

pub fn initial_state() -> TuiState {
    TuiState {
        connected: false,
        sessions: Vec::new(),
        current_id: None,
        session: None,
        thread: Vec::new(),
        streaming: HashMap::new(),
        tool_logs: HashMap::new(),
        asks: Vec::new(),
        queued: Vec::new(),
        last_send_at: None,
        notice: None,
        activity: None,
        usage: None,
        effective_model: None,
        context_limit: None,
        primed_tags: Vec::new(),
        project_rules: Vec::new(),
        dropped_ids: Vec::new(),
        changes: None,
        jobs: Vec::new(),
        job_view: None,
        workflows: Vec::new(),
        schedules: Vec::new(),
        workflow_logs: HashMap::new(),
        workflow_seq: 0,
        replay: None,
        background: None,
        marks: Vec::new(),
        turn: None,
        reconciled_at: HashMap::new(),
        seen: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Everything that can change state. SSE reader, timers and input all post
/// these over one mpsc so the reducer stays single-threaded and pure.
#[derive(Clone, Debug)]
pub enum StoreAction {
    /// One event off the wire. Everything about dedupe happens under this arm.
    Event { event: BoughEvent },
    Connection { connected: bool },
    Sessions { sessions: Vec<SessionRow> },
    /// Focus a session. Clears everything that belonged to the previous one.
    Open { session_id: Option<String> },
    /// A fresh `GET /sessions/:id`. `at` is when the FETCH WAS ISSUED, not when
    /// it landed — the conservative end of the window.
    Snapshot { at: i64, snapshot: SessionSnapshot },
    Questions { questions: Vec<AskQuestion> },
    /// Optimistic settle: the next hold surfaces immediately; the event confirms it.
    AskSettled { id: String },
    Changes { session_id: String, changes: SessionChangeSet },
    Jobs { session_id: String, jobs: Vec<JobListRow> },
    /// Open, refresh, or (with `view: None`) close the job output view.
    JobView { view: Option<JobViewState> },
    Workflows { session_id: String, workflows: Vec<WorkflowSummary> },
    /// The whole schedule list, re-read. No sessionId gate — schedules are global.
    Schedules { schedules: Vec<Schedule> },
    Replay { replay: Option<ReplayReport> },
    Notice { notice: Option<String> },
    /// A destructive outcome, recorded permanently. Raised by `record()`, which
    /// ALSO sets the notice — the two are one call.
    Mark { session_id: String, at: i64, text: String },
    /// The model a NEW conversation would run on just changed, with none open.
    EffectiveModel { model: Option<String> },
    /// Live usage for a session, polled while its turn runs.
    Usage { session_id: String, usage: SnapshotUsage },
    /// The turn is over AND its final usage has landed: compute the delta and
    /// write the settled mark.
    TurnSettle { at: i64 },
    Queue { text: String },
    QueueDrained,
    /// The tail of `queued` goes back to the composer.
    QueuePop,
    /// A message left this client. Arms the take-back window.
    Sent { at: i64 },
    /// Messages the server has DELETED — the posted half of the take-back.
    /// Named ids, because the server decided what went.
    ThreadDropped { session_id: String, ids: Vec<String> },
}
