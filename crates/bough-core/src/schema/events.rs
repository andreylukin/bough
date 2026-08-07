//! The SSE envelope and the closed set of event names (port of
//! `src/schema/events.ts`).
//!
//! The invariant: **events are display transport, never the source of truth.**
//! `seq` is a process-monotonic counter that resets on server restart, so it is
//! a dedupe key and NOT a resume cursor. A reconnecting client re-fetches
//! `GET /sessions/:id` and reconciles by message id; nothing replays from a seq.
//!
//! The name list is closed here so the TUI store can match on it exhaustively
//! (no default arm — a new event type must be a compile error). The envelope IS
//! parsed (it comes off a socket); payloads are typed but not wire-validated.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::parts::{Part, TurnStatus};

// ---- the closed event-name set ---------------------------------------------

/// The 16 wire names, frozen. `tool.log` is deliberately included: streaming
/// console output has no other carrier.
pub const EVENT_TYPES: [&str; 16] = [
    "session.created",
    "session.updated",
    "session.activity",
    "message.started",
    "message.delta",
    "message.part",
    "message.finished",
    "message.retry",
    "tool.log",
    "turn.finished",
    "ask.question",
    "job.spawned",
    "job.exited",
    "workflow.updated",
    "workflow.agent",
    "workflow.log",
];

/// The closed 16-name enum. Reducers match exhaustively with no default arm.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventType {
    #[serde(rename = "session.created")]
    SessionCreated,
    #[serde(rename = "session.updated")]
    SessionUpdated,
    #[serde(rename = "session.activity")]
    SessionActivity,
    #[serde(rename = "message.started")]
    MessageStarted,
    #[serde(rename = "message.delta")]
    MessageDelta,
    #[serde(rename = "message.part")]
    MessagePart,
    #[serde(rename = "message.finished")]
    MessageFinished,
    #[serde(rename = "message.retry")]
    MessageRetry,
    #[serde(rename = "tool.log")]
    ToolLog,
    #[serde(rename = "turn.finished")]
    TurnFinished,
    #[serde(rename = "ask.question")]
    AskQuestion,
    #[serde(rename = "job.spawned")]
    JobSpawned,
    #[serde(rename = "job.exited")]
    JobExited,
    #[serde(rename = "workflow.updated")]
    WorkflowUpdated,
    #[serde(rename = "workflow.agent")]
    WorkflowAgent,
    #[serde(rename = "workflow.log")]
    WorkflowLog,
    /// A hook did something. Announced so the TUI can say so as it happens —
    /// a hook that silently rewrites a command is indistinguishable from a
    /// harness that is behaving strangely.
    #[serde(rename = "hook.fired")]
    HookFired,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::SessionCreated => "session.created",
            EventType::SessionUpdated => "session.updated",
            EventType::SessionActivity => "session.activity",
            EventType::MessageStarted => "message.started",
            EventType::MessageDelta => "message.delta",
            EventType::MessagePart => "message.part",
            EventType::MessageFinished => "message.finished",
            EventType::MessageRetry => "message.retry",
            EventType::ToolLog => "tool.log",
            EventType::TurnFinished => "turn.finished",
            EventType::AskQuestion => "ask.question",
            EventType::JobSpawned => "job.spawned",
            EventType::JobExited => "job.exited",
            EventType::WorkflowUpdated => "workflow.updated",
            EventType::WorkflowAgent => "workflow.agent",
            EventType::WorkflowLog => "workflow.log",
            EventType::HookFired => "hook.fired",
        }
    }
}

// ---- the envelope ----------------------------------------------------------

/// Every event carries a process-monotonic `seq` and a `ts`, both stamped by
/// the bus at publish time. `sessionId` is what `GET /events?sessionId=`
/// filters on; events with no session reach every subscriber.
///
/// `data` is untyped on the envelope on purpose — its shape is per-`type`;
/// payloads are deserialized per-type by the consumer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BoughEvent {
    #[serde(rename = "type")]
    pub r#type: EventType,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub seq: u64,
    pub ts: i64,
    #[serde(default)]
    pub data: Value,
}

