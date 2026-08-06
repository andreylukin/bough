//! The pure reducer (port of `src/tui/store.ts` — the transition half).
//!
//! FIRST INVARIANT: `reduce(state, action)` is a pure function of data — no I/O,
//! no clock, no terminal — so every rule is testable by replaying a recorded
//! event sequence with nothing mounted.
//!
//! SECOND — **the reducer is idempotent under re-delivery.** `seq` is a dedupe
//! key, not a resume cursor: it resets on restart, so a reconnecting client
//! re-fetches and reconciles by message id. Three defence layers:
//!   1. the `seq:ts` dedupe window (bounded at [`DEDUPE_WINDOW`]);
//!   2. the snapshot watermark (server persists THEN publishes, so events
//!      stamped before the snapshot request are already in it);
//!   3. identity-keyed part appends (text/reasoning have no identity and are
//!      legal to repeat — never content-deduped).
//!
//! THIRD — **a snapshot merges, it does not clobber**: union by id, the longer
//! part list wins, `pending` only ever goes true→false, stream-only local
//! messages survive.
//!
//! The event match is EXHAUSTIVE over the closed [`EventType`] enum with no
//! default arm — a new event type must be a compile error.

use bough_core::schema::events::{BoughEvent, EventType};
use bough_core::schema::parts::{
    AskQuestion, AskQuestionStatus, BackgroundJob, Message, Part, Session, TurnStatus,
};
use serde::Deserialize;
use serde_json::Value;

use super::selectors::{humanize_retry_reason, settled_line, total_tokens};
use super::state::{
    initial_state, BackgroundToast, MarkKind, SessionRow, SnapshotUsage, StoreAction,
    TranscriptMark, TuiSessionRow, TuiState, TurnMeter, DEDUPE_WINDOW, MARK_LIMIT,
    RECONCILED_LIMIT,
};

// ---------------------------------------------------------------------------
// Dedupe and reconciliation primitives (pure)
// ---------------------------------------------------------------------------

/// Layer 1's key. The pair: `seq` alone resets on restart, `ts` alone collides
/// at ms resolution.
pub fn event_key(seq: u64, ts: i64) -> String {
    format!("{seq}:{ts}")
}

/// Has this exact event already been applied, or does the snapshot already
/// contain it? The rule the whole reconnect story rests on.
pub fn is_duplicate(state: &TuiState, event: &BoughEvent) -> bool {
    if state
        .seen
        .iter()
        .any(|k| *k == event_key(event.seq, event.ts))
    {
        return true;
    }
    let Some(session_id) = &event.session_id else {
        // An un-scoped event is never watermark-dropped.
        return false;
    };
    match state.reconciled_at.get(session_id) {
        // The boundary is exclusive: `at` itself is live.
        Some(watermark) => event.ts < *watermark,
        None => false,
    }
}

fn remember(seen: &[String], key: String) -> Vec<String> {
    let mut next: Vec<String> = if seen.len() >= DEDUPE_WINDOW {
        seen[seen.len() - DEDUPE_WINDOW + 1..].to_vec()
    } else {
        seen.to_vec()
    };
    next.push(key);
    next
}

/// Write a session's snapshot watermark, capped at [`RECONCILED_LIMIT`]. The
/// one just written is never the one evicted, and neither is the OPEN session's.
fn remember_watermark(
    reconciled_at: &std::collections::HashMap<String, i64>,
    session_id: &str,
    at: i64,
    current_id: Option<&str>,
) -> std::collections::HashMap<String, i64> {
    let mut next = reconciled_at.clone();
    next.insert(session_id.to_string(), at);
    if next.len() <= RECONCILED_LIMIT {
        return next;
    }
    let mut stale: Vec<(String, i64)> = next
        .iter()
        .filter(|(id, _)| id.as_str() != session_id && Some(id.as_str()) != current_id)
        .map(|(id, ts)| (id.clone(), *ts))
        .collect();
    stale.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let excess = next.len() - RECONCILED_LIMIT;
    for (id, _) in stale.into_iter().take(excess) {
        next.remove(&id);
    }
    next
}

/// A part's identity, or None when it has none (text, reasoning — legal to
/// repeat, so never deduped by content).
pub fn part_key(part: &Part) -> Option<String> {
    match part {
        Part::ToolCall { id, .. } => Some(format!("tool_call:{id}")),
        Part::ToolResult { call_id, .. } => Some(format!("tool_result:{call_id}")),
        Part::Ask { id, .. } => Some(format!("ask:{id}")),
        Part::Image { path, .. } => Some(format!("image:{path}")),
        Part::Workflow { id, .. } => Some(format!("workflow:{id}")),
        Part::Text { .. } | Part::Reasoning { .. } => None,
    }
}

/// Append `part` unless an identity-carrying twin is already there (layer 3).
/// Returns None when nothing changed.
fn append_part(parts: &[Part], part: &Part) -> Option<Vec<Part>> {
    if let Some(key) = part_key(part) {
        if parts
            .iter()
            .any(|p| part_key(p).as_deref() == Some(key.as_str()))
        {
            return None;
        }
    }
    let mut next = parts.to_vec();
    next.push(part.clone());
    Some(next)
}

/// Merge one message from a snapshot with the one the events built: take the
/// longer part list, take finished over pending.
fn merge_message(from_db: &Message, local: &Message) -> Message {
    let mut merged = from_db.clone();
    if local.parts.len() > from_db.parts.len() {
        merged.parts = local.parts.clone();
    }
    merged.pending = from_db.pending && local.pending;
    merged
}

/// The snapshot thread, plus anything the stream delivered that the read
/// predates. A straight replace would drop a `message.started` that landed
/// while the request was in flight.
pub fn merge_thread(from_db: &[Message], local: &[Message]) -> Vec<Message> {
    let mut merged: Vec<Message> = from_db
        .iter()
        .map(|m| match local.iter().find(|l| l.id == m.id) {
            Some(mine) => merge_message(m, mine),
            None => m.clone(),
        })
        .collect();
    for m in local {
        if !from_db.iter().any(|d| d.id == m.id) {
            merged.push(m.clone());
        }
    }
    merged
}

// ---------------------------------------------------------------------------
// The reducer
// ---------------------------------------------------------------------------

/// Rows carry client-side memory (`unseen`) that a server refetch must not erase.
fn merge_session_rows(previous: &[TuiSessionRow], next: Vec<SessionRow>) -> Vec<TuiSessionRow> {
    next.into_iter()
        .map(|s| {
            let old = previous.iter().find(|p| p.row.session.id == s.session.id);
            let unseen = match old {
                Some(o) if o.unseen == Some(true) => Some(true),
                _ => None,
            };
            TuiSessionRow { row: s, unseen }
        })
        .collect()
}

fn patch_session<F: Fn(&mut TuiSessionRow)>(
    sessions: &[TuiSessionRow],
    id: &str,
    patch: F,
) -> Vec<TuiSessionRow> {
    sessions
        .iter()
        .map(|s| {
            if s.row.session.id != id {
                return s.clone();
            }
            let mut updated = s.clone();
            patch(&mut updated);
            updated
        })
        .collect()
}

fn patch_message<F: Fn(&Message) -> Message>(
    thread: &[Message],
    id: &str,
    patch: F,
) -> Vec<Message> {
    thread
        .iter()
        .map(|m| if m.id == id { patch(m) } else { m.clone() })
        .collect()
}

/// Fold fresh usage totals in, re-deriving the running turn's own delta.
/// Every path that learns a new total goes through here — poll AND snapshot.
fn with_usage(mut state: TuiState, usage: SnapshotUsage) -> TuiState {
    if let Some(turn) = &state.turn {
        if Some(turn.session_id.as_str()) == state.current_id.as_deref() {
            let mut t = turn.clone();
            t.tokens = (total_tokens(&usage.totals) - t.base_tokens).max(0);
            t.cost_usd = (usage.totals.cost_usd - t.base_cost_usd).max(0.0);
            state.turn = Some(t);
        }
    }
    state.usage = Some(usage);
    state
}

/// Append a mark, oldest first, capped. Marks are a ledger, not a log.
fn append_mark(marks: &[TranscriptMark], mark: TranscriptMark) -> Vec<TranscriptMark> {
    let mut next = marks.to_vec();
    next.push(mark);
    if next.len() > MARK_LIMIT {
        next.drain(0..next.len() - MARK_LIMIT);
    }
    next
}

