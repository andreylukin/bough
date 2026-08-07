//! The injection seams (port of `src/types.ts`). Everything a module needs
//! from the outside world arrives through one of the ports here, and nothing
//! reaches for a global.
//!
//! The invariant: **the database, the clock and the LLM are parameters, not
//! imports.** That is what makes the whole tree testable offline.
//!
//! Second invariant: `hostfn` must never reference `bough-server`. Host
//! functions take a [`TurnCtx`] and nothing else.
//!
//! `Db` and `BusPort` here are PORTS; `db::sqlite_db` and `bus` export
//! concrete implementations that satisfy them.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agents::caps::SpawnCaps;
use crate::bus::{Bus, Listener};
use crate::errors::BoughError;
use crate::harness::protocol::HostFnName;
use crate::hostfn::ask::AskRegistry;
use crate::hostfn::delegate::DetachedSubagents;
use crate::hostfn::files::{SnapshotStore, WriteLog};
use crate::hostfn::jobs::JobRegistry;
use crate::schema::events::{BoughEvent, EventInput};
use crate::schema::parts::{
    Message, Part, Schedule, Session, Turn, TurnStatus, Usage, WorkflowAgent, WorkflowAgentStatus,
    WorkflowPhase, WorkflowRun, WorkflowStatus,
};
use crate::turn::queue::TurnRegistry;

// ---- the clock --------------------------------------------------------------

/// Epoch ms. Injected everywhere the TS injects `now` (db updateTurn,
/// schedules, stats, ask, caps…). Tests never sleep.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The production clock.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    })
}

// ---- Patch<T>: the tri-state PATCH field ------------------------------------

/// Reifies TS key-membership merges (`"error" in patch`): absent = `Keep`,
/// explicit `null` = `Clear`, value = `Set`. Deserialize via the double-Option
/// trick (`#[serde(default)]` on the field makes absent `Keep`; a present
/// field deserializes `null` to `Clear`). Serialize with
/// `#[serde(skip_serializing_if = "Patch::is_keep")]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Patch<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

impl<T> Patch<T> {
    pub fn is_keep(&self) -> bool {
        matches!(self, Patch::Keep)
    }
    /// `Keep` → `None`; `Clear`/`Set` → `Some(applied)`.
    pub fn apply(self, current: Option<T>) -> Option<T> {
        match self {
            Patch::Keep => current,
            Patch::Clear => None,
            Patch::Set(v) => Some(v),
        }
    }
}

impl<T: Serialize> Serialize for Patch<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // Keep is normally skipped via skip_serializing_if; if forced,
            // serialize as null (indistinguishable from Clear on the wire —
            // callers must use skip_serializing_if).
            Patch::Keep | Patch::Clear => serializer.serialize_none(),
            Patch::Set(v) => v.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A PRESENT field: JSON null → Clear, any value → Set. Absence never
        // reaches here — `#[serde(default)]` supplies Keep.
        match Option::<serde_json::Value>::deserialize(deserializer)? {
            None => Ok(Patch::Clear),
            Some(v) => T::deserialize(v)
                .map(Patch::Set)
                .map_err(serde::de::Error::custom),
        }
    }
}

// ---- the event bus port -----------------------------------------------------

/// In-process fan-out to the SSE subscribers. Memory-only and
/// persistence-agnostic: the caller persists first, then publishes.
pub trait BusPort: Send + Sync {
    /// Stamps `seq`/`ts`, delivers synchronously, returns the stamped event.
    fn publish(&self, event: EventInput) -> BoughEvent;
    /// Returns the subscription id for [`BusPort::unsubscribe`].
    fn subscribe(&self, listener: Listener) -> u64;
    fn unsubscribe(&self, id: u64);
    /// Live subscriber count — the leak check in the SSE tests reads it.
    fn size(&self) -> usize;
}

impl BusPort for Bus {
    fn publish(&self, event: EventInput) -> BoughEvent {
        Bus::publish(self, event)
    }
    fn subscribe(&self, listener: Listener) -> u64 {
        Bus::subscribe(self, listener)
    }
    fn unsubscribe(&self, id: u64) {
        Bus::unsubscribe(self, id)
    }
    fn size(&self) -> usize {
        Bus::size(self)
    }
}

// ---- the database port ------------------------------------------------------

/// A session's non-wire runtime facts, kept off the `Session` shape the UI sees.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRuntime {
    /// None = fall back to the process default workspace.
    pub workspace: Option<String>,
    /// The git sha the session started from; None for a non-git workspace.
    pub base: Option<String>,
}

/// Aggregated token/cost totals for the status bar.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd: f64,
}

/// One keyword-search hit (SQLite FTS over transcripts — no embeddings).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub message_id: String,
    pub session_id: String,
    /// The matched excerpt, with the FTS snippet markers already resolved.
    pub snippet: String,
    pub created_at: i64,
}

