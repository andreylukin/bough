//! The wire contract (port of `src/schema/parts.ts`).
//!
//! The invariant this module holds is *derived visibility*: a Session carries its
//! lineage (`kind`, `parentId`, `originId`) and nothing else. There is no
//! `archivedAt`, no `deprecatedAt`, no hidden flag — a subagent collapses under
//! its origin because of what it IS, not because something marked it.
//!
//! Second invariant: parts are a discriminated union on `type` — the closed
//! 7-variant set. The UI switches on it exhaustively and replay maps each arm to
//! a provider block.
//!
//! Third: image bytes never live in the parts JSON. An `image` part stores a
//! path under `~/.bough/attachments/`.
//!
//! Parsing rule: unknown keys are STRIPPED, never rejected — `deny_unknown_fields`
//! is wrong here (freeze test: `archivedAt` must not survive parsing, but must
//! not reject either). Serde's default ignore-unknown behavior is the contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- roles & kinds ---------------------------------------------------------

/// `system` = harness-injected notes (a detached subagent's report, a background
/// job's exit, artifact comments). They render distinctly in the UI and replay
/// to the model as user-side text — they are not a provider role.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Supervisor,
    System,
}

/// Visibility is derived from this plus lineage: the collapsing kinds sit under
/// their `originId` and surface only on drill-in; `root`, `fork`, `compaction`
/// and `shell` are always listed.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Root,
    Fork,
    Compaction,
    Subagent,
    WorkflowAgent,
    /// One firing of a schedule — a real session with a fresh, task-only
    /// thread, hung off the conversation that CREATED the schedule.
    ScheduleRun,
    /// The conversation a `!` command runs in when none is open. Not collapsed.
    /// One per workspace, reused.
    Shell,
}

/// The kinds that collapse under their `originId` and are reached by drill-in.
/// Canonical HERE — three TS modules had drifted into three literals of this list.
pub const COLLAPSED_KINDS: [SessionKind; 3] = [
    SessionKind::Subagent,
    SessionKind::WorkflowAgent,
    SessionKind::ScheduleRun,
];

/// True when a session of this kind surfaces only under its origin.
pub fn is_collapsed_kind(kind: SessionKind) -> bool {
    COLLAPSED_KINDS.contains(&kind)
}

/// The kinds that are DELEGATED work — a program asked for them, inside a turn.
/// A narrower question than [`is_collapsed_kind`], and the two must not be
/// merged: a schedule firing collapses like a subagent, but it is not delegation.
pub const DELEGATED_KINDS: [SessionKind; 2] = [SessionKind::Subagent, SessionKind::WorkflowAgent];

/// True when a program spawned this session as part of a turn.
pub fn is_delegated_kind(kind: SessionKind) -> bool {
    DELEGATED_KINDS.contains(&kind)
}

// ---- parts -----------------------------------------------------------------

/// A settled `ask()` hold's status. Never `pending`: an [`Part::Ask`] is
/// appended only once resolved, so replay can never re-block.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AskStatus {
    Answered,
    Declined,
    Interrupted,
}

/// The seven part kinds. Discriminated on `type`, snake_case tags, camelCase
/// fields — the persisted-part asymmetry vs LLM wire blocks (`callId`/`output`
/// here, `toolUseId`/`content` there) is deliberate; the two are never unified.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Part {
    /// Model prose. The only part kind a turn is *required* to produce.
    Text { text: String },
    /// Summarized thinking — persisted for display AND, when `meta` carries a
    /// provider signature, replayed across turns. `meta` is the provider's own
    /// block verbatim, never rendered and never inspected outside the
    /// provider's own mapper. `model` gates replay (signatures are
    /// model-scoped). Both absent on old rows.
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// One `run_steps` / `stop` call. `input` is unknown here; the tool's own
    /// schema validates it.
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    /// `interrupted` means the call was stopped by a user interrupt rather than
    /// completing — `output` holds whatever partial work survived. Distinct
    /// from `isError`, and rendered distinctly.
    ToolResult {
        call_id: String,
        #[serde(default)]
        output: Value,
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupted: Option<bool>,
    },
    /// An image the model can see. The bytes live at `path` under
    /// `~/.bough/attachments/`, never inline. A lost file replays as
    /// placeholder text rather than failing the turn.
    Image {
        path: String,
        media_type: String,
        name: String,
        size: i64,
    },
    /// A SETTLED `ask()` hold. Appended only once resolved — never while
    /// pending. `id` joins the row to the `ask.question` events.
    Ask {
        id: String,
        question: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<Vec<String>>,
        status: AskStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
    },
    /// A workflow run LAUNCHED from this turn. Three identity fields and no
    /// status on purpose: the card reads the run row live by `id`.
    Workflow {
        id: String,
        name: String,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rerun_of: Option<String>,
    },
}