// Per-type payloads, parsed LENIENTLY: the envelope was schema-validated at the
// socket; a payload the reducer cannot read makes the event a no-op rather than
// a crash (the TS reducer never validates payloads at all).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeltaData {
    message_id: String,
    delta: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryData {
    message_id: String,
    attempt: u32,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartData {
    message_id: String,
    part: Part,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinishedData {
    message_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogData {
    call_id: String,
    line: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityData {
    activity: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnDoneData {
    session_id: String,
    status: TurnStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WfUpdatedData {
    id: String,
    status: bough_core::schema::parts::WorkflowStatus,
    #[serde(default)]
    current_phase: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    finished_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WfLogData {
    run_id: String,
    line: String,
}

fn parse<T: for<'de> Deserialize<'de>>(data: &Value) -> Option<T> {
    serde_json::from_value(data.clone()).ok()
}

/// Apply one event. Assumes dedupe already passed — `reduce` owns that.
fn apply_event(state: TuiState, event: &BoughEvent) -> TuiState {
    let current = state.current_id.clone();
    let mine = event.session_id.is_some() && event.session_id == current;

    // EXHAUSTIVE over the frozen event enum; no default arm — a new event name
    // must be a compile error here, mirroring the TS comment.
    match event.r#type {
        EventType::SessionCreated => {
            let Some(s) = parse::<Session>(&event.data) else {
                return state;
            };
            if state.sessions.iter().any(|p| p.row.session.id == s.id) {
                return state;
            }
            // Machine-spawned work collapses under its origin and is reached by
            // drill-in, never by the top-level list.
            if bough_core::schema::parts::is_collapsed_kind(s.kind) {
                return state;
            }
            let mut next = state;
            next.sessions.insert(
                0,
                TuiSessionRow {
                    row: SessionRow {
                        session: s,
                        busy: false,
                        last_turn_status: None,
                        cost_usd: None,
                        tokens: None,
                    },
                    unseen: None,
                },
            );
            next
        }

        EventType::SessionUpdated => {
            let Some(s) = parse::<Session>(&event.data) else {
                return state;
            };
            let mut next = state;
            next.sessions = patch_session(&next.sessions, &s.id, |p| p.row.session = s.clone());
            if next.session.as_ref().map(|x| x.id.as_str()) == Some(s.id.as_str()) {
                next.session = Some(s.clone());
            }
            next
        }

        EventType::SessionActivity => {
            if !mine {
                return state;
            }
            let Some(d) = parse::<ActivityData>(&event.data) else {
                return state;
            };
            let mut next = state;
            next.activity = d.activity;
            next
        }

        EventType::MessageStarted => {
            let Some(m) = parse::<Message>(&event.data) else {
                return state;
            };
            let mut next = state;
            if m.pending {
                next.sessions = patch_session(&next.sessions, &m.session_id, |s| s.row.busy = true);
            }
            if Some(m.session_id.as_str()) != current.as_deref() {
                return next;
            }
            let existing = next.thread.iter().any(|x| x.id == m.id);
            if existing {
                // Already here: keep what the stream has accumulated.
                next.thread = patch_message(&next.thread, &m.id, |x| merge_message(&m, x));
            } else {
                next.thread.push(m.clone());
            }
            // A pending message in the open session IS the turn starting. The
            // clock the meter needs is the event's own `ts`.
            let start_turn = m.pending
                && (next.turn.is_none()
                    || next.turn.as_ref().is_some_and(|t| t.ended_at.is_some()));
            if start_turn {
                next.turn = Some(TurnMeter {
                    session_id: m.session_id.clone(),
                    started_at: event.ts,
                    base_tokens: next.usage.as_ref().map_or(0, |u| total_tokens(&u.totals)),
                    base_cost_usd: next.usage.as_ref().map_or(0.0, |u| u.totals.cost_usd),
                    tokens: 0,
                    cost_usd: 0.0,
                    ended_at: None,
                    status: None,
                });
            }
            next
        }

        EventType::MessageDelta => {
            if !mine && current.is_some() {
                return state;
            }
            let Some(d) = parse::<DeltaData>(&event.data) else {
                return state;
            };
            let mut next = state;
            next.streaming
                .entry(d.message_id)
                .or_default()
                .push_str(&d.delta);
            next
        }

        EventType::MessageRetry => {
            // The round re-streams from the top: the partial text is a
            // competing copy, not a prefix of what is coming.
            let Some(d) = parse::<RetryData>(&event.data) else {
                return state;
            };
            let mut next = state;
            next.streaming.remove(&d.message_id);
            if mine {
                next.notice = Some(format!(
                    "retrying (attempt {}) — {}",
                    d.attempt,
                    humanize_retry_reason(&d.reason, 60)
                ));
            }
            next
        }

        EventType::MessagePart => {
            let Some(d) = parse::<PartData>(&event.data) else {
                return state;
            };
            let mut next = state;
            next.thread = patch_message(&next.thread, &d.message_id, |m| {
                match append_part(&m.parts, &d.part) {
                    Some(parts) => {
                        let mut m2 = m.clone();
                        m2.parts = parts;
                        m2
                    }
                    None => m.clone(),
                }
            });
            // The finalized text part supersedes the live buffer.
            if matches!(d.part, Part::Text { .. }) {
                next.streaming.remove(&d.message_id);
            }
            // The arriving tool_result carries the live lines joined; freeing
            // the buffer here is what keeps a chatty program from being
            // retained, unread, for the whole session.
            if let Part::ToolResult { call_id, .. } = &d.part {
                next.tool_logs.remove(call_id);
            }
            next
        }

        EventType::ToolLog => {
            let Some(d) = parse::<LogData>(&event.data) else {
                return state;
            };
            let mut next = state;
            next.tool_logs.entry(d.call_id).or_default().push(d.line);
            next
        }

        EventType::MessageFinished => {
            let Some(d) = parse::<FinishedData>(&event.data) else {
                return state;
            };
            let mut next = state;
            if let Some(session_id) = event.session_id.clone() {
                let row = next
                    .sessions
                    .iter()
                    .find(|s| s.row.session.id == session_id)
                    .cloned();
                next.sessions = patch_session(&next.sessions, &session_id, |s| {
                    s.row.busy = false;
                    s.unseen = Some(s.unseen == Some(true) || !mine);
                });
                // A background session finishing while you watch another is
                // news; work that collapses under an origin is not.
                if let Some(row) = row {
                    if row.row.busy
                        && !mine
                        && !bough_core::schema::parts::is_collapsed_kind(row.row.session.kind)
                    {
                        next.background = Some(BackgroundToast {
                            session_id,
                            title: if row.row.session.title.is_empty() {
                                "session".to_string()
                            } else {
                                row.row.session.title.clone()
                            },
                            seq: next.background.as_ref().map_or(0, |b| b.seq) + 1,
                        });
                    }
                }
            }
            if mine {
                next.activity = None;
            }
            next.thread = patch_message(&next.thread, &d.message_id, |m| {
                let mut m2 = m.clone();
                m2.pending = false;
                m2
            });
            next.streaming.remove(&d.message_id);
            next
        }

        EventType::TurnFinished => {
            let Some(d) = parse::<TurnDoneData>(&event.data) else {
                return state;
            };
            let mut next = state;
            // Stamped, not settled: the tokens are only final after the usage
            // refetch this event triggers.
            if let Some(turn) = &next.turn {
                if turn.session_id == d.session_id && turn.ended_at.is_none() {
                    let mut t = turn.clone();
                    t.ended_at = Some(event.ts);
                    t.status = Some(d.status);
                    next.turn = Some(t);
                }
            }
            next.sessions = patch_session(&next.sessions, &d.session_id, |s| {
                s.row.busy = false;
                s.row.last_turn_status = Some(d.status);
            });
            next
        }

        EventType::AskQuestion => {
            let Some(q) = parse::<AskQuestion>(&event.data) else {
                return state;
            };
            let mut next = state;
            if q.status == AskQuestionStatus::Pending {
                if let Some(existing) = next.asks.iter_mut().find(|p| p.id == q.id) {
                    *existing = q;
                } else {
                    next.asks.push(q);
                }
            } else {
                // Settled: drop it so the next hold surfaces.
                next.asks.retain(|p| p.id != q.id);
            }
            next
        }

        EventType::JobSpawned | EventType::JobExited => {
            let Some(job) = parse::<BackgroundJob>(&event.data) else {
                return state;
            };
            let known = state.jobs.iter().any(|j| j.job.id == job.id);
            // Only the open session's own rows are inserted; the server owns
            // the lineage rules.
            if !known && Some(job.session_id.as_str()) != current.as_deref() {
                return state;
            }
            let mut next = state;
            if known {
                for j in next.jobs.iter_mut() {
                    if j.job.id == job.id {
                        j.job = job.clone();
                        j.tail = None;
                        j.output_lines = None;
                    }
                }
            } else {
                next.jobs.insert(
                    0,
                    super::state::JobListRow {
                        job,
                        tail: None,
                        output_lines: None,
                    },
                );
            }
            next
        }

        EventType::WorkflowUpdated => {
            let mut next = state;
            if let Some(run) = parse::<WfUpdatedData>(&event.data) {
                for w in next.workflows.iter_mut() {
                    if w.id == run.id {
                        w.status = run.status;
                        w.current_phase = run.current_phase.clone();
                        w.error = run.error.clone();
                        w.finished_at = run.finished_at;
                    }
                }
            }
            next.workflow_seq += 1;
            next
        }

        EventType::WorkflowAgent => {
            let mut next = state;
            next.workflow_seq += 1;
            next
        }

        EventType::WorkflowLog => {
            let Some(d) = parse::<WfLogData>(&event.data) else {
                return state;
            };
            let mut next = state;
            next.workflow_logs.insert(d.run_id, d.line);
            next
        }
    }
}

/// The whole state transition. Pure: same inputs, same output, no I/O anywhere.
pub fn reduce(state: TuiState, action: StoreAction) -> TuiState {
    match action {
        StoreAction::Event { event } => {
            if is_duplicate(&state, &event) {
                return state;
            }
            let seen = remember(&state.seen, event_key(event.seq, event.ts));
            // Remembered even when the event changed nothing: "seen" is about
            // delivery, not about effect.
            let mut next = apply_event(state, &event);
            next.seen = seen;
            next
        }

        StoreAction::Connection { connected } => {
            let mut next = state;
            next.connected = connected;
            next
        }

        StoreAction::Sessions { sessions } => {
            let mut next = state;
            next.sessions = merge_session_rows(&next.sessions, sessions);
            next
        }

        StoreAction::Open { session_id } => {
            if session_id == state.current_id {
                return state;
            }
            // Everything below belonged to the session being left.
            let mut next = state;
            if let Some(id) = &session_id {
                next.sessions = patch_session(&next.sessions, id, |s| {
                    if s.unseen == Some(true) {
                        s.unseen = Some(false);
                    }
                });
            }
            next.current_id = session_id;
            next.session = None;
            next.thread = Vec::new();
            next.streaming.clear();
            next.tool_logs.clear();
            next.queued = Vec::new();
            // The take-back window is about the conversation you sent INTO.
            next.last_send_at = None;
            next.activity = None;
            next.usage = None;
            next.effective_model = None;
            next.context_limit = None;
            next.primed_tags = Vec::new();
            next.project_rules = Vec::new();
            next.dropped_ids = Vec::new();
            next.changes = None;
            next.jobs = Vec::new();
            next.job_view = None;
            next.workflows = Vec::new();
            // A narrator line kept past its chip is unreachable state that only
            // ever grows, since a runId never recurs.
            next.workflow_logs.clear();
            next.replay = None;
            // `marks` deliberately do NOT reset.
            next.turn = None;
            next
        }

        StoreAction::Snapshot { at, snapshot } => {
            if Some(snapshot.session.id.as_str()) != state.current_id.as_deref() {
                // Lost the race with a session switch. Record the watermark
                // anyway — it is a fact about that session, not about the view.
                let mut next = state;
                next.reconciled_at = remember_watermark(
                    &next.reconciled_at,
                    &snapshot.session.id,
                    at,
                    next.current_id.as_deref(),
                );
                return next;
            }
            // Tombstones win over the read.
            let incoming: Vec<Message> = if state.dropped_ids.is_empty() {
                snapshot.thread.clone()
            } else {
                snapshot
                    .thread
                    .iter()
                    .filter(|m| !state.dropped_ids.contains(&m.id))
                    .cloned()
                    .collect()
            };
            let merged = merge_thread(&incoming, &state.thread);
            // Drop the live buffer of any message the database now shows as
            // finished; a still-pending message keeps its buffer.
            let streaming: std::collections::HashMap<String, String> = state
                .streaming
                .iter()
                .filter(|(id, _)| merged.iter().any(|m| m.id == **id && m.pending))
                .map(|(id, text)| (id.clone(), text.clone()))
                .collect();
            let mut next = state;
            next.session = Some(snapshot.session.clone());
            next.thread = merged;
            next.streaming = streaming;
            if let Some(m) = snapshot.effective_model.clone() {
                next.effective_model = Some(m);
            }
            if let Some(l) = snapshot.context_limit {
                next.context_limit = Some(l);
            }
            if let Some(t) = snapshot.primed_tags.clone() {
                next.primed_tags = t;
            }
            if let Some(r) = snapshot.project_rules.clone() {
                next.project_rules = r;
            }
            next.reconciled_at = remember_watermark(
                &next.reconciled_at,
                &snapshot.session.id,
                at,
                next.current_id.as_deref(),
            );
            with_usage(next, snapshot.usage)
        }

        StoreAction::Questions { questions } => {
            let mut next = state;
            next.asks = questions;
            next
        }

        StoreAction::AskSettled { id } => {
            let mut next = state;
            next.asks.retain(|q| q.id != id);
            next
        }

        StoreAction::Changes {
            session_id,
            changes,
        } => {
            if Some(session_id.as_str()) != state.current_id.as_deref() {
                return state;
            }
            let mut next = state;
            next.changes = Some(changes);
            next
        }

        StoreAction::Jobs { session_id, jobs } => {
            if Some(session_id.as_str()) != state.current_id.as_deref() {
                return state;
            }
            let mut next = state;
            next.jobs = jobs;
            next
        }

        StoreAction::JobView { view } => {
            let mut next = state;
            next.job_view = view;
            next
        }

        StoreAction::Schedules { schedules } => {
            let mut next = state;
            next.schedules = schedules;
            next
        }

        StoreAction::Workflows {
            session_id,
            workflows,
        } => {
            if Some(session_id.as_str()) != state.current_id.as_deref() {
                return state;
            }
            let mut next = state;
            next.workflows = workflows;
            next
        }

        StoreAction::Replay { replay } => {
            let mut next = state;
            next.replay = replay;
            next
        }

        StoreAction::Notice { notice } => {
            let mut next = state;
            next.notice = notice;
            next
        }

        StoreAction::Mark {
            session_id,
            at,
            text,
        } => {
            let id = format!("mark:{at}:{}", state.marks.len());
            let mut next = state;
            next.marks = append_mark(
                &next.marks,
                TranscriptMark {
                    id,
                    session_id,
                    at,
                    kind: MarkKind::Destructive,
                    text,
                },
            );
            next
        }

        StoreAction::EffectiveModel { model } => {
            let mut next = state;
            next.effective_model = model;
            next
        }

        StoreAction::Usage { session_id, usage } => {
            if Some(session_id.as_str()) != state.current_id.as_deref() {
                return state;
            }
            with_usage(state, usage)
        }

        StoreAction::TurnSettle { at: _ } => {
            // Only a turn that has ENDED settles: a stray settle mid-turn would
            // print a "✓" under a spinner that is still going.
            let Some(turn) = state.turn.clone() else {
                return state;
            };
            let Some(ended_at) = turn.ended_at else {
                return state;
            };
            let mut next = state;
            next.turn = None;
            next.marks = append_mark(
                &next.marks,
                TranscriptMark {
                    id: format!("mark:{}:{}", turn.session_id, turn.started_at),
                    session_id: turn.session_id.clone(),
                    at: ended_at,
                    kind: MarkKind::Turn,
                    text: settled_line(&turn, ended_at),
                },
            );
            next
        }

        StoreAction::Queue { text } => {
            let mut next = state;
            next.queued.push(text);
            next
        }

        StoreAction::QueueDrained => {
            if state.queued.is_empty() {
                return state;
            }
            let mut next = state;
            next.queued = Vec::new();
            next
        }

        StoreAction::QueuePop => {
            if state.queued.is_empty() {
                return state;
            }
            let mut next = state;
            next.queued.pop();
            next
        }

        StoreAction::Sent { at } => {
            let mut next = state;
            next.last_send_at = Some(at);
            next
        }

        StoreAction::ThreadDropped { session_id, ids } => {
            if Some(session_id.as_str()) != state.current_id.as_deref() || ids.is_empty() {
                return state;
            }
            let mut next = state;
            next.thread.retain(|m| !ids.contains(&m.id));
            // The live buffers go WITH them.
            next.streaming.retain(|id, _| !ids.contains(id));
            next.dropped_ids.extend(ids);
            // The window is spent — it was armed by the message that just went away.
            next.last_send_at = None;
            next
        }
    }
}

/// Convenience for tests and the shell: reduce a fresh initial state.
pub fn fresh() -> TuiState {
    initial_state()
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/store.test.ts + retention.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::selectors::is_busy;
    use super::super::state::*;
    use super::*;
    use bough_core::schema::parts::{Role, SessionKind};
    use serde_json::json;

    const SESSION: &str = "sess-1";
    const OTHER: &str = "sess-2";

    /// A stamped event, exactly as the bus would have produced it.
    struct Recorder {
        seq: u64,
        ts: i64,
        log: Vec<BoughEvent>,
    }

    impl Recorder {
        fn new() -> Self {
            Recorder {
                seq: 0,
                ts: 1_000,
                log: Vec::new(),
            }
        }
        fn now(&self) -> i64 {
            self.ts
        }
        /// Advance the clock without publishing — the outage, the round trip.
        fn tick(&mut self, ms: i64) {
            self.ts += ms;
        }
        fn emit(&mut self, t: EventType, data: Value, session_id: Option<&str>) -> BoughEvent {
            self.ts += 1;
            self.seq += 1;
            let event = BoughEvent {
                r#type: t,
                session_id: session_id.map(str::to_string),
                seq: self.seq,
                ts: self.ts,
                data,
            };
            self.log.push(event.clone());
            event
        }
        /// Restart the server under the client: `seq` resets, the clock does not.
        fn restart(&mut self) {
            self.seq = 0;
        }
    }

    fn session(id: &str) -> Value {
        json!({ "id": id, "title": format!("session {id}"), "kind": "root", "createdAt": 1, "parentId": null })
    }

    fn row(id: &str) -> SessionRow {
        serde_json::from_value(json!({
            "id": id, "title": format!("session {id}"), "kind": "root", "createdAt": 1,
            "parentId": null, "busy": false,
        }))
        .unwrap()
    }

    fn row_over(id: &str, over: Value) -> SessionRow {
        let mut base = serde_json::to_value(row(id)).unwrap();
        for (k, v) in over.as_object().unwrap() {
            base[k] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn message(id: &str, over: Value) -> Value {
        let mut base = json!({
            "id": id, "sessionId": SESSION, "role": "supervisor", "parts": [],
            "pending": false, "createdAt": 1,
        });
        for (k, v) in over.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    fn tool_call() -> Part {
        serde_json::from_value(
            json!({"type": "tool_call", "id": "call-1", "name": "run_steps", "input": {"code": "1"}}),
        )
        .unwrap()
    }
    fn tool_result() -> Part {
        serde_json::from_value(
            json!({"type": "tool_result", "callId": "call-1", "output": "ok", "isError": false}),
        )
        .unwrap()
    }
    fn text_part() -> Part {
        serde_json::from_value(json!({"type": "text", "text": "Looking…!"})).unwrap()
    }

    fn replay_events(state: TuiState, events: &[BoughEvent]) -> TuiState {
        events.iter().fold(state, |s, event| {
            reduce(
                s,
                StoreAction::Event {
                    event: event.clone(),
                },
            )
        })
    }

    fn apply(state: TuiState, actions: Vec<StoreAction>) -> TuiState {
        actions.into_iter().fold(state, reduce)
    }

    fn usage(input: i64, output: i64, cost: f64) -> SnapshotUsage {
        serde_json::from_value(json!({
            "inputTokens": input, "outputTokens": output, "reasoningTokens": 0,
            "cacheReadTokens": 0, "cacheWriteTokens": 0, "costUsd": cost,
            "tree": { "inputTokens": input, "outputTokens": output, "reasoningTokens": 0,
                      "cacheReadTokens": 0, "cacheWriteTokens": 0, "costUsd": cost },
        }))
        .unwrap()
    }

    /// What `GET /sessions/:id` returns at the moment of the reconnect.
    fn snapshot_after_outage() -> SessionSnapshot {
        SessionSnapshot {
            session: serde_json::from_value(session(SESSION)).unwrap(),
            thread: vec![
                serde_json::from_value(message(
                    "m-user",
                    json!({"role": "user", "parts": [{"type": "text", "text": "go"}]}),
                ))
                .unwrap(),
                serde_json::from_value(message(
                    "m-1",
                    json!({"parts": [
                        {"type": "tool_call", "id": "call-1", "name": "run_steps", "input": {"code": "1"}},
                        {"type": "tool_result", "callId": "call-1", "output": "ok", "isError": false},
                    ], "pending": true}),
                ))
                .unwrap(),
            ],
            usage: usage(10, 5, 0.01),
            effective_model: None,
            context_limit: None,
            primed_tags: None,
            project_rules: None,
        }
    }

    fn open(id: &str) -> StoreAction {
        StoreAction::Open {
            session_id: Some(id.to_string()),
        }
    }

    // ---- the acceptance test ------------------------------------------------

    #[test]
    fn reconnect_redelivers_applied_events_no_duplicates_no_lost_deltas() {
        let mut rec = Recorder::new();
        let user = message(
            "m-user",
            json!({"role": "user", "parts": [{"type": "text", "text": "go"}]}),
        );
        let supervisor = message("m-1", json!({"pending": true}));

        let before: Vec<BoughEvent> = vec![
            rec.emit(EventType::SessionCreated, session(SESSION), Some(SESSION)),
            rec.emit(EventType::MessageStarted, user, Some(SESSION)),
            rec.emit(EventType::MessageStarted, supervisor, Some(SESSION)),
            rec.emit(EventType::MessageDelta, json!({"messageId": "m-1", "delta": "Look"}), Some(SESSION)),
            rec.emit(EventType::MessageDelta, json!({"messageId": "m-1", "delta": "ing…"}), Some(SESSION)),
            rec.emit(EventType::ToolLog, json!({"messageId": "m-1", "callId": "call-1", "line": "compiling"}), Some(SESSION)),
            rec.emit(
                EventType::MessagePart,
                json!({"messageId": "m-1", "part": {"type": "tool_call", "id": "call-1", "name": "run_steps", "input": {"code": "1"}}}),
                Some(SESSION),
            ),
        ];
        // The outage. Both are lost to the client and restored by the fetch.
        let _missed = [rec.emit(
                EventType::MessagePart,
                json!({"messageId": "m-1", "part": {"type": "tool_result", "callId": "call-1", "output": "ok", "isError": false}}),
                Some(SESSION),
            ),
            rec.emit(EventType::MessageDelta, json!({"messageId": "m-1", "delta": "!"}), Some(SESSION))];

        // 1. Live, through the outage point.
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        state = replay_events(state, &before);
        assert_eq!(state.thread.len(), 2);
        assert_eq!(state.streaming["m-1"], "Looking…");
        assert_eq!(state.tool_logs["call-1"], vec!["compiling"]);

        // 2. The stream drops.
        state = reduce(state, StoreAction::Connection { connected: false });

        // 3. Reconnect: re-fetch and reconcile by message id. Watermark taken
        // when the request was ISSUED.
        rec.tick(500);
        let at = rec.now();
        rec.tick(20); // the round trip
        state = reduce(
            state,
            StoreAction::Snapshot {
                at,
                snapshot: snapshot_after_outage(),
            },
        );
        state = reduce(state, StoreAction::Connection { connected: true });

        let supervisor_parts = |s: &TuiState| {
            s.thread
                .iter()
                .find(|m| m.id == "m-1")
                .unwrap()
                .parts
                .clone()
        };
        assert_eq!(supervisor_parts(&state), vec![tool_call(), tool_result()]);
        // The delta streamed before the outage survives — "no lost deltas".
        assert_eq!(state.streaming["m-1"], "Looking…");

        // 4. THE CASE THIS TEST EXISTS FOR: the whole log delivered again.
        let before4 = state.clone();
        state = replay_events(state, &rec.log.clone());
        assert_eq!(state, before4, "a fully re-delivered log must be a no-op");
        assert_eq!(state.thread.len(), 2, "no duplicate message");
        assert_eq!(
            supervisor_parts(&state),
            vec![tool_call(), tool_result()],
            "no duplicate part"
        );
        assert_eq!(state.streaming["m-1"], "Looking…", "no doubled delta");
        assert_eq!(
            state.tool_logs["call-1"],
            vec!["compiling"],
            "no doubled tool log"
        );

        // 5. Live again. New events land; a redialed overlap deduped.
        let live = [
            rec.emit(
                EventType::MessageDelta,
                json!({"messageId": "m-1", "delta": "!"}),
                Some(SESSION),
            ),
            rec.emit(
                EventType::MessagePart,
                json!({"messageId": "m-1", "part": {"type": "text", "text": "Looking…!"}}),
                Some(SESSION),
            ),
            rec.emit(
                EventType::MessageFinished,
                json!({"messageId": "m-1"}),
                Some(SESSION),
            ),
            rec.emit(
                EventType::TurnFinished,
                json!({"turnId": "t-1", "sessionId": SESSION, "status": "done"}),
                Some(SESSION),
            ),
        ];
        state = replay_events(
            state,
            &[
                live[0].clone(),
                live[0].clone(),
                live[1].clone(),
                live[2].clone(),
                live[3].clone(),
            ],
        );

        let finished = state.thread.iter().find(|m| m.id == "m-1").unwrap();
        assert_eq!(
            finished.parts,
            vec![tool_call(), tool_result(), text_part()]
        );
        assert!(!finished.pending);
        assert!(
            !state.streaming.contains_key("m-1"),
            "the finalized text supersedes the buffer"
        );
        assert_eq!(state.thread.len(), 2);
        assert!(!is_busy(&state));
        let listed = state
            .sessions
            .iter()
            .find(|s| s.row.session.id == SESSION)
            .unwrap();
        assert!(!listed.row.busy);
        assert_eq!(listed.row.last_turn_status, Some(TurnStatus::Done));
    }

    #[test]
    fn delta_text_reaching_the_finalized_part_is_neither_doubled_nor_short() {
        let mut rec = Recorder::new();
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let first = vec![
            rec.emit(
                EventType::MessageStarted,
                message("m-1", json!({"pending": true})),
                Some(SESSION),
            ),
            rec.emit(
                EventType::MessageDelta,
                json!({"messageId": "m-1", "delta": "one "}),
                Some(SESSION),
            ),
            rec.emit(
                EventType::MessageDelta,
                json!({"messageId": "m-1", "delta": "two "}),
                Some(SESSION),
            ),
        ];
        state = replay_events(state, &first);
        rec.tick(100);
        let at = rec.now();
        state = reduce(
            state,
            StoreAction::Snapshot {
                at,
                snapshot: SessionSnapshot {
                    session: serde_json::from_value(session(SESSION)).unwrap(),
                    thread: vec![
                        serde_json::from_value(message("m-1", json!({"pending": true}))).unwrap(),
                    ],
                    usage: usage(10, 5, 0.01),
                    effective_model: None,
                    context_limit: None,
                    primed_tags: None,
                    project_rules: None,
                },
            },
        );
        let mut whole = rec.log.clone();
        whole.push(rec.emit(
            EventType::MessageDelta,
            json!({"messageId": "m-1", "delta": "three"}),
            Some(SESSION),
        ));
        state = replay_events(state, &whole);
        state = replay_events(state, &rec.log.clone()); // once more, for good measure
        assert_eq!(state.streaming["m-1"], "one two three");
    }

    #[test]
    fn seq_resets_on_restart_so_the_dedupe_key_cannot_be_seq_alone() {
        let mut rec = Recorder::new();
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let first = vec![
            rec.emit(
                EventType::MessageStarted,
                message("m-1", json!({"pending": true})),
                Some(SESSION),
            ),
            rec.emit(
                EventType::MessageDelta,
                json!({"messageId": "m-1", "delta": "before"}),
                Some(SESSION),
            ),
        ];
        state = replay_events(state, &first);

        rec.tick(50);
        let at = rec.now();
        state = reduce(
            state,
            StoreAction::Snapshot {
                at,
                snapshot: SessionSnapshot {
                    session: serde_json::from_value(session(SESSION)).unwrap(),
                    thread: vec![
                        serde_json::from_value(message("m-2", json!({"pending": true}))).unwrap(),
                    ],
                    usage: usage(10, 5, 0.01),
                    effective_model: None,
                    context_limit: None,
                    primed_tags: None,
                    project_rules: None,
                },
            },
        );
        rec.restart();
        rec.tick(10);
        let fresh = rec.emit(
            EventType::MessageDelta,
            json!({"messageId": "m-2", "delta": "after"}),
            Some(SESSION),
        );
        assert_eq!(
            fresh.seq, 1,
            "the recorder must actually reset, or this test proves nothing"
        );

        state = reduce(state, StoreAction::Event { event: fresh });
        assert_eq!(
            state.streaming["m-2"], "after",
            "a restarted server's seq 1 is not the old seq 1"
        );
    }

    #[test]
    fn the_snapshot_watermark_drops_only_events_the_fetch_already_contains() {
        let state = apply(
            initial_state(),
            vec![StoreAction::Snapshot {
                at: 5_000,
                snapshot: SessionSnapshot {
                    session: serde_json::from_value(session(SESSION)).unwrap(),
                    thread: vec![],
                    usage: usage(10, 5, 0.01),
                    effective_model: None,
                    context_limit: None,
                    primed_tags: None,
                    project_rules: None,
                },
            }],
        );
        let ev = |sid: Option<&str>, seq: u64, ts: i64| BoughEvent {
            r#type: EventType::MessageDelta,
            session_id: sid.map(str::to_string),
            seq,
            ts,
            data: json!({}),
        };
        assert!(is_duplicate(&state, &ev(Some(SESSION), 9, 4_999)));
        assert!(
            !is_duplicate(&state, &ev(Some(SESSION), 10, 5_000)),
            "the boundary is exclusive: `at` itself is live"
        );
        assert!(
            !is_duplicate(&state, &ev(Some(OTHER), 11, 1)),
            "another session has its own watermark"
        );
        let global = BoughEvent {
            r#type: EventType::WorkflowLog,
            session_id: None,
            seq: 12,
            ts: 1,
            data: json!({}),
        };
        assert!(
            !is_duplicate(&state, &global),
            "an un-scoped event is never watermarked away"
        );
    }

    #[test]
    fn the_dedupe_window_is_bounded_and_keeps_the_most_recent_identities() {
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let n = DEDUPE_WINDOW as u64 + 10;
        for i in 1..=n {
            state = reduce(
                state,
                StoreAction::Event {
                    event: BoughEvent {
                        r#type: EventType::ToolLog,
                        session_id: Some(SESSION.into()),
                        seq: i,
                        ts: i as i64,
                        data: json!({"messageId": "m", "callId": "c", "line": "x"}),
                    },
                },
            );
        }
        assert_eq!(state.seen.len(), DEDUPE_WINDOW);
        assert!(
            !state.seen.contains(&event_key(1, 1)),
            "the oldest identity aged out"
        );
        assert_eq!(state.seen.last().unwrap(), &format!("{n}:{n}"));
    }

    // ---- reducer rules ------------------------------------------------------

    #[test]
    fn a_subagent_announcement_never_enters_the_top_level_list() {
        let mut state = initial_state();
        state = reduce(
            state,
            StoreAction::Event {
                event: BoughEvent {
                    r#type: EventType::SessionCreated,
                    session_id: Some("sub-1".into()),
                    seq: 1,
                    ts: 1,
                    data: json!({"id": "sub-1", "title": "t", "kind": "subagent", "createdAt": 1, "parentId": null, "originId": SESSION}),
                },
            },
        );
        assert_eq!(
            state.sessions.len(),
            0,
            "delegated work collapses under its origin (spec §4)"
        );

        state = reduce(
            state,
            StoreAction::Event {
                event: BoughEvent {
                    r#type: EventType::SessionCreated,
                    session_id: Some("root-2".into()),
                    seq: 2,
                    ts: 2,
                    data: session("root-2"),
                },
            },
        );
        let ids: Vec<&str> = state
            .sessions
            .iter()
            .map(|s| s.row.session.id.as_str())
            .collect();
        assert_eq!(ids, vec!["root-2"]);
    }

    #[test]
    fn another_sessions_turn_marks_its_row_busy_without_touching_the_open_thread() {
        let mut state = apply(
            initial_state(),
            vec![
                StoreAction::Sessions {
                    sessions: vec![row(SESSION), row(OTHER)],
                },
                open(SESSION),
            ],
        );
        state = reduce(
            state,
            StoreAction::Event {
                event: BoughEvent {
                    r#type: EventType::MessageStarted,
                    session_id: Some(OTHER.into()),
                    seq: 1,
                    ts: 1,
                    data: message("m-other", json!({"sessionId": OTHER, "pending": true})),
                },
            },
        );
        assert_eq!(
            state.thread.len(),
            0,
            "a message of another session is not in this thread"
        );
        assert!(
            state
                .sessions
                .iter()
                .find(|s| s.row.session.id == OTHER)
                .unwrap()
                .row
                .busy
        );
    }

    #[test]
    fn a_background_session_finishing_is_announced_once_with_a_distinct_seq() {
        let mut state = apply(
            initial_state(),
            vec![
                StoreAction::Sessions {
                    sessions: vec![row(SESSION), row_over(OTHER, json!({"busy": true}))],
                },
                open(SESSION),
            ],
        );
        state = reduce(
            state,
            StoreAction::Event {
                event: BoughEvent {
                    r#type: EventType::MessageFinished,
                    session_id: Some(OTHER.into()),
                    seq: 1,
                    ts: 1,
                    data: json!({"messageId": "m-other"}),
                },
            },
        );
        assert_eq!(state.background.as_ref().unwrap().session_id, OTHER);
        assert_eq!(state.background.as_ref().unwrap().seq, 1);
        assert_eq!(
            state
                .sessions
                .iter()
                .find(|s| s.row.session.id == OTHER)
                .unwrap()
                .unseen,
            Some(true)
        );

        // Opening it clears the mark; a server refetch must not bring it back.
        state = reduce(state, open(OTHER));
        assert_eq!(
            state
                .sessions
                .iter()
                .find(|s| s.row.session.id == OTHER)
                .unwrap()
                .unseen,
            Some(false)
        );
        state = reduce(
            state,
            StoreAction::Sessions {
                sessions: vec![row(SESSION), row(OTHER)],
            },
        );
        assert_eq!(
            state
                .sessions
                .iter()
                .find(|s| s.row.session.id == OTHER)
                .unwrap()
                .unseen,
            None
        );
    }

    #[test]
    fn a_subagents_finish_raises_no_background_toast() {
        let mut state = apply(
            initial_state(),
            vec![
                StoreAction::Sessions {
                    sessions: vec![
                        row(SESSION),
                        row_over("sub-1", json!({"kind": "subagent", "busy": true})),
                    ],
                },
                open(SESSION),
            ],
        );
        state = reduce(
            state,
            StoreAction::Event {
                event: BoughEvent {
                    r#type: EventType::MessageFinished,
                    session_id: Some("sub-1".into()),
                    seq: 1,
                    ts: 1,
                    data: json!({"messageId": "m"}),
                },
            },
        );
        assert_eq!(
            state.background, None,
            "a subagent finishes inside its spawner's turn — not news"
        );
    }

    #[test]
    fn message_retry_drops_the_partial_text_rather_than_prefixing_the_restream() {
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let events = vec![
            BoughEvent {
                r#type: EventType::MessageStarted,
                session_id: Some(SESSION.into()),
                seq: 1,
                ts: 1,
                data: message("m-1", json!({"pending": true})),
            },
            BoughEvent {
                r#type: EventType::MessageDelta,
                session_id: Some(SESSION.into()),
                seq: 2,
                ts: 2,
                data: json!({"messageId": "m-1", "delta": "half a too"}),
            },
            BoughEvent {
                r#type: EventType::MessageRetry,
                session_id: Some(SESSION.into()),
                seq: 3,
                ts: 3,
                data: json!({"messageId": "m-1", "attempt": 2, "reason": "truncated tool call"}),
            },
            BoughEvent {
                r#type: EventType::MessageDelta,
                session_id: Some(SESSION.into()),
                seq: 4,
                ts: 4,
                data: json!({"messageId": "m-1", "delta": "all of it"}),
            },
        ];
        state = replay_events(state, &events);
        assert_eq!(state.streaming["m-1"], "all of it");
        assert!(state.notice.as_deref().unwrap().contains("attempt 2"));
    }

    #[test]
    fn ask_holds_surface_oldest_first_and_settle_out_of_the_queue() {
        let hold = |id: &str, status: &str| json!({"id": id, "sessionId": SESSION, "messageId": "m-1", "question": format!("q {id}"), "status": status, "ts": 1});
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        state = replay_events(
            state,
            &[
                BoughEvent {
                    r#type: EventType::AskQuestion,
                    session_id: Some(SESSION.into()),
                    seq: 1,
                    ts: 1,
                    data: hold("q1", "pending"),
                },
                BoughEvent {
                    r#type: EventType::AskQuestion,
                    session_id: Some(SESSION.into()),
                    seq: 2,
                    ts: 2,
                    data: hold("q2", "pending"),
                },
            ],
        );
        assert_eq!(state.asks.len(), 2);
        assert_eq!(state.asks[0].id, "q1");

        // Optimistic settle, then the confirming event. Neither may resurrect it.
        state = reduce(state, StoreAction::AskSettled { id: "q1".into() });
        assert_eq!(state.asks[0].id, "q2");
        state = reduce(
            state,
            StoreAction::Event {
                event: BoughEvent {
                    r#type: EventType::AskQuestion,
                    session_id: Some(SESSION.into()),
                    seq: 3,
                    ts: 3,
                    data: hold("q1", "answered"),
                },
            },
        );
        let ids: Vec<&str> = state.asks.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids, vec!["q2"]);
    }

    #[test]
    fn opening_a_session_drops_everything_that_belonged_to_the_previous_one() {
        let mut state = apply(
            initial_state(),
            vec![
                StoreAction::Sessions {
                    sessions: vec![row(SESSION), row(OTHER)],
                },
                open(SESSION),
                StoreAction::Queue {
                    text: "typed while busy".into(),
                },
            ],
        );
        state = replay_events(
            state,
            &[
                BoughEvent {
                    r#type: EventType::MessageStarted,
                    session_id: Some(SESSION.into()),
                    seq: 1,
                    ts: 1,
                    data: message("m-1", json!({"pending": true})),
                },
                BoughEvent {
                    r#type: EventType::SessionActivity,
                    session_id: Some(SESSION.into()),
                    seq: 2,
                    ts: 2,
                    data: json!({"sessionId": SESSION, "activity": "running tests"}),
                },
            ],
        );
        assert_eq!(state.activity.as_deref(), Some("running tests"));
        assert_eq!(state.queued.len(), 1);

        state = reduce(state, open(OTHER));
        assert!(state.thread.is_empty());
        assert!(
            state.queued.is_empty(),
            "a staged message belongs to the session it was typed in"
        );
        assert_eq!(state.activity, None);
        assert!(!state.streaming.contains_key("m-1"));
    }

    #[test]
    fn merge_thread_keeps_stream_only_messages_and_the_longer_part_list() {
        let m = |id: &str, over: Value| -> Message {
            serde_json::from_value(message(id, over)).unwrap()
        };
        let from_db = vec![
            m(
                "a",
                json!({"parts": [{"type": "tool_call", "id": "call-1", "name": "run_steps", "input": {"code": "1"}}]}),
            ),
            m("b", json!({})),
        ];
        let local = vec![
            m(
                "a",
                json!({"parts": [
                {"type": "tool_call", "id": "call-1", "name": "run_steps", "input": {"code": "1"}},
                {"type": "tool_result", "callId": "call-1", "output": "ok", "isError": false},
            ], "pending": true}),
            ),
            m("c", json!({"pending": true})),
        ];
        let merged = merge_thread(&from_db, &local);
        let ids: Vec<&str> = merged.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert_eq!(merged[0].parts, vec![tool_call(), tool_result()]);
        assert!(
            !merged[0].pending,
            "finished beats pending — `pending` only ever clears"
        );
    }

    #[test]
    fn part_identity_is_what_makes_an_append_idempotent() {
        assert_eq!(part_key(&tool_call()).as_deref(), Some("tool_call:call-1"));
        assert_eq!(
            part_key(&tool_result()).as_deref(),
            Some("tool_result:call-1")
        );
        assert_eq!(part_key(&Part::Text { text: "hi".into() }), None);
        assert_eq!(
            part_key(&Part::Reasoning {
                text: "hm".into(),
                meta: None,
                model: None
            }),
            None
        );
    }

    // ---- attribution, and the audit trail -----------------------------------

    #[test]
    fn a_turns_tokens_are_its_own_measured_from_where_the_session_already_stood() {
        let mut rec = Recorder::new();
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        state = apply(
            state,
            vec![StoreAction::Snapshot {
                at: 0,
                snapshot: SessionSnapshot {
                    session: serde_json::from_value(session(SESSION)).unwrap(),
                    thread: vec![],
                    usage: usage(1_000, 200, 0.02),
                    effective_model: None,
                    context_limit: None,
                    primed_tags: None,
                    project_rules: None,
                },
            }],
        );
        let started = rec.emit(
            EventType::MessageStarted,
            message("m-1", json!({"pending": true})),
            Some(SESSION),
        );
        state = replay_events(state, &[started]);
        assert_eq!(state.turn.as_ref().unwrap().base_tokens, 1_200);
        assert_eq!(state.turn.as_ref().unwrap().tokens, 0);

        state = apply(
            state,
            vec![StoreAction::Usage {
                session_id: SESSION.into(),
                usage: usage(1_500, 700, 0.05),
            }],
        );
        assert_eq!(state.turn.as_ref().unwrap().tokens, 1_000);
        assert_eq!(
            (state.turn.as_ref().unwrap().cost_usd * 1000.0).round() as i64,
            30
        );
        // The session meter still reports the session.
        assert_eq!(state.usage.as_ref().unwrap().totals.input_tokens, 1_500);
    }

    #[test]
    fn a_finished_turn_leaves_a_settled_line_and_the_spinners_numbers_do_not_vanish() {
        let mut rec = Recorder::new();
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let started = rec.emit(
            EventType::MessageStarted,
            message("m-1", json!({"pending": true})),
            Some(SESSION),
        );
        state = replay_events(state, &[started]);
        let started_at = state.turn.as_ref().unwrap().started_at;
        state = apply(
            state,
            vec![StoreAction::Usage {
                session_id: SESSION.into(),
                usage: usage(3_000, 200, 0.021),
            }],
        );
        let done = rec.emit(
            EventType::TurnFinished,
            json!({"sessionId": SESSION, "turnId": "t1", "status": "done"}),
            Some(SESSION),
        );
        state = replay_events(state, &[done]);
        // Ended, but not settled: the numbers are only final after the refetch.
        assert!(state.turn.as_ref().unwrap().ended_at.is_some());
        assert_eq!(state.marks.len(), 0);

        state = apply(
            state,
            vec![StoreAction::TurnSettle {
                at: started_at + 14_000,
            }],
        );
        assert_eq!(state.turn, None);
        let mark = state.marks.last().unwrap();
        assert_eq!(mark.kind, MarkKind::Turn);
        assert!(mark.text.starts_with("✓ "), "{}", mark.text);
        assert!(mark.text.contains("3.2k tok"), "{}", mark.text);
        // NO per-turn cost: the session total lives on the status row.
        assert!(!mark.text.contains('$'), "{}", mark.text);
        // A settle with nothing to settle is a no-op, not a "✓" under a live spinner.
        let settled = state.clone();
        assert_eq!(
            reduce(settled.clone(), StoreAction::TurnSettle { at: 0 }),
            settled
        );
    }

    #[test]
    fn an_interrupted_turn_says_so_and_does_not_wear_a_check() {
        let mut rec = Recorder::new();
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let events = vec![
            rec.emit(
                EventType::MessageStarted,
                message("m-1", json!({"pending": true})),
                Some(SESSION),
            ),
            rec.emit(
                EventType::TurnFinished,
                json!({"sessionId": SESSION, "turnId": "t1", "status": "interrupted"}),
                Some(SESSION),
            ),
        ];
        state = replay_events(state, &events);
        state = apply(state, vec![StoreAction::TurnSettle { at: 0 }]);
        let mark = state.marks.last().unwrap();
        assert!(mark.text.starts_with("⏹ "), "{}", mark.text);
        assert!(mark.text.contains("interrupted"), "{}", mark.text);
    }

    // ---- the take-back, on the posted half ----------------------------------

    #[test]
    fn a_take_back_drops_the_message_and_its_half_written_answer_and_disarms_the_window() {
        let mut rec = Recorder::new();
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let events = vec![
            rec.emit(
                EventType::MessageStarted,
                message(
                    "m-user",
                    json!({"role": "user", "parts": [{"type": "text", "text": "the typo"}]}),
                ),
                Some(SESSION),
            ),
            rec.emit(
                EventType::MessageStarted,
                message("m-1", json!({"pending": true})),
                Some(SESSION),
            ),
            rec.emit(
                EventType::MessageDelta,
                json!({"messageId": "m-1", "delta": "half an ans"}),
                Some(SESSION),
            ),
        ];
        state = replay_events(state, &events);
        state = apply(state, vec![StoreAction::Sent { at: 5_000 }]);
        assert_eq!(state.thread.len(), 2);
        assert_eq!(state.streaming["m-1"], "half an ans");

        state = apply(
            state,
            vec![StoreAction::ThreadDropped {
                session_id: SESSION.into(),
                ids: vec!["m-user".into(), "m-1".into()],
            }],
        );
        assert!(state.thread.is_empty());
        // The live buffer goes with the message.
        assert!(state.streaming.is_empty());
        // The window was armed by the message that just went away.
        assert_eq!(state.last_send_at, None);
    }

    #[test]
    fn a_snapshot_in_flight_when_the_take_back_landed_cannot_resurrect_the_message() {
        let mut rec = Recorder::new();
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let started = rec.emit(
            EventType::MessageStarted,
            message(
                "m-user",
                json!({"role": "user", "parts": [{"type": "text", "text": "the typo"}]}),
            ),
            Some(SESSION),
        );
        state = replay_events(state, &[started]);
        state = apply(
            state,
            vec![StoreAction::ThreadDropped {
                session_id: SESSION.into(),
                ids: vec!["m-user".into()],
            }],
        );

        // The read the server computed BEFORE it deleted the row.
        let mut stale = snapshot_after_outage();
        stale.thread = vec![serde_json::from_value(message(
            "m-user",
            json!({"role": "user", "parts": [{"type": "text", "text": "the typo"}]}),
        ))
        .unwrap()];
        state = apply(
            state,
            vec![StoreAction::Snapshot {
                at: 9_000,
                snapshot: stale,
            }],
        );
        assert!(state.thread.is_empty());

        // …and a fresh read that no longer carries it settles the same way.
        let mut fresh_snap = snapshot_after_outage();
        fresh_snap.thread = vec![];
        state = apply(
            state,
            vec![StoreAction::Snapshot {
                at: 10_000,
                snapshot: fresh_snap,
            }],
        );
        assert!(state.thread.is_empty());
    }

    #[test]
    fn a_take_back_aimed_at_another_session_leaves_the_open_one_alone() {
        let mut rec = Recorder::new();
        let mut state = apply(initial_state(), vec![open(SESSION)]);
        let started = rec.emit(
            EventType::MessageStarted,
            message("m-user", json!({"role": "user"})),
            Some(SESSION),
        );
        state = replay_events(state, &[started]);
        state = apply(
            state,
            vec![StoreAction::ThreadDropped {
                session_id: "other".into(),
                ids: vec!["m-user".into()],
            }],
        );
        let ids: Vec<&str> = state.thread.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["m-user"]);
    }

    // ---- retention (retention.test.ts) --------------------------------------

    /// One complete round: message, tool call, `lines` of live output, result.
    fn round(rec: &mut Recorder, n: usize, lines: usize, session_id: &str) -> Vec<BoughEvent> {
        let message_id = format!("{session_id}-msg-{n}");
        let call_id = format!("{session_id}-call-{n}");
        let mut out = vec![
            rec.emit(
                EventType::MessageStarted,
                {
                    let mut m = message(&message_id, json!({"pending": true}));
                    m["sessionId"] = json!(session_id);
                    m
                },
                Some(session_id),
            ),
            rec.emit(
                EventType::MessagePart,
                json!({"messageId": message_id, "part": {"type": "tool_call", "id": call_id, "name": "run_steps", "input": {"code": "1"}}}),
                Some(session_id),
            ),
        ];
        for i in 0..lines {
            out.push(rec.emit(
                EventType::ToolLog,
                json!({"messageId": message_id, "callId": call_id, "line": format!("line {i} of round {n}")}),
                Some(session_id),
            ));
        }
        out.push(rec.emit(
            EventType::MessagePart,
            json!({"messageId": message_id, "part": {"type": "tool_result", "callId": call_id, "output": "ok", "isError": false}}),
            Some(session_id),
        ));
        out.push(rec.emit(
            EventType::MessageFinished,
            json!({"messageId": message_id}),
            Some(session_id),
        ));
        out
    }

    fn open_session(rec: &Recorder) -> TuiState {
        apply(
            initial_state(),
            vec![
                open(SESSION),
                StoreAction::Snapshot {
                    at: rec.now(),
                    snapshot: SessionSnapshot {
                        session: serde_json::from_value(session(SESSION)).unwrap(),
                        thread: vec![],
                        usage: usage(0, 0, 0.0),
                        effective_model: None,
                        context_limit: None,
                        primed_tags: None,
                        project_rules: None,
                    },
                },
            ],
        )
    }

    #[test]
    fn a_calls_live_output_is_freed_the_moment_its_result_lands() {
        let mut rec = Recorder::new();
        let mut state = open_session(&rec);
        let head = vec![
            rec.emit(EventType::MessageStarted, message("msg-1", json!({"pending": true})), Some(SESSION)),
            rec.emit(
                EventType::MessagePart,
                json!({"messageId": "msg-1", "part": {"type": "tool_call", "id": "call-1", "name": "run_steps", "input": {"code": "1"}}}),
                Some(SESSION),
            ),
        ];
        state = replay_events(state, &head);
        for i in 0..5_000 {
            let log = rec.emit(
                EventType::ToolLog,
                json!({"messageId": "msg-1", "callId": "call-1", "line": format!("{i}")}),
                Some(SESSION),
            );
            state = replay_events(state, &[log]);
        }
        assert_eq!(
            state.tool_logs["call-1"].len(),
            5_000,
            "live output must be retained while running"
        );

        let result = rec.emit(
            EventType::MessagePart,
            json!({"messageId": "msg-1", "part": {"type": "tool_result", "callId": "call-1", "output": "ok", "isError": false}}),
            Some(SESSION),
        );
        state = replay_events(state, &[result]);
        assert!(
            !state.tool_logs.contains_key("call-1"),
            "the buffer must be released with the result"
        );
        // Released, not lost: the finalized result carries the same output.
        let parts = &state.thread.iter().find(|m| m.id == "msg-1").unwrap().parts;
        assert!(parts
            .iter()
            .any(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == "call-1")));
    }

    #[test]
    fn a_long_session_retains_no_output_from_rounds_that_finished() {
        let mut rec = Recorder::new();
        let mut state = open_session(&rec);
        for n in 0..50 {
            let events = round(&mut rec, n, 200, SESSION);
            state = replay_events(state, &events);
        }
        assert_eq!(
            state.tool_logs.len(),
            0,
            "every settled call must have released its buffer"
        );
        assert_eq!(
            state.streaming.len(),
            0,
            "no live text buffer outlives its finalized part"
        );
        assert_eq!(
            state.thread.len(),
            50,
            "the transcript itself is the thing that keeps growing"
        );
    }

    #[test]
    fn output_of_a_call_with_no_result_survives_and_a_session_switch_frees_it() {
        let mut rec = Recorder::new();
        let mut state = open_session(&rec);
        let events = vec![
            rec.emit(EventType::MessageStarted, message("msg-9", json!({"pending": true})), Some(SESSION)),
            rec.emit(
                EventType::MessagePart,
                json!({"messageId": "msg-9", "part": {"type": "tool_call", "id": "call-9", "name": "run_steps", "input": {"code": "1"}}}),
                Some(SESSION),
            ),
            rec.emit(EventType::ToolLog, json!({"messageId": "msg-9", "callId": "call-9", "line": "still going"}), Some(SESSION)),
        ];
        state = replay_events(state, &events);
        assert_eq!(state.tool_logs["call-9"].len(), 1);

        state = apply(state, vec![open("sess-2")]);
        assert!(
            state.tool_logs.is_empty(),
            "the previous session's live output must not follow you"
        );
    }

    #[test]
    fn workflow_log_lines_do_not_outlive_the_session_that_ran_them() {
        let mut rec = Recorder::new();
        let mut state = open_session(&rec);
        for i in 0..200 {
            let log = rec.emit(
                EventType::WorkflowLog,
                json!({"runId": format!("run-{i}"), "line": format!("phase {i}")}),
                Some(SESSION),
            );
            state = replay_events(state, &[log]);
        }
        assert_eq!(
            state.workflow_logs.len(),
            200,
            "a live run's narrator line is state"
        );

        state = apply(state, vec![open("sess-2")]);
        assert_eq!(
            state.workflow_logs.len(),
            0,
            "run ids never recur, so a line kept past its session is unreachable forever"
        );
    }

    #[test]
    fn snapshot_watermarks_are_capped_newest_kept() {
        let rec = Recorder::new();
        let mut state = open_session(&rec);
        let total = RECONCILED_LIMIT * 8;
        for i in 0..total {
            state = apply(
                state,
                vec![StoreAction::Snapshot {
                    at: 10_000 + i as i64,
                    snapshot: SessionSnapshot {
                        session: serde_json::from_value(session(&format!("other-{i}"))).unwrap(),
                        thread: vec![],
                        usage: usage(0, 0, 0.0),
                        effective_model: None,
                        context_limit: None,
                        primed_tags: None,
                        project_rules: None,
                    },
                }],
            );
        }
        assert_eq!(state.reconciled_at.len(), RECONCILED_LIMIT);
        assert!(
            state
                .reconciled_at
                .contains_key(&format!("other-{}", total - 1)),
            "the newest watermark must survive"
        );
        assert!(
            !state.reconciled_at.contains_key("other-0"),
            "the oldest must have been evicted"
        );
    }

    #[test]
    fn the_open_sessions_watermark_is_never_evicted() {
        let rec = Recorder::new();
        let mut state = open_session(&rec);
        state = apply(
            state,
            vec![StoreAction::Snapshot {
                at: 5_000,
                snapshot: SessionSnapshot {
                    session: serde_json::from_value(session(SESSION)).unwrap(),
                    thread: vec![],
                    usage: usage(0, 0, 0.0),
                    effective_model: None,
                    context_limit: None,
                    primed_tags: None,
                    project_rules: None,
                },
            }],
        );
        for i in 0..RECONCILED_LIMIT * 4 {
            state = apply(
                state,
                vec![StoreAction::Snapshot {
                    at: 10_000 + i as i64,
                    snapshot: SessionSnapshot {
                        session: serde_json::from_value(session(&format!("other-{i}"))).unwrap(),
                        thread: vec![],
                        usage: usage(0, 0, 0.0),
                        effective_model: None,
                        context_limit: None,
                        primed_tags: None,
                        project_rules: None,
                    },
                }],
            );
        }
        assert_eq!(
            state.reconciled_at.get(SESSION),
            Some(&5_000),
            "the open session's watermark must be kept"
        );
        assert_eq!(state.reconciled_at.len(), RECONCILED_LIMIT);
        // And it still does its job: an event older than the snapshot is dropped.
        let stale = BoughEvent {
            r#type: EventType::MessageFinished,
            session_id: Some(SESSION.into()),
            seq: 1,
            ts: 4_000,
            data: json!({"messageId": "old"}),
        };
        let held = state.clone();
        assert_eq!(
            replay_events(state, &[stale]),
            held,
            "the surviving watermark must still drop stale events"
        );
    }

    #[test]
    fn the_dedupe_window_and_the_mark_ledger_stay_at_their_caps() {
        let mut rec = Recorder::new();
        let mut state = open_session(&rec);
        for i in 0..DEDUPE_WINDOW * 4 {
            let ev = rec.emit(
                EventType::SessionActivity,
                json!({"sessionId": SESSION, "activity": format!("step {i}")}),
                Some(SESSION),
            );
            state = replay_events(state, &[ev]);
        }
        assert_eq!(
            state.seen.len(),
            DEDUPE_WINDOW,
            "the dedupe window is a window, not a ledger"
        );

        for i in 0..MARK_LIMIT * 3 {
            state = apply(
                state,
                vec![StoreAction::Mark {
                    session_id: SESSION.into(),
                    at: 20_000 + i as i64,
                    text: format!("reverted f{i}"),
                }],
            );
        }
        assert_eq!(state.marks.len(), MARK_LIMIT, "marks are capped");
        assert!(
            state
                .marks
                .last()
                .unwrap()
                .text
                .ends_with(&format!("f{}", MARK_LIMIT * 3 - 1)),
            "the cap must drop the OLDEST marks, keeping the recent ones"
        );
    }

    #[test]
    fn a_long_day_of_use_leaves_every_container_bounded() {
        let mut rec = Recorder::new();
        let mut state = open_session(&rec);
        for s in 0..25 {
            let id = format!("sess-{s}");
            state = apply(
                state,
                vec![
                    StoreAction::Open {
                        session_id: Some(id.clone()),
                    },
                    StoreAction::Snapshot {
                        at: rec.now(),
                        snapshot: SessionSnapshot {
                            session: serde_json::from_value(session(&id)).unwrap(),
                            thread: vec![],
                            usage: usage(0, 0, 0.0),
                            effective_model: None,
                            context_limit: None,
                            primed_tags: None,
                            project_rules: None,
                        },
                    },
                ],
            );
            for n in 0..40 {
                let events = round(&mut rec, n, 50, &id);
                state = replay_events(state, &events);
                let log = rec.emit(
                    EventType::WorkflowLog,
                    json!({"runId": format!("run-{s}-{n}"), "line": "phase"}),
                    Some(&id),
                );
                state = replay_events(state, &[log]);
            }
        }
        let tool_log_lines: usize = state.tool_logs.values().map(Vec::len).sum();
        assert_eq!(tool_log_lines, 0, "dead program output retained");
        assert_eq!(state.streaming.len(), 0, "dead text buffers retained");
        assert!(
            state.workflow_logs.len() <= 40,
            "{} workflow lines retained across 1,000 runs",
            state.workflow_logs.len()
        );
        assert!(
            state.reconciled_at.len() <= RECONCILED_LIMIT,
            "{} watermarks retained",
            state.reconciled_at.len()
        );
        assert_eq!(state.seen.len(), DEDUPE_WINDOW);
        assert!(state.marks.len() <= MARK_LIMIT);
        assert_eq!(
            state.thread.len(),
            40,
            "only the open session's thread is held"
        );
    }

    #[test]
    fn a_role_worker_is_not_a_role_any_more() {
        // Belt-and-braces pin used implicitly by the TS suite: the reducer's
        // payload parsing must reject a role that no longer exists.
        assert!(serde_json::from_value::<Role>(json!("worker")).is_err());
        assert!(serde_json::from_value::<SessionKind>(json!("worker")).is_err());
    }
}