/// What the search index has swallowed in this process. All-zero/None means a
/// healthy run. Lives here (beside [`SearchHit`]) because the search-safe
/// wrapper is reached through the [`Db`] port: [`Db::index_health`] is how
/// `GET /search` reads the record off the one shared handle.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IndexHealth {
    pub failures: u64,
    pub last_error: Option<String>,
    pub last_failure_at: Option<i64>,
}

/// One finished shell command entering the tag-history memory.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandRecord {
    pub session_id: String,
    pub ts: i64,
    /// Git remote origin URL, else the workspace root path — the scope key.
    pub repo: String,
    pub cmd: String,
    /// Normalized colon-separated tags as one string; "" when the verb carries none.
    pub tags: String,
    /// `tags` split and deduped, for the junction table.
    pub tag_list: Vec<String>,
    /// Workspace-relative directories the command was about (`extractDirs`).
    pub dirs: Vec<String>,
    /// None = unknown (still running when the turn moved on).
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    /// First ~2k chars of what it printed, as the program saw it. "" = silent.
    pub output_head: String,
    /// The spill file holding the full output, when there was one.
    pub spill_path: Option<String>,
    /// "live" | "backfill" (keep the column; the backfill writer is dropped).
    pub source: String,
    /// The supervisor message whose program ran it; None when there is none.
    pub message_id: Option<String>,
}

/// What the memory already knows about one command failing here.
#[derive(Clone, Debug, PartialEq)]
pub struct PriorFailures {
    /// Failing runs of this exact command in this repo, inside the window.
    pub count: i64,
    /// How many of those were this session — the loop signal.
    pub in_session: i64,
    pub last_ts: i64,
    /// The last failure's exit code, and the first ~2k chars it printed.
    pub exit_code: Option<i64>,
    pub output_head: String,
}

/// One day of the tag memory, as `bough tags stats` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct TagDiversityDay {
    /// `YYYY-MM-DD`, local time.
    pub day: String,
    pub sessions: i64,
    pub commands: i64,
    /// Commands that carried at least one tag.
    pub tagged: i64,
    /// The vocabulary: distinct COINED tags that day, references excluded.
    pub distinct_tags: i64,
    /// Distinct references that day (`linear.*`, `pr.*`, …), counted apart.
    pub distinct_refs: i64,
    /// Total tag applications.
    pub tag_uses: i64,
    /// Coined tags used EXACTLY ONCE that day.
    pub singletons: i64,
}

/// One recalled command, as `bough tags show` prints it.
#[derive(Clone, Debug, PartialEq)]
pub struct TaggedCommand {
    pub ts: i64,
    pub repo: String,
    pub cmd: String,
    pub tags: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub session_id: String,
    /// None for a row written before the link existed.
    pub message_id: Option<String>,
}

/// One (tag, outcome) observation, the unit the popularity stats aggregate.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandTagRow {
    pub tag: String,
    pub ts: i64,
    pub exit_code: Option<i64>,
}

/// Options for [`Db::command_tag_rows`].
#[derive(Clone, Debug, Default)]
pub struct CommandTagOpts {
    pub dir: Option<String>,
    pub since_ts: Option<i64>,
}

/// One recent failing command, for error-signature recall.
#[derive(Clone, Debug, PartialEq)]
pub struct RecentFailure {
    pub cmd: String,
    pub output_head: String,
    pub ts: i64,
    pub session_id: String,
}

/// One `session_state` listing row — `length(value)` only, never the value.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StateEntry {
    pub key: String,
    pub bytes: i64,
    pub updated_at: i64,
}

/// `updateTurn` patch. `error` is key-membership merged in TS (`{error: null}`
/// clears), hence [`Patch`]; `usage` REPLACES the six usage columns wholesale.
#[derive(Clone, Debug, Default)]
pub struct TurnPatch {
    pub status: Option<TurnStatus>,
    pub step: Option<String>,
    pub error: Patch<String>,
    pub usage: Option<Usage>,
}

/// `updateWorkflow` patch — key-membership merge; identity fields (id,
/// sessionId, script, createdAt, resumeOf) are NOT patchable.
#[derive(Clone, Debug, Default)]
pub struct WorkflowPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub phases: Option<Vec<WorkflowPhase>>,
    pub status: Option<WorkflowStatus>,
    pub current_phase: Patch<String>,
    /// `undefined` is a legitimate script return — `Clear` stores NULL.
    pub result: Patch<Value>,
    pub error: Patch<String>,
    pub args: Patch<Value>,
    pub finished_at: Patch<i64>,
}

/// `updateWorkflowAgent` patch. `startedAt` patchable on purpose: a queued
/// agent's clock resets when it leaves the run's semaphore.
#[derive(Clone, Debug, Default)]
pub struct WorkflowAgentPatch {
    pub label: Option<String>,
    pub phase: Patch<String>,
    pub status: Option<WorkflowAgentStatus>,
    pub result: Patch<String>,
    pub error: Patch<String>,
    /// Real FK: create the backing session before patching sessionId in.
    pub session_id: Patch<String>,
    pub started_at: Option<i64>,
    pub finished_at: Patch<i64>,
}