/// An event as published — everything except the bus-assigned stamp. The bus
/// assigns `seq`/`ts` and returns the stamped [`BoughEvent`].
#[derive(Clone, Debug, PartialEq)]
pub struct EventInput {
    pub r#type: EventType,
    pub session_id: Option<String>,
    pub data: Value,
}

// ---- per-event payloads ----------------------------------------------------
// `session.created`/`session.updated` carry a `Session`; `message.started` a
// `Message`; `ask.question` an `AskQuestion`; `job.spawned`/`job.exited` a
// `BackgroundJob`; `workflow.updated` a `WorkflowRun`; `workflow.agent` a
// `WorkflowAgent`. The rest are below.

/// `message.delta` — incremental model text for a streaming message.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessageDeltaData {
    pub message_id: String,
    pub delta: String,
}

/// `message.part` — one finalized Part appended to a message.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessagePartData {
    pub message_id: String,
    pub part: Part,
}

/// `message.finished` — the message is complete; `pending` is now false.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessageFinishedData {
    pub message_id: String,
}

/// `message.retry` — the round is being re-attempted. The message re-streams
/// from the top, so a client drops its streaming buffer for it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessageRetryData {
    pub message_id: String,
    /// 1-based; retries are bounded and an exhausted one surfaces as a turn error.
    pub attempt: u32,
    pub reason: String,
}

/// `tool.log` — one `console.*` line from a running program, keyed to the
/// `tool_call` that produced it. Display-only.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolLogData {
    pub message_id: String,
    pub call_id: String,
    pub line: String,
}

/// `session.activity` — what the session is doing right now, in two
/// independent slots: the cheap-tier `activity` blurb ("running the test
/// suite") and the `command` a shell verb is blocked on right now
/// (`cargo test`). Both render on the busy line.
///
/// TWO PUBLISHERS, ONE EVENT, so each field is `Option<Option<_>>`: **absent
/// leaves the other slot alone, `null` clears it.** The cheap tier and
/// `hostfn::shell` publish concurrently and neither knows the other's state
/// — a plain `Option` would make every blurb erase the running command and
/// every command erase the blurb. Absent-is-untouched is what keeps them
/// independent without a shared owner.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionActivityData {
    pub session_id: String,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub activity: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub command: Option<Option<String>>,
}

/// Absent → `None`, `null` → `Some(None)`, value → `Some(Some(v))`.
///
/// REQUIRED, not decoration: serde's derive collapses an explicit `null` to
/// `None` for a plain `Option<Option<T>>`, which is exactly the case that
/// carries the CLEAR. Without this the busy line named a finished command
/// forever — the fields would have been write-only.
pub fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// `turn.finished` — emitted after `message.finished`, once per turn.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnFinishedData {
    pub turn_id: String,
    pub session_id: String,
    pub status: TurnStatus,
    /// Present when status is `error` — names the limit or the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `workflow.log` — one narrator `log()` line from a running script.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLogData {
    pub run_id: String,
    pub line: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_list_has_exactly_16_names_and_round_trips() {
        assert_eq!(EVENT_TYPES.len(), 16);
        for name in EVENT_TYPES {
            let t: EventType = serde_json::from_value(Value::String(name.into())).unwrap();
            assert_eq!(t.as_str(), name);
            assert_eq!(serde_json::to_value(t).unwrap(), Value::String(name.into()));
        }
    }

    #[test]
    fn unknown_event_type_rejected_on_the_envelope() {
        let json = r#"{"type":"message.exploded","seq":1,"ts":2,"data":{}}"#;
        assert!(serde_json::from_str::<BoughEvent>(json).is_err());
    }

    #[test]
    fn envelope_round_trips() {
        let json = r#"{"type":"message.delta","sessionId":"s1","seq":7,"ts":123,"data":{"messageId":"m1","delta":"hi"}}"#;
        let e: BoughEvent = serde_json::from_str(json).unwrap();
        assert_eq!(e.r#type, EventType::MessageDelta);
        assert_eq!(e.session_id.as_deref(), Some("s1"));
        let d: MessageDeltaData = serde_json::from_value(e.data.clone()).unwrap();
        assert_eq!(d.delta, "hi");
        let out = serde_json::to_string(&e).unwrap();
        assert!(out.contains(r#""type":"message.delta""#));
        assert!(out.contains(r#""sessionId":"s1""#));
    }
}