// ---- messages --------------------------------------------------------------

/// `pending` is the streaming flag: a supervisor message is created pending and
/// flipped when `message.finished` lands. Ordering is `(createdAt, rowid)`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: Role,
    pub parts: Vec<Part>,
    pub pending: bool,
    pub created_at: i64,
}

// ---- sessions --------------------------------------------------------------

/// One conversation. Note what is absent: no archive, deprecate, hide or purge
/// field. Visibility is derived from `kind` + `originId`. Parsing STRIPS unknown
/// keys (`archivedAt` must not survive, must not reject).
///
/// **Every optional serializes, as `null` when empty** — no `skip_serializing_if`
/// on this struct. `db.ts::toSession` states the invariant: "Absent optionals
/// come back as `null`, never `undefined`: one shape per row." Omitting the key
/// instead would give a session row two wire shapes depending on its contents,
/// and the parity harness diffs this struct against the TS server field by
/// field. `#[serde(default)]` keeps an absent key parsing as `None` on input.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub title: String,
    pub kind: SessionKind,
    pub created_at: i64,
    /// Thread inheritance. Fork and compaction parent at the TARGET's parent.
    /// A subagent has `parentId: null` — a fresh, task-only thread.
    pub parent_id: Option<String>,
    /// Lineage edge for the tree view: what this branched from, at which message.
    #[serde(default)]
    pub origin_id: Option<String>,
    #[serde(default)]
    pub origin_message_id: Option<String>,
    /// The checkout the session operates on, edited in place.
    #[serde(default)]
    pub workspace: Option<String>,
    /// The project directory the session was created on; never rewritten.
    #[serde(default)]
    pub origin_dir: Option<String>,
    /// The git sha the session started from. Absent for a non-git workspace.
    #[serde(default)]
    pub base: Option<String>,
    /// Per-session pins; absent = the global default.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// Prefilled composer text, set by handoff. Cleared server-side by the first post.
    #[serde(default)]
    pub draft: Option<String>,
    /// GAUGE — last round only; the client derives cache warmth from
    /// `lastLlmAt` + TTL, not a stored boolean.
    #[serde(default)]
    pub context_tokens: Option<i64>,
    #[serde(default)]
    pub cached_tokens: Option<i64>,
    #[serde(default)]
    pub last_llm_at: Option<i64>,
    /// Whether the delegated TURN errored; no acceptance gate.
    #[serde(default)]
    pub outcome_ok: Option<bool>,
}

// ---- turns -----------------------------------------------------------------

/// `orphaned` is what a `running` turn becomes when the server restarts under
/// it: the session unblocks instead of hanging on a pending message forever.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TurnStatus {
    Running,
    Done,
    Error,
    Interrupted,
    Orphaned,
}

/// Per-round provider usage, summed across the turn and aggregated per session.
/// Owned by `bough-llm`; re-exported here so the persisted shape and the wire
/// shape stay one type.
pub use bough_llm::types::Usage;

/// The persisted state machine covering everything after a user message lands.
/// Checkpointed as it progresses (`step`) so a restart can find turns still
/// marked `running` and orphan them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub session_id: String,
    /// The pending supervisor message this turn is producing.
    pub message_id: String,
    pub status: TurnStatus,
    /// Last checkpoint, human-readable.
    pub step: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Present when status is `error`; names the limit or the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `None` until a round reported usage — zeros would be "a claim we cannot make".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