/// Typed persistence. No raw SQL exists outside `db/`, so every read and write
/// in the system goes through a method here.
///
/// Ordering contract, which several callers depend on and none may re-derive:
/// - `messages_for` orders by `(created_at, rowid)`.
/// - `thread_for` is ancestors root→parent, then the session's own.
/// - `ancestor_chain` walks to the lineage root, for root-scoped `session_state`.
///
/// Methods are synchronous; the live impl is `SqliteDb` behind
/// `Arc<Mutex<..>>` ([`SharedDb`]). All methods return `Result` — SQLite I/O is
/// fallible in Rust and FK violations must surface (db.test pins them).
pub trait Db: Send {
    // sessions
    /// INSERT then read-back — the returned row is *as stored*.
    fn create_session(&self, session: Session) -> Result<Session, BoughError>;
    fn get_session(&self, id: &str) -> Result<Option<Session>, BoughError>;
    /// Runtime facts (workspace, base) that are not on the wire `Session`.
    /// Unknown id → both None.
    fn get_session_runtime(&self, id: &str) -> Result<SessionRuntime, BoughError>;
    /// Every session, newest first. Visibility is the CALLER's derivation.
    fn list_sessions(&self) -> Result<Vec<Session>, BoughError>;
    /// The branches that collapsed under `originId` — the drill-in query.
    fn sessions_by_origin(&self, origin_id: &str) -> Result<Vec<Session>, BoughError>;
    /// Root→self, inclusive. `[]` for unknown id; a parent_id cycle terminates.
    fn ancestor_chain(&self, id: &str) -> Result<Vec<Session>, BoughError>;
    fn set_session_title(&self, id: &str, title: &str) -> Result<(), BoughError>;
    fn set_session_workspace(&self, id: &str, workspace: &str) -> Result<(), BoughError>;
    fn set_session_base(&self, id: &str, base: &str) -> Result<(), BoughError>;
    fn set_session_draft(&self, id: &str, draft: Option<&str>) -> Result<(), BoughError>;
    fn set_session_model(&self, id: &str, model: Option<&str>) -> Result<(), BoughError>;
    fn set_session_effort(&self, id: &str, effort: Option<&str>) -> Result<(), BoughError>;
    /// Records whether the delegated TURN errored. Not an acceptance gate.
    fn set_session_outcome(&self, id: &str, ok: bool) -> Result<(), BoughError>;
    /// Cost columns ACCUMULATE; the context gauge OVERWRITES.
    fn add_session_usage(&self, id: &str, usage: &Usage, at: i64) -> Result<(), BoughError>;
    /// Session row totals, NULLs → 0; unknown id → all zeros.
    fn session_usage(&self, id: &str) -> Result<UsageTotals, BoughError>;
    /// The session plus every branch collapsed under it (`subagent`/
    /// `workflow_agent` only — forks are siblings).
    fn tree_usage(&self, id: &str) -> Result<UsageTotals, BoughError>;
    /// Sessions with a `running` turn — read from turns, NOT pending messages.
    fn busy_session_ids(&self) -> Result<HashSet<String>, BoughError>;