// ---- ask() questions -------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AskQuestionStatus {
    Pending,
    Answered,
    Declined,
    Interrupted,
}

/// One live `ask()` hold. Memory-only server-side ("the hold dies with the
/// turn") — the durable record is the settled [`Part::Ask`] on the supervisor
/// message.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AskQuestion {
    pub id: String,
    pub session_id: String,
    /// The supervisor message whose turn raised it — the transcript anchor.
    pub message_id: String,
    pub question: String,
    /// Pick-one choices; absent = free text only. Free text is always possible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    pub status: AskQuestionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    pub ts: i64,
}

// ---- background jobs -------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Running,
    Exited,
}

/// An auto-backgrounded (`bash` past 60s) or explicit (`bashBg`) shell. NOT
/// persisted: a job's process dies with the server, so a stored row would
/// always be a lie after a restart.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundJob {
    pub id: String,
    /// Defaulted rather than required on the wire so a job row from an older
    /// server parses as unnamed instead of failing the whole response.
    #[serde(default)]
    pub name: String,
    pub session_id: String,
    pub pid: i64,
    pub command: String,
    pub status: JobStatus,
    /// THE THREE OUTCOME FIELDS SERIALIZE AS `null`, they are not skipped. TS
    /// declares them `.nullish()` and its rows carry the keys from the moment a
    /// job spawns — `job.spawned` on that server is a ten-key object with three
    /// nulls in it. Skipping them here made `job.spawned` a seven-key object,
    /// which is a different wire: a client that asks whether the key is present
    /// (`"signal" in job`, `Object.keys`, a schema with no default) reads a
    /// Rust-served job differently from a TS-served one. `default` stays, so a
    /// row from a server that DID omit them still parses.
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// The signal that ended it, when one did — `"SIGTERM"` for a job the user
    /// killed. `exitCode` is null for a signalled process, and without this a
    /// user-killed shell misread as `✓ done`.
    #[serde(default)]
    pub signal: Option<String>,
    pub started_at: i64,
    #[serde(default)]
    pub exited_at: Option<i64>,
}

// ---- schedules -------------------------------------------------------------

/// A recurring run. `nextRunAt` advances FROM NOW at fire time, never from the
/// stale stored value — a server down through N missed slots fires once on the
/// first tick after boot, then resumes cadence.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub workspace: Option<String>,
    /// The conversation that created it — where each firing's outcome is
    /// posted back as a system note. Null = created outside any conversation
    /// (REST), so the firing reports to nobody. Defaulted so a row from an
    /// older server still parses.
    #[serde(default)]
    pub session_id: Option<String>,
    /// `every:<N><m|h|d>` (N ≥ 1) or `daily@HH:MM` (local wall clock). Stored verbatim.
    pub spec: String,
    pub enabled: bool,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub next_run_at: i64,
}

// ---- workflows -------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowStatus {
    Running,
    Paused,
    Done,
    Error,
    Stopped,
    Orphaned,
}

/// From the script's `meta` literal, extracted host-side before the body runs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPhase {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One detached orchestration run. The script text is persisted verbatim (and
/// mirrored to `~/.bough/workflows/<id>.js`) so a rerun can diff against it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: String,
    pub session_id: String,
    pub name: String,
    pub description: String,
    pub script: String,
    pub phases: Vec<WorkflowPhase>,
    pub status: WorkflowStatus,
    pub current_phase: Option<String>,
    /// The script's return value (status `done`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    pub error: Option<String>,
    /// The run's input value, handed to the script as `args` verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// The run whose journal this rerun replays from.
    pub resume_of: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowAgentStatus {
    Queued,
    Running,
    Done,
    Error,
    Stopped,
    /// Replayed from the source run's journal — no live agent call was made.
    Cached,
}

/// One `agent()` call's journal row — the unit a rerun replays. `key` is
/// `hash(prompt + opts)`. No `schema` field on the wire (the JSON Schema is
/// part of what `key` hashes; the DB column is always written NULL).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgent {
    pub id: String,
    pub run_id: String,
    /// Call order within the run.
    pub idx: i64,
    pub key: String,
    pub label: String,
    pub phase: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub status: WorkflowAgentStatus,
    /// The agent's report — raw text, or the JSON of a `{schema}` call.
    pub result: Option<String>,
    /// Present when the call failed; the message names what went wrong.
    pub error: Option<String>,
    /// The subagent session backing this call. Absent for cached replays.
    pub session_id: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_part_kind_round_trips() {
        // The freeze-suite pin: the seven part kinds, and nothing else. Each
        // raw wire object parses and re-serializes byte-equivalent (camelCase
        // fields, snake_case tags).
        let raws = [
            json!({"type":"text","text":"hi"}),
            json!({"type":"reasoning","text":"thinking"}),
            json!({"type":"tool_call","id":"c1","name":"run_steps","input":{"code":"1"}}),
            json!({"type":"tool_result","callId":"c1","output":"out","isError":false,"interrupted":true}),
            json!({"type":"image","path":"/a/b.png","mediaType":"image/png","name":"b.png","size":12}),
            json!({"type":"ask","id":"q1","question":"which?","options":["a","b"],"status":"answered","answer":"a"}),
            json!({"type":"workflow","id":"w1","name":"sweep","description":"fix bugs","rerunOf":"w0"}),
        ];
        for raw in &raws {
            let part: Part = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(serde_json::to_value(&part).unwrap(), *raw);
        }
        assert_eq!(raws.len(), 7, "the spec names exactly seven part kinds");
    }

    #[test]
    fn part_union_is_closed() {
        // `prose` and `worker` are not part kinds.
        assert!(serde_json::from_str::<Part>(r#"{"type":"prose","text":"x"}"#).is_err());
        assert!(serde_json::from_str::<Part>(r#"{"type":"worker","text":"x"}"#).is_err());
    }

    #[test]
    fn role_and_kind_unions_are_closed() {
        assert!(serde_json::from_str::<Role>(r#""worker""#).is_err());
        assert!(serde_json::from_str::<SessionKind>(r#""worker""#).is_err());
        assert!(serde_json::from_str::<SessionKind>(r#""schedule_run""#).is_ok());
    }

    #[test]
    fn part_round_trips_camel_case() {
        let p = Part::ToolResult {
            call_id: "c1".into(),
            output: Value::String("ok".into()),
            is_error: false,
            interrupted: Some(true),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains(r#""type":"tool_result""#), "{s}");
        assert!(s.contains(r#""callId":"c1""#), "{s}");
        assert!(s.contains(r#""isError":false"#), "{s}");
        let back: Part = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn a_message_carries_an_ordered_parts_array_and_a_pending_flag() {
        let m: Message = serde_json::from_value(json!({
            "id": "m1",
            "sessionId": "s1",
            "role": "supervisor",
            "parts": [{"type": "text", "text": "hi"}],
            "pending": true,
            "createdAt": 1,
        }))
        .unwrap();
        assert_eq!(m.parts.len(), 1);
        assert!(m.pending);
        // `worker` is not a role any more — user | supervisor | system.
        let mut raw = serde_json::to_value(&m).unwrap();
        raw["role"] = json!("worker");
        assert!(serde_json::from_value::<Message>(raw).is_err());
    }

    #[test]
    fn every_session_kind_parses_and_the_union_is_closed() {
        for kind in [
            "root",
            "fork",
            "compaction",
            "subagent",
            "workflow_agent",
            "schedule_run",
            "shell",
        ] {
            let s: Session = serde_json::from_value(json!({
                "id": "s", "title": "t", "kind": kind, "createdAt": 0, "parentId": null,
            }))
            .unwrap();
            assert_eq!(serde_json::to_value(s.kind).unwrap(), json!(kind));
        }
        assert!(serde_json::from_value::<Session>(json!({
            "id": "s", "title": "t", "kind": "worker", "createdAt": 0, "parentId": null,
        }))
        .is_err());
    }

    #[test]
    fn a_turn_can_be_orphaned_and_every_status_round_trips() {
        // Restart recovery depends on `orphaned` existing.
        let t: Turn = serde_json::from_value(json!({
            "id": "t1", "sessionId": "s1", "messageId": "m1", "status": "orphaned",
            "step": "round 2", "createdAt": 1, "updatedAt": 2,
        }))
        .unwrap();
        assert_eq!(t.status, TurnStatus::Orphaned);
        for status in ["running", "done", "error", "interrupted", "orphaned"] {
            let parsed: TurnStatus = serde_json::from_value(json!(status)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), json!(status));
        }
    }

    #[test]
    fn session_parse_strips_unknown_keys() {
        // strip-and-accept, never reject: archivedAt must not survive parsing.
        let json = r#"{"id":"s1","title":"t","kind":"root","createdAt":1,
            "parentId":null,"archivedAt":123,"deprecatedAt":456}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        let out = serde_json::to_value(&s).unwrap();
        assert!(out.get("archivedAt").is_none());
        assert!(out.get("deprecatedAt").is_none());
        assert_eq!(s.id, "s1");
    }

    #[test]
    fn usage_optionals_omit_when_absent() {
        let u = Usage {
            input_tokens: 1,
            output_tokens: 2,
            ..Default::default()
        };
        let s = serde_json::to_string(&u).unwrap();
        assert_eq!(s, r#"{"inputTokens":1,"outputTokens":2}"#);
    }

    #[test]
    fn a_spawned_job_carries_its_outcome_keys_as_null_the_way_ts_does() {
        // The TS server's `job.spawned` is a TEN-key object: `exitCode`,
        // `signal` and `exitedAt` are present and null from the moment the job
        // starts. Skipping them here made it a seven-key object — the same
        // information, a different wire. Caught by event-parity.sh on a turn
        // that ran `bashBg`, which is the only place a job row reaches a
        // client; no unit test could see it, because both sides were
        // self-consistent.
        let job = BackgroundJob {
            id: "j1".into(),
            name: String::new(),
            session_id: "s1".into(),
            pid: 42,
            command: "sleep 1".into(),
            status: JobStatus::Running,
            exit_code: None,
            signal: None,
            started_at: 7,
            exited_at: None,
        };
        let v: serde_json::Value = serde_json::to_value(&job).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        for k in ["exitCode", "signal", "exitedAt"] {
            assert!(keys.contains(&k), "{k} must be on the wire: {keys:?}");
            assert!(v[k].is_null(), "{k} must be null, not {}", v[k]);
        }
        assert_eq!(keys.len(), 10, "{keys:?}");
        // And a row from a server that DID omit them still parses.
        assert_eq!(
            serde_json::from_value::<BackgroundJob>(serde_json::json!({
                "id": "j1", "name": "", "sessionId": "s1", "pid": 42,
                "command": "sleep 1", "status": "running", "startedAt": 7
            }))
            .unwrap(),
            job
        );
    }

    #[test]
    fn background_job_name_defaults_for_old_rows() {
        let json = r#"{"id":"j1","sessionId":"s","pid":1,"command":"sleep 1",
            "status":"running","startedAt":0}"#;
        let j: BackgroundJob = serde_json::from_str(json).unwrap();
        assert_eq!(j.name, "");
    }

    #[test]
    fn schedule_session_id_defaults_null() {
        let json = r#"{"id":"x","title":"t","prompt":"p","workspace":null,
            "spec":"every:30m","enabled":true,"createdAt":0,"lastRunAt":null,"nextRunAt":10}"#;
        let s: Schedule = serde_json::from_str(json).unwrap();
        assert_eq!(s.session_id, None);
    }

    #[test]
    fn collapsed_and_delegated_kinds() {
        assert!(is_collapsed_kind(SessionKind::ScheduleRun));
        assert!(!is_delegated_kind(SessionKind::ScheduleRun));
        assert!(is_delegated_kind(SessionKind::Subagent));
        assert!(!is_collapsed_kind(SessionKind::Shell));
    }
}