    // messages
    fn create_message(&self, message: Message) -> Result<Message, BoughError>;
    fn get_message(&self, id: &str) -> Result<Option<Message>, BoughError>;
    /// The session's OWN messages, ordered `(created_at, rowid)`.
    fn messages_for(&self, session_id: &str) -> Result<Vec<Message>, BoughError>;
    /// Ancestors root→parent, then own. The full replayable thread.
    fn thread_for(&self, session_id: &str) -> Result<Vec<Message>, BoughError>;
    /// Wholesale parts overwrite; the turn runner streams into this every round.
    fn update_message(&self, id: &str, parts: &[Part], pending: bool) -> Result<(), BoughError>;
    /// Delete a message and every message after it in its session, in ONE
    /// transaction, returning the ids removed. The only destructive thread write.
    fn delete_messages_from(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Vec<String>, BoughError>;

    // turns
    fn create_turn(&self, turn: Turn) -> Result<Turn, BoughError>;
    fn get_turn(&self, id: &str) -> Result<Option<Turn>, BoughError>;
    /// Most recently touched wins (`updated_at DESC, rowid DESC LIMIT 1`).
    fn turn_for_message(&self, message_id: &str) -> Result<Option<Turn>, BoughError>;
    fn turns_for_session(&self, session_id: &str) -> Result<Vec<Turn>, BoughError>;
    /// Boot recovery reads `running` here and orphans every row it finds.
    fn turns_by_status(&self, status: TurnStatus) -> Result<Vec<Turn>, BoughError>;
    fn latest_turn_statuses(&self) -> Result<HashMap<String, TurnStatus>, BoughError>;
    /// Stamps `updated_at` from the injected clock on EVERY call; `usage`
    /// REPLACES; missing id → silent no-op.
    fn update_turn(&self, id: &str, patch: TurnPatch) -> Result<(), BoughError>;

    // durable KV, scoped to the lineage root
    fn get_state(&self, root_id: &str, key: &str) -> Result<Option<String>, BoughError>;
    fn set_state(&self, root_id: &str, key: &str, value: &str, now: i64) -> Result<(), BoughError>;
    /// Byte lengths only, ordered by key — a listing must never drag whole
    /// values into context.
    fn list_state(&self, root_id: &str) -> Result<Vec<StateEntry>, BoughError>;
    /// True iff a row existed.
    fn delete_state(&self, root_id: &str, key: &str) -> Result<bool, BoughError>;

    // schedules
    fn create_schedule(&self, schedule: Schedule) -> Result<Schedule, BoughError>;
    fn get_schedule(&self, id: &str) -> Result<Option<Schedule>, BoughError>;
    fn list_schedules(&self) -> Result<Vec<Schedule>, BoughError>;
    /// Enabled schedules whose `next_run_at` has passed, soonest first.
    fn due_schedules(&self, now: i64) -> Result<Vec<Schedule>, BoughError>;
    /// Overwrites title/prompt/workspace/spec/enabled/last_run_at/next_run_at
    /// (NOT session_id, NOT created_at); caller merges PATCH into the full row.
    fn update_schedule(&self, schedule: &Schedule) -> Result<(), BoughError>;
    /// Advances `next_run_at` FROM NOW, never from the stale value.
    fn mark_schedule_run(
        &self,
        id: &str,
        last_run_at: i64,
        next_run_at: i64,
    ) -> Result<(), BoughError>;
    fn delete_schedule(&self, id: &str) -> Result<(), BoughError>;

    // workflows
    fn create_workflow(&self, run: WorkflowRun) -> Result<WorkflowRun, BoughError>;
    fn get_workflow(&self, id: &str) -> Result<Option<WorkflowRun>, BoughError>;
    /// No arg: all, newest first. With arg: the session-graph walk (follow
    /// `parent_id` always; `origin_id` only for fork/compaction kinds).
    fn list_workflows(&self, session_id: Option<&str>) -> Result<Vec<WorkflowRun>, BoughError>;
    /// Runs still `running`/`paused` at boot — orphaned like turns.
    fn unfinished_workflows(&self) -> Result<Vec<WorkflowRun>, BoughError>;
    fn update_workflow(&self, id: &str, patch: WorkflowPatch) -> Result<(), BoughError>;
    /// The `schema` column is always inserted NULL; the wire shape has no
    /// schema field (the JSON Schema is part of what `key` hashes).
    fn create_workflow_agent(&self, agent: WorkflowAgent) -> Result<WorkflowAgent, BoughError>;
    fn update_workflow_agent(&self, id: &str, patch: WorkflowAgentPatch) -> Result<(), BoughError>;
    /// `ORDER BY idx, rowid`.
    fn list_workflow_agents(&self, run_id: &str) -> Result<Vec<WorkflowAgent>, BoughError>;
    /// Journal lookup on rerun: first call wins (`idx, rowid LIMIT 1`).
    fn find_workflow_agent(
        &self,
        run_id: &str,
        key: &str,
    ) -> Result<Option<WorkflowAgent>, BoughError>;

    // command-history memory
    /// Append one finished command + its tag/dir junction rows + FTS row, in
    /// ONE transaction.
    fn record_command(&self, record: &CommandRecord) -> Result<(), BoughError>;
    /// The (tag, ts, exit_code) observations, scoped to a repo, optionally to
    /// commands attributed to `dir` or its DESCENDANTS (not name prefixes).
    fn command_tag_rows(
        &self,
        repo: &str,
        opts: CommandTagOpts,
    ) -> Result<Vec<CommandTagRow>, BoughError>;
    /// Distinct repos in the memory, and how many of them use each tag.
    fn tag_spread(&self, since_ts: Option<i64>) -> Result<(i64, HashMap<String, i64>), BoughError>;
    /// Per-day tag coverage and vocabulary size, day DESC, local time.
    fn tag_diversity_by_day(
        &self,
        since_ts: i64,
        repo: Option<&str>,
    ) -> Result<Vec<TagDiversityDay>, BoughError>;
    /// Commands recorded under one tag, newest first.
    fn commands_for_tag(
        &self,
        tag: &str,
        repo: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<TaggedCommand>, BoughError>;
    /// Commands whose text, tags OR printed output match an FTS query,
    /// newest first. The query is a bag of words, not FTS syntax — the
    /// implementation quotes them, so an operator a user typed is a word.
    fn search_commands(
        &self,
        repo: &str,
        words: &[String],
        limit: i64,
    ) -> Result<Vec<TaggedCommand>, BoughError>;
    /// This repo's coined tags (references excluded) and their use counts.
    fn repo_tag_counts(
        &self,
        repo: &str,
        since_ts: i64,
    ) -> Result<HashMap<String, i64>, BoughError>;
    /// How this exact command has failed in this repo since `since_ts`, or None.
    fn prior_failures(
        &self,
        repo: &str,
        cmd: &str,
        since_ts: i64,
        session_id: &str,
    ) -> Result<Option<PriorFailures>, BoughError>;
    /// Recent failing commands in this repo, newest first.
    fn recent_failures(
        &self,
        repo: &str,
        since_ts: i64,
        limit: i64,
    ) -> Result<Vec<RecentFailure>, BoughError>;
    /// The newest successful command starting with a LIKE `prefix`
    /// (pre-escaped, `\` escape char), excluding `not_cmd`.
    fn last_success_like(
        &self,
        repo: &str,
        prefix: &str,
        not_cmd: &str,
        since_ts: i64,
    ) -> Result<Option<String>, BoughError>;
    /// The `run_steps` program a supervisor message ran, or None. A corrupt
    /// parts row is not a crash for a reader — swallow the parse error.
    fn program_for_message(&self, message_id: &str) -> Result<Option<String>, BoughError>;

    // keyword search
    /// Idempotent: re-indexing a message replaces its rows. Only `text` and
    /// `reasoning` parts index; empty projection → no row at all.
    fn index_message(&self, message: &Message) -> Result<(), BoughError>;
    /// An FTS5 syntax error is a `BadRequestError` (400) whose message quotes
    /// the query, names FTS5, and carries the quote-a-phrase hint.
    fn search_messages(
        &self,
        query: &str,
        session_id: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<SearchHit>, BoughError>;
    /// Must produce results identical to incremental indexing.
    fn rebuild_search_index(&self) -> Result<(), BoughError>;

    /// The swallowed-index-error record, when this handle is the search-safe
    /// wrapper (`bough-server::search::SearchSafeDb`). The raw handle answers
    /// `None` — a healthy default, which is also what keeps the wrapper
    /// installable at boot without anything downstream telling the difference.
    fn index_health(&self) -> Option<IndexHealth> {
        None
    }
    /// Clear the swallowed-error record — called after a rebuild has actually
    /// repaired the drift. No-op on the raw handle.
    fn heal_search_index(&self) {}

    fn close(&self);
}

/// How the tree shares the one live db: rusqlite `Connection` is `!Sync`, so
/// `SqliteDb` lives behind a mutex; contention is negligible (single-user
/// local server; the TS is fully sync here too).
pub type SharedDb = Arc<Mutex<dyn Db>>;

// ---- the LLM boundary -------------------------------------------------------

/// Thinking depth. Not every model accepts one; an unsupported value is a turn
/// error.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// A content block as the model produces it. `meta` on `Reasoning` is an
/// opaque provider payload replayed VERBATIM — never inspected outside the
/// provider's own mapper. Note the wire asymmetry vs persisted parts:
/// `toolUseId`/`content` here, `callId`/`output` there. Two types, never
/// unified.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LlmBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
}

/// A block as it appears in a request message.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LlmContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// Base64-encoded at assembly time; each provider maps it to its native shape.
    Image {
        data: String,
        media_type: String,
        name: String,
    },
}

impl From<LlmBlock> for LlmContentBlock {
    fn from(b: LlmBlock) -> Self {
        match b {
            LlmBlock::Text { text } => LlmContentBlock::Text { text },
            LlmBlock::Reasoning { text, meta } => LlmContentBlock::Reasoning { text, meta },
            LlmBlock::ToolUse { id, name, input } => LlmContentBlock::ToolUse { id, name, input },
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LlmRole {
    User,
    Assistant,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: Vec<LlmContentBlock>,
}

/// The model sees exactly two of these: `run_steps` and `stop`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LlmToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LlmParams {
    pub model: String,
    /// The STABLE system prefix. Prompt-cache contract: byte-identical across
    /// sessions and turns per delegation tier.
    pub system: Option<String>,
    /// The per-session suffix, sent after `system` with its own cache
    /// breakpoint.
    pub system_volatile: Option<String>,
    pub max_tokens: i64,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<LlmToolDef>,
    /// `Some(ToolChoiceNone)` forbids tool calls for this round, forcing plain
    /// text — the runner's last resort.
    pub tool_choice_none: bool,
    pub effort: Option<Effort>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LlmResult {
    pub content: Vec<LlmBlock>,
    pub stop_reason: String,
    pub usage: Option<Usage>,
}

/// Streamed text deltas as they arrive.
pub type OnText = Arc<dyn Fn(&str) + Send + Sync>;

/// The whole provider surface. The turn runner must not know which provider it
/// is talking to — if provider-specific handling leaks past this interface, it
/// leaks everywhere.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// One round. Cancelling `cancel` aborts the in-flight request; the caller
    /// treats the resulting abort as an interrupt.
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        cancel: CancellationToken,
    ) -> Result<LlmResult, BoughError>;
}

/// The cheap tier: auto titles, composer ghost text, live activity blurbs.
/// Each must fail silently — `None` on failure, NEVER an error. One in-flight
/// blurb per session: drop, don't queue. v1 ships `cheap: None` — every reader
/// degrades on absence by contract.
#[async_trait::async_trait]
pub trait CheapTier: Send + Sync {
    async fn title(&self, first_message: &str) -> Option<String>;
    async fn ghost_text(&self, prefix: &str) -> Option<String>;
    async fn activity(&self, recent: &str) -> Option<String>;
}

// ---- the turn starter -------------------------------------------------------

/// The ONE composed turn starter, wired at boot (step 6) and read off the ctx
/// by everything that starts turns (postMessage, schedules, notes). It decides
/// when a turn actually starts (a post into a busy session must queue) and
/// never blocks the caller — the response is a 202 and the turn outlives it.
pub trait TurnStarter: Send + Sync {
    fn start_turn(&self, ctx: &AppCtx, session: &Session, message: &Message);
}

// ---- host state -------------------------------------------------------------

/// The memory-only registries that in TS were module statics — built once at
/// boot, `Arc`-cloned into each turn. The TS statics existed only because
/// `TurnCtx` was frozen; here they live on the ctx.
#[derive(Clone)]
pub struct HostState {
    pub jobs: Arc<JobRegistry>,
    pub snapshots: Arc<SnapshotStore>,
    pub writes: Arc<WriteLog>,
    pub asks: Arc<AskRegistry>,
    pub detached: Arc<DetachedSubagents>,
    pub caps: Arc<SpawnCaps>,
}

impl HostState {
    pub fn new() -> Self {
        HostState {
            jobs: Arc::new(JobRegistry::new()),
            snapshots: Arc::new(SnapshotStore::new()),
            writes: Arc::new(WriteLog::new()),
            asks: Arc::new(AskRegistry::new()),
            detached: Arc::new(DetachedSubagents::new()),
            caps: Arc::new(SpawnCaps::new()),
        }
    }
}

impl Default for HostState {
    fn default() -> Self {
        Self::new()
    }
}

// ---- application contexts ---------------------------------------------------

/// What every HTTP handler receives. Built once at boot; the same object is
/// handed to all of them.
#[derive(Clone)]
pub struct AppCtx {
    pub db: SharedDb,
    pub bus: Arc<Bus>,
    /// Injected in tests; production wires the provider-routed client.
    pub llm: Option<Arc<dyn LlmClient>>,
    /// Absent = the global default (env, then the built-in).
    pub model: Option<String>,
    pub effort: Option<Effort>,
    /// Injected clock. Pure core takes this, never the global.
    pub now: Clock,
    /// Absent in tests and v1 — every cheap-tier feature degrades to nothing.
    pub cheap: Option<Arc<dyn CheapTier>>,
    /// The memory-only registries (jobs, snapshots, asks, caps…).
    pub host: Arc<HostState>,
    /// Set after boot wiring (the ctx is built before the starter exists).
    pub starter: Arc<RwLock<Option<Arc<dyn TurnStarter>>>>,
    /// The one-turn-per-session claim table (TS: the process-wide registry in
    /// `turn/queue.ts`, injected via `WithTurnRegistry`). On the ctx in Rust —
    /// the TS module-static existed only because `AppCtx` was frozen.
    pub turn_registry: Arc<TurnRegistry>,
    /// Where `~/.bough/model.json` is read from (TS `WithModelDefaults`).
    /// `None` = the real `paths::model_settings_path()`; tests inject a temp
    /// path so nothing reads the developer's own pins.
    pub model_defaults_path: Option<std::path::PathBuf>,
}

impl AppCtx {
    /// The composed turn starter, when boot has wired one.
    pub fn turn_starter(&self) -> Option<Arc<dyn TurnStarter>> {
        self.starter.read().ok().and_then(|g| g.clone())
    }
}

/// One command that exited non-zero during this turn.
#[derive(Clone, Debug, PartialEq)]
pub struct ExitNote {
    pub command: String,
    pub code: i64,
}

/// One finished command as the shell layer reports it to the recorder.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedCommand {
    pub command: String,
    pub tags: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub output_head: String,
    pub spill_path: Option<String>,
}

/// Where finished shell commands enter the tag-history memory.
pub type CommandRecorder = Arc<dyn Fn(RecordedCommand) + Send + Sync>;

/// What a running turn — and therefore every host function — receives. Host
/// functions take this and nothing else, which is what keeps `hostfn` free of
/// any reference to the server crate.
///
/// The shared arrays (`exits`, `reads`, `touched`) are ON the ctx precisely
/// because host fns are built from it in two places (`baseHostFns` and
/// `delegationDeps`); a closure-local version was silently bypassed and
/// shipped green tests that did nothing live.
#[derive(Clone)]
pub struct TurnCtx {
    pub app: AppCtx,
    pub session_id: String,
    pub turn_id: String,
    /// The pending supervisor message the turn is producing.
    pub message_id: String,
    /// Resolved at turn start; subagents share it — one checkout, no worktrees.
    pub workspace: String,
    pub model: String,
    /// The turn's interrupt. Child tokens = cascade.
    pub cancel: CancellationToken,
    /// Commands that exited non-zero during this turn, recorded by `bash()`.
    pub exits: Arc<Mutex<Vec<ExitNote>>>,
    /// Where finished shell commands enter the tag-history memory. None in
    /// contexts that do not record.
    pub record: Option<CommandRecorder>,
    /// Absolute paths the turn's programs viewed, appended by `view()`.
    pub reads: Arc<Mutex<Vec<String>>>,
    /// Absolute directories the turn's shell commands were about.
    pub touched: Arc<Mutex<Vec<String>>>,
    /// MCP servers inherited from the spawning turn, captured at spawn time.
    pub mcp_grant: Option<crate::mcp::manager::McpGrant>,
    /// Delegation depth. 0 = top level (may `spawn` and start workflows);
    /// 1 = a subagent, which may delegate one level further, blocking only.
    pub depth: u8,
}

// ---- host functions ---------------------------------------------------------

/// One bridged host call: JSON-string args in (in protocol order), string out.
/// The postMessage wire is string-only; `view`/`patch` text IS the format.
pub type HostFn = Arc<
    dyn Fn(Vec<String>) -> futures::future::BoxFuture<'static, Result<String, BoughError>>
        + Send
        + Sync,
>;

/// The host side of the program bridge. Shell and file verbs are always wired
/// and therefore required; optionality is the capability grant — a function
/// the turn does not bridge is simply absent, and calling it rejects with
/// "unknown host function".
#[derive(Clone, Default)]
pub struct HostFns {
    /// Combined output; carries the turn's interrupt; auto-backgrounds past
    /// 60s. `tags` is REQUIRED and ALSO enforced at runtime with a catchable
    /// teaching error.
    pub bash: Option<HostFn>,
    /// Concurrent shells; never throws on a non-zero exit — the code is data.
    pub sh: Option<HostFn>,
    /// Explicit background shell outliving the turn. NAME first, required.
    pub bash_bg: Option<HostFn>,
    pub bash_output: Option<HostFn>,
    pub bash_wait: Option<HostFn>,
    pub bash_kill: Option<HostFn>,
    /// `[path#TAG]` header plus numbered `N:text` lines.
    pub view: Option<HostFn>,
    /// Hash-anchored line edits; multi-file patches apply all-or-none.
    pub patch: Option<HostFn>,
    /// New files and wholesale rewrites. There is no `read()` and no `edit()`.
    pub write: Option<HostFn>,
    /// Blocking subagent — `{sessionId, ok, report, changedFiles}` as JSON.
    pub agent: Option<HostFn>,
    /// Detached subagent — `{sessionId, title}` as JSON, immediately.
    pub spawn: Option<HostFn>,
    /// Claim a detached subagent's result in-band.
    pub join: Option<HostFn>,
    /// Take over a subagent's session.
    pub adopt: Option<HostFn>,
    /// Verb-dispatched: start/rerun/stop/pause/resume/status/list.
    pub workflow: Option<HostFn>,
    /// Park the program and ask the human; rejects catchably with "user
    /// declined" on dismissal.
    pub ask: Option<HostFn>,
    /// Verb-dispatched: get/set/list/delete. Lineage root scoped, 16KB/key.
    pub state: Option<HostFn>,
    /// Verb-dispatched: list/add/enable/disable/remove.
    pub schedule: Option<HostFn>,
    /// Publish a file for browser viewing; returns `{url, href}`.
    pub artifact: Option<HostFn>,
}

impl HostFns {
    /// The dispatcher — and the exhaustive-match pin that `HostFns` and
    /// `HOST_FN_NAMES` agree (the Rust replacement for TS's `UnboundHostFn`
    /// compile-time proof). No default arm: a new protocol name fails to
    /// compile here.
    pub fn get(&self, name: HostFnName) -> Option<&HostFn> {
        match name {
            HostFnName::Bash => self.bash.as_ref(),
            HostFnName::Sh => self.sh.as_ref(),
            HostFnName::BashBg => self.bash_bg.as_ref(),
            HostFnName::BashOutput => self.bash_output.as_ref(),
            HostFnName::BashWait => self.bash_wait.as_ref(),
            HostFnName::BashKill => self.bash_kill.as_ref(),
            HostFnName::View => self.view.as_ref(),
            HostFnName::Patch => self.patch.as_ref(),
            HostFnName::Write => self.write.as_ref(),
            HostFnName::Agent => self.agent.as_ref(),
            HostFnName::Spawn => self.spawn.as_ref(),
            HostFnName::Join => self.join.as_ref(),
            HostFnName::Adopt => self.adopt.as_ref(),
            HostFnName::Workflow => self.workflow.as_ref(),
            HostFnName::Ask => self.ask.as_ref(),
            HostFnName::State => self.state.as_ref(),
            HostFnName::Schedule => self.schedule.as_ref(),
            HostFnName::Artifact => self.artifact.as_ref(),
        }
    }
}

/// The workflow worker's bridge. Three verbs, `permissions: "none"`, nothing
/// else. `parallel` and `pipeline` are pure combinators over `agent` and live
/// worker-side.
#[derive(Clone, Default)]
pub struct WorkflowHostFns {
    /// Runs a subagent and returns its report. Fails on failure.
    pub agent: Option<HostFn>,
    /// Fire-and-forget progress. Never blocks.
    pub phase: Option<HostFn>,
    pub log: Option<HostFn>,
}

// ---- re-exports -------------------------------------------------------------
// One import for consumers that need the ctx and the shapes it carries.

pub use crate::schema::parts::{
    AskQuestion as AskQuestionShape, Message as MessageShape, Part as PartShape,
    Session as SessionShape, Turn as TurnShape, Usage as UsageShape,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::protocol::HOST_FN_NAMES;

    #[test]
    fn patch_deserializes_tri_state() {
        #[derive(Deserialize, Default)]
        struct Body {
            #[serde(default)]
            error: Patch<String>,
        }
        let b: Body = serde_json::from_str("{}").unwrap();
        assert!(b.error.is_keep());
        let b: Body = serde_json::from_str(r#"{"error":null}"#).unwrap();
        assert_eq!(b.error, Patch::Clear);
        let b: Body = serde_json::from_str(r#"{"error":"boom"}"#).unwrap();
        assert_eq!(b.error, Patch::Set("boom".into()));
    }

    #[test]
    fn patch_serializes_clear_as_null_and_skips_keep() {
        #[derive(Serialize, Default)]
        struct Body {
            #[serde(skip_serializing_if = "Patch::is_keep")]
            error: Patch<String>,
        }
        assert_eq!(
            serde_json::to_string(&Body { error: Patch::Keep }).unwrap(),
            "{}"
        );
        assert_eq!(
            serde_json::to_string(&Body {
                error: Patch::Clear
            })
            .unwrap(),
            r#"{"error":null}"#
        );
        assert_eq!(
            serde_json::to_string(&Body {
                error: Patch::Set("x".into())
            })
            .unwrap(),
            r#"{"error":"x"}"#
        );
    }

    #[test]
    fn patch_apply_semantics() {
        assert_eq!(Patch::<i32>::Keep.apply(Some(1)), Some(1));
        assert_eq!(Patch::<i32>::Clear.apply(Some(1)), None);
        assert_eq!(Patch::Set(2).apply(Some(1)), Some(2));
    }

    #[test]
    fn host_fns_dispatch_covers_every_protocol_name() {
        // The Rust replacement for the TS compile-time drift proof: every wire
        // name parses to a HostFnName, and `get` matches it exhaustively.
        let fns = HostFns::default();
        assert_eq!(HOST_FN_NAMES.len(), 18);
        for name in HOST_FN_NAMES {
            let parsed = HostFnName::parse(name)
                .unwrap_or_else(|| panic!("protocol name {name} not in HostFnName"));
            assert_eq!(parsed.as_str(), name);
            assert!(fns.get(parsed).is_none()); // default = nothing granted
        }
    }

    #[test]
    fn llm_content_block_wire_names() {
        let b = LlmContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "ok".into(),
            is_error: false,
        };
        let s = serde_json::to_string(&b).unwrap();
        // The LLM wire uses toolUseId/content — NOT the persisted callId/output.
        assert!(s.contains(r#""toolUseId":"t1""#), "{s}");
        assert!(s.contains(r#""type":"tool_result""#), "{s}");
    }

    #[test]
    fn persisted_and_llm_wire_tool_results_are_two_types_never_unified() {
        // Same tag, different field names, by design: persisted parts carry
        // `callId`/`output`; LLM request blocks carry `toolUseId`/`content`.
        let persisted = Part::ToolResult {
            call_id: "t1".into(),
            output: Value::String("ok".into()),
            is_error: false,
            interrupted: None,
        };
        let ps = serde_json::to_string(&persisted).unwrap();
        assert!(ps.contains(r#""callId":"t1""#), "{ps}");
        assert!(ps.contains(r#""output":"ok""#), "{ps}");

        let wire = LlmContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "ok".into(),
            is_error: false,
        };
        let ws = serde_json::to_string(&wire).unwrap();
        assert!(ws.contains(r#""toolUseId":"t1""#), "{ws}");
        assert!(ws.contains(r#""content":"ok""#), "{ws}");

        // Neither shape parses as the other — the asymmetry is load-bearing.
        assert!(serde_json::from_str::<LlmContentBlock>(&ps).is_err());
        assert!(serde_json::from_str::<Part>(&ws).is_err());
    }

    #[test]
    fn effort_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Effort::Xhigh).unwrap(), r#""xhigh""#);
    }
}
