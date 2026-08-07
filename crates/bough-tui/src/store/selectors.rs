//! Derived views over the store (port of the selector half of `src/tui/store.ts`).
//! Everything here is pure: derived, never stored.
//!
//! The small text helpers at the bottom (`fmt_duration`, `fmt_tokens`,
//! `one_line`, `clip`, `plural`, `humanize_retry_reason`) mirror their
//! `src/tui/format.ts` originals. They live here until `format.rs` (row 1.34)
//! lands, at which point these become re-exports/thin calls — the behavior is
//! pinned by the tests either way.

use std::collections::{HashMap, HashSet};

use bough_core::schema::parts::{BackgroundJob, JobStatus, Schedule, TurnStatus, WorkflowStatus};
use bough_core::types::UsageTotals;

use super::state::{SessionRow, TranscriptMark, TuiState, TurnMeter, WorkflowSummary};

/// A turn is in flight in the open session. Derived from the thread, like the server.
pub fn is_busy(state: &TuiState) -> bool {
    state.thread.iter().any(|m| m.pending)
}

/// The tokens a session is charged for: what it sent, what it got back, what it
/// thought. Cache reads/writes are already inside `inputTokens` for billing —
/// adding them here would count the same tokens twice.
pub fn total_tokens(usage: &UsageTotals) -> i64 {
    usage.input_tokens + usage.output_tokens + usage.reasoning_tokens
}

/// How a settled turn reads: `✓ 14s · 3.2k tok`. The glyph carries the outcome;
/// zero tokens are omitted rather than printed as `0 tok`; elapsed and tokens,
/// deliberately NOT cost.
pub fn settled_line(turn: &TurnMeter, ended_at: i64) -> String {
    let glyph = match turn.status {
        Some(TurnStatus::Error) => "✗",
        Some(TurnStatus::Interrupted) => "⏹",
        Some(TurnStatus::Orphaned) => "⚠",
        _ => "✓",
    };
    let mut bits = vec![fmt_duration((ended_at - turn.started_at).max(0))];
    if turn.tokens > 0 {
        bits.push(format!("{} tok", fmt_tokens(turn.tokens)));
    }
    if turn.status == Some(TurnStatus::Interrupted) {
        bits.push("interrupted".to_string());
    }
    if turn.status == Some(TurnStatus::Error) {
        bits.push("failed".to_string());
    }
    format!("{glyph} {}", bits.join(" · "))
}

/// The message's live text, or "" once it finalized.
pub fn live_text<'a>(state: &'a TuiState, message_id: &str) -> &'a str {
    state.streaming.get(message_id).map_or("", String::as_str)
}

/// The marks belonging to one session, oldest first — what the transcript interleaves.
pub fn marks_for<'a>(state: &'a TuiState, session_id: Option<&str>) -> Vec<&'a TranscriptMark> {
    let Some(id) = session_id else {
        return Vec::new();
    };
    state.marks.iter().filter(|m| m.session_id == id).collect()
}

/// The hold the card shows: oldest first, and ONLY a hold that belongs to the
/// conversation on screen or to something running under it. Lineage is walked
/// over `state.sessions` (`originId ?? parentId`), cycle-guarded; `descendants`
/// is the caller's delegate list, because `GET /sessions` hides collapsed kinds.
pub fn current_ask<'a>(
    state: &'a TuiState,
    descendants: &[&str],
) -> Option<&'a bough_core::schema::parts::AskQuestion> {
    let current = state.current_id.as_deref()?;
    let mut mine: HashSet<&str> = HashSet::new();
    mine.insert(current);
    mine.extend(descendants.iter().copied());
    let by_id: HashMap<&str, &super::state::TuiSessionRow> = state
        .sessions
        .iter()
        .map(|s| (s.row.session.id.as_str(), s))
        .collect();
    let belongs = |session_id: &str| -> bool {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = Some(session_id);
        while let Some(id) = cur {
            if seen.contains(id) {
                break;
            }
            if mine.contains(id) {
                return true;
            }
            seen.insert(id);
            cur = by_id.get(id).and_then(|s| {
                s.row
                    .session
                    .origin_id
                    .as_deref()
                    .or(s.row.session.parent_id.as_deref())
            });
        }
        false
    };
    state.asks.iter().find(|q| belongs(&q.session_id))
}

/// One thing running on this session's behalf, with its own numbers. The one
/// shape shells, subagents, workflows and schedules all reduce to, so one rail
/// can hold them and one key can stop them.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveUnit {
    pub kind: LiveUnitKind,
    /// The job id, the session id, the run id, the schedule id.
    pub id: String,
    /// The session a stop is addressed to.
    pub session_id: String,
    pub title: String,
    /// For a schedule: the time UNTIL it fires (negative once due) — a countdown.
    pub elapsed_ms: i64,
    /// This unit's own tokens. None for a shell, which spends none.
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    /// Determinate progress 0..1 when the unit can know it; None must render as
    /// NO bar, never an empty one.
    pub progress: Option<f64>,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveUnitKind {
    Shell,
    Subagent,
    Workflow,
    Schedule,
}

/// Everything running right now, as rows. PURE and parameterized. Ordered
/// oldest-first within kind; shells before agents before runs before schedules,
/// so a row does not move under the cursor while it works.
pub fn live_units(
    jobs: &[BackgroundJob],
    subagents: &[SessionRow],
    workflows: &[WorkflowSummary],
    schedules: &[Schedule],
    now: i64,
) -> Vec<LiveUnit> {
    let mut shells: Vec<&BackgroundJob> = jobs
        .iter()
        .filter(|j| j.status == JobStatus::Running)
        .collect();
    shells.sort_by_key(|j| j.started_at);
    let mut out: Vec<LiveUnit> = shells
        .into_iter()
        .map(|j| LiveUnit {
            kind: LiveUnitKind::Shell,
            id: j.id.clone(),
            session_id: j.session_id.clone(),
            // The NAME the job was started under, falling back to the id.
            title: one_line(if j.name.is_empty() { &j.id } else { &j.name }),
            elapsed_ms: (now - j.started_at).max(0),
            tokens: None,
            cost_usd: None,
            progress: None,
            // ONE LINE, always: a rail row is one screen row.
            detail: Some(one_line(&j.command)),
        })
        .collect();

    let mut agents: Vec<&SessionRow> = subagents.iter().filter(|s| s.busy).collect();
    agents.sort_by_key(|s| s.session.created_at);
    out.extend(agents.into_iter().map(|s| LiveUnit {
        kind: LiveUnitKind::Subagent,
        id: s.session.id.clone(),
        session_id: s.session.id.clone(),
        title: one_line(if s.session.title.is_empty() {
            "subagent"
        } else {
            &s.session.title
        }),
        elapsed_ms: (now - s.session.created_at).max(0),
        tokens: s.tokens,
        cost_usd: s.cost_usd,
        progress: None,
        detail: None,
    }));

    let mut runs: Vec<&WorkflowSummary> = workflows
        .iter()
        .filter(|w| w.status == WorkflowStatus::Running || w.status == WorkflowStatus::Paused)
        .collect();
    runs.sort_by_key(|w| w.created_at);
    out.extend(runs.into_iter().map(|w| LiveUnit {
        kind: LiveUnitKind::Workflow,
        id: w.id.clone(),
        session_id: w.id.clone(),
        title: one_line(if w.name.is_empty() {
            "workflow"
        } else {
            &w.name
        }),
        elapsed_ms: (now - w.created_at).max(0),
        tokens: None,
        cost_usd: None,
        // The one unit that knows how far along it is. Replays count as done.
        progress: if w.agents.total > 0 {
            Some(((w.agents.done + w.agents.cached) as f64 / w.agents.total as f64).min(1.0))
        } else {
            None
        },
        detail: if w.status == WorkflowStatus::Paused {
            Some(format!(
                "paused · {}",
                w.current_phase.as_deref().unwrap_or("no phase")
            ))
        } else {
            w.current_phase.clone()
        },
    }));

    // LAST, below the live work: a schedule is a standing promise. Ordered by
    // creation, not by `nextRunAt` — a fire must not re-sort a row out from
    // under the cursor.
    let mut timers: Vec<&Schedule> = schedules.iter().filter(|s| s.enabled).collect();
    timers.sort_by_key(|s| s.created_at);
    out.extend(timers.into_iter().map(|s| LiveUnit {
        kind: LiveUnitKind::Schedule,
        id: s.id.clone(),
        session_id: s.id.clone(),
        title: one_line(if s.title.is_empty() {
            &s.prompt
        } else {
            &s.title
        }),
        // Countdown, deliberately unclamped: past-due reads as "due".
        elapsed_ms: s.next_run_at - now,
        tokens: None,
        cost_usd: None,
        progress: None,
        detail: Some(s.spec.clone()),
    }));
    out
}

// ---------------------------------------------------------------------------
// Text helpers mirrored from src/tui/format.ts (see module header)
// ---------------------------------------------------------------------------

/// `9s`, `1m04s`, `1h02m`.
pub fn fmt_duration(ms: i64) -> String {
    let total = (ms / 1000).max(0);
    if total < 60 {
        return format!("{total}s");
    }
    let mins = total / 60;
    let secs = total % 60;
    if mins < 60 {
        return format!("{mins}m{secs:02}s");
    }
    format!("{}h{:02}m", mins / 60, mins % 60)
}

/// 1234 → "1.2k", 999 → "999".
pub fn fmt_tokens(n: i64) -> String {
    if n >= 10_000 {
        format!("{}k", (n as f64 / 1000.0).round() as i64)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// 1.234 → "$1.23", 0.0042 → "$0.004" — sub-dollar spend keeps a visible digit.
pub fn fmt_usd(n: f64) -> String {
    if n >= 1.0 {
        format!("${n:.2}")
    } else if n >= 0.001 {
        format!("${n:.3}")
    } else {
        format!("${n:.4}")
    }
}

/// Char-count clip with a `…`.
pub fn clip(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > n {
        format!("{}…", chars[..n].iter().collect::<String>())
    } else {
        s.to_string()
    }
}

/// Text forced onto ONE row: control bytes to spaces, newlines to a visible
/// ` ¶ ` join, tabs to spaces, runs collapsed, trimmed.
pub fn one_line(s: &str) -> String {
    let mut cleaned = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}' => {
                cleaned.push(' ')
            }
            _ => cleaned.push(c),
        }
    }
    // `\s*\r?\n\s*` → " ¶ "
    let mut joined = String::with_capacity(cleaned.len());
    let mut chars = cleaned.chars().peekable();
    let mut pending_ws = String::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() && c != '\n' && c != '\r' {
            pending_ws.push(c);
            continue;
        }
        if c == '\r' || c == '\n' {
            // Consume the whole whitespace run (incl. further newlines).
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            let mut saw_more = true;
            while saw_more {
                saw_more = false;
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() {
                        let n = chars.next().unwrap();
                        if n == '\n' || n == '\r' {
                            saw_more = true;
                        }
                    } else {
                        break;
                    }
                }
            }
            pending_ws.clear();
            joined.push_str(" ¶ ");
            continue;
        }
        if !pending_ws.is_empty() {
            joined.push_str(&pending_ws);
            pending_ws.clear();
        }
        joined.push(c);
    }
    // Tabs → space, runs of 2+ spaces collapse, trim.
    let tabbed: String = joined
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .collect();
    let mut collapsed = String::with_capacity(tabbed.len());
    let mut last_space = false;
    for c in tabbed.chars() {
        if c == ' ' {
            if !last_space {
                collapsed.push(' ');
            }
            last_space = true;
        } else {
            collapsed.push(c);
            last_space = false;
        }
    }
    collapsed.trim().to_string()
}

/// `3 agents`, `1 agent`.
pub fn plural(n: i64, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// A provider's retry reason, reduced to something a person can read.
/// Conservative: lifts a nested JSON `message`, names a well-known status,
/// never classifies an error it does not recognize.
pub fn humanize_retry_reason(raw: &str, max: usize) -> String {
    let text = raw.trim();
    if text.is_empty() {
        return "no reason given".to_string();
    }
    let named = |s: &str| match s {
        "429" => Some("rate limited"),
        "408" => Some("request timed out"),
        "500" => Some("provider error"),
        "502" => Some("provider unreachable"),
        "503" => Some("provider overloaded"),
        "504" => Some("provider timed out"),
        _ => None,
    };
    // First well-known status token with word boundaries.
    let status: Option<String> = {
        let bytes: Vec<char> = text.chars().collect();
        let mut found = None;
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let token: String = bytes[start..i].iter().collect();
                let left_ok = start == 0 || !bytes[start - 1].is_alphanumeric();
                let right_ok = i >= bytes.len() || !bytes[i].is_alphanumeric();
                if left_ok && right_ok && named(&token).is_some() {
                    found = Some(token);
                    break;
                }
            } else {
                i += 1;
            }
        }
        found
    };

    // The provider's own sentence, if it buried one in the JSON:
    // `"message"\s*:\s*"((?:[^"\\]|\\.)*)"`.
    let nested: Option<String> = {
        let mut found = None;
        let mut search = text;
        while let Some(pos) = search.find("\"message\"") {
            let rest = &search[pos + "\"message\"".len()..];
            let rest_trim = rest.trim_start();
            if let Some(after_colon) = rest_trim.strip_prefix(':') {
                let after = after_colon.trim_start();
                if let Some(body) = after.strip_prefix('"') {
                    let mut out = String::new();
                    let mut chars = body.chars();
                    let mut ok = false;
                    while let Some(c) = chars.next() {
                        if c == '\\' {
                            if let Some(n) = chars.next() {
                                out.push('\\');
                                out.push(n);
                            }
                        } else if c == '"' {
                            ok = true;
                            break;
                        } else {
                            out.push(c);
                        }
                    }
                    if ok {
                        found = Some(out);
                        break;
                    }
                }
            }
            search = &search[pos + "\"message\"".len()..];
        }
        found
    };

    let source = nested.unwrap_or_else(|| text.split(['{', '[']).next().unwrap_or("").to_string());
    // Drop the status token itself from the prose — the name is the readable half.
    let without_status = match &status {
        Some(s) => {
            let mut out = String::new();
            let chars: Vec<char> = source.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                if chars[i].is_ascii_digit() {
                    let start = i;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    let token: String = chars[start..i].iter().collect();
                    let left_ok = start == 0 || !chars[start - 1].is_alphanumeric();
                    let right_ok = i >= chars.len() || !chars[i].is_alphanumeric();
                    if !(left_ok && right_ok && token == *s) {
                        out.push_str(&token);
                    }
                } else {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            out
        }
        None => source,
    };
    let prose = without_status
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == ':' || c == '-' || c.is_whitespace())
        .to_string();

    let prefix = status.as_deref().and_then(named).unwrap_or("");
    let body = if !prose.is_empty() && prose != prefix {
        prose.as_str()
    } else {
        ""
    };
    let joined = if !prefix.is_empty() && !body.is_empty() {
        format!("{prefix} · {body}")
    } else if !prefix.is_empty() {
        prefix.to_string()
    } else if !body.is_empty() {
        body.to_string()
    } else {
        text.to_string()
    };
    let chars: Vec<char> = joined.chars().collect();
    if chars.len() > max {
        let head: String = chars[..max - 1].iter().collect();
        format!("{}…", head.trim_end())
    } else {
        joined
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/store.test.ts (selector cases)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::reduce::reduce;
    use super::super::state::*;
    use super::*;
    use bough_core::schema::events::{BoughEvent, EventType};
    use serde_json::json;

    const SESSION: &str = "sess-1";

    fn job(id: &str, name: &str, command: &str, status: &str, started_at: i64) -> BackgroundJob {
        serde_json::from_value(json!({
            "id": id, "name": name, "sessionId": SESSION, "pid": 1, "command": command,
            "status": status, "startedAt": started_at,
        }))
        .unwrap()
    }

    fn subagent_row(
        id: &str,
        title: &str,
        busy: bool,
        created_at: i64,
        tokens: Option<i64>,
    ) -> SessionRow {
        serde_json::from_value(json!({
            "id": id, "title": title, "kind": "root", "createdAt": created_at, "parentId": null,
            "busy": busy, "tokens": tokens,
        }))
        .unwrap()
    }

    fn schedule(id: &str, created_at: i64, next_run_at: i64, enabled: bool) -> Schedule {
        serde_json::from_value(json!({
            "id": id, "title": id, "prompt": "run the bench", "workspace": null,
            "spec": "every:4h", "enabled": enabled, "createdAt": created_at,
            "lastRunAt": null, "nextRunAt": next_run_at,
        }))
        .unwrap()
    }

    fn workflow(id: &str, created_at: i64) -> WorkflowSummary {
        serde_json::from_value(json!({
            "id": id, "name": "bench", "description": "", "status": "running",
            "currentPhase": "measure", "phases": [],
            "agents": {"total": 8, "done": 2, "cached": 1, "running": 2, "queued": 3, "failed": 0},
            "result": null, "error": null, "resumeOf": null, "createdAt": created_at,
            "finishedAt": null, "scriptFile": "x.js",
        }))
        .unwrap()
    }

    #[test]
    fn live_units_attributes_every_running_thing_separately() {
        let now = 100_000;
        let jobs = vec![
            job(
                "bg_7",
                "the long sleep",
                "sleep 90",
                "running",
                now - 30_000,
            ),
            job("bg_6", "finished one", "done", "exited", now - 60_000),
        ];
        let subagents = vec![
            subagent_row("sub-1", "review app.ts", true, now - 45_000, Some(3_200)),
            subagent_row("sub-2", "finished", false, now - 90_000, None),
        ];
        let workflows = vec![workflow("run-1", now - 120_000)];
        let units = live_units(&jobs, &subagents, &workflows, &[], now);

        let labels: Vec<String> = units
            .iter()
            .map(|u| format!("{:?}:{}", u.kind, u.id).to_lowercase())
            .collect();
        assert_eq!(
            labels,
            vec!["shell:bg_7", "subagent:sub-1", "workflow:run-1"]
        );
        assert_eq!(units[0].elapsed_ms, 30_000);
        assert_eq!(units[0].tokens, None);
        assert_eq!(units[0].detail.as_deref(), Some("sleep 90"));
        assert_eq!(units[1].tokens, Some(3_200));
        // A run is the one unit that knows how far along it is — replays count as done.
        assert_eq!(units[2].progress, Some(3.0 / 8.0));
        // …and everything else must report NO progress rather than an invented bar.
        assert_eq!(units[0].progress, None);
        assert_eq!(units[1].progress, None);
    }

    #[test]
    fn enabled_schedules_ride_the_rail_as_countdowns_below_the_live_work() {
        let now = 100_000;
        let jobs = vec![job(
            "bg_7",
            "dev server",
            "npm run dev",
            "running",
            now - 30_000,
        )];
        let schedules = vec![
            // Created later but due sooner: order is by CREATION.
            schedule("soon", 2, now + 10_000, true),
            schedule("later", 1, now + 7_200_000, true),
            schedule("off", 3, now + 1, false),
        ];
        let units = live_units(&jobs, &[], &[], &schedules, now);
        let labels: Vec<String> = units
            .iter()
            .map(|u| format!("{:?}:{}", u.kind, u.id).to_lowercase())
            .collect();
        assert_eq!(
            labels,
            vec!["shell:bg_7", "schedule:later", "schedule:soon"]
        );
        // The countdown, unclamped.
        assert_eq!(units[2].elapsed_ms, 10_000);
        assert_eq!(units[1].detail.as_deref(), Some("every:4h"));
        assert_eq!(units[1].tokens, None);
        assert_eq!(units[1].progress, None);
    }

    #[test]
    fn a_multi_line_command_still_makes_one_rail_row() {
        let jobs = vec![job(
            "bg_1",
            "webhook POST every 10s",
            "for i in 1 2 3; do\n  echo \"request $i\"\n  sleep 10\ndone",
            "running",
            0,
        )];
        let units = live_units(&jobs, &[], &[], &[], 1_000);
        assert_eq!(units.len(), 1);
        let detail = units[0].detail.as_deref().unwrap();
        assert!(!detail.contains('\n'), "{detail}");
        // The join is MARKED rather than silently closed up.
        assert!(
            detail.contains("for i in 1 2 3; do ¶ echo \"request $i\""),
            "{detail}"
        );
    }

    #[test]
    fn the_ask_card_shows_only_this_conversations_holds_and_its_delegates() {
        let hold = |id: &str, session_id: &str| -> bough_core::schema::parts::AskQuestion {
            serde_json::from_value(json!({
                "id": id, "sessionId": session_id, "messageId": format!("m-{id}"),
                "question": format!("q {id}"), "status": "pending", "ts": 1,
            }))
            .unwrap()
        };
        let mut state = reduce(
            initial_state(),
            StoreAction::Open {
                session_id: Some(SESSION.into()),
            },
        );
        state = reduce(
            state,
            StoreAction::Sessions {
                sessions: vec![
                    serde_json::from_value(json!({"id": SESSION, "title": "mine", "kind": "root", "createdAt": 1, "parentId": null, "busy": false})).unwrap(),
                    serde_json::from_value(json!({"id": "other", "title": "theirs", "kind": "root", "createdAt": 2, "parentId": null, "busy": false})).unwrap(),
                    serde_json::from_value(json!({"id": "branch", "title": "fork of mine", "kind": "fork", "createdAt": 3, "parentId": null, "originId": SESSION, "busy": false})).unwrap(),
                ],
            },
        );

        // Another root's hold: never mine, whatever order it arrived in.
        state = reduce(
            state,
            StoreAction::Event {
                event: BoughEvent {
                    r#type: EventType::AskQuestion,
                    session_id: Some("other".into()),
                    seq: 1,
                    ts: 1,
                    data: json!({"id": "foreign", "sessionId": "other", "messageId": "m-foreign", "question": "q", "status": "pending", "ts": 1}),
                },
            },
        );
        assert!(current_ask(&state, &[]).is_none());

        // My own hold surfaces even though the foreign one is older.
        state = reduce(
            state,
            StoreAction::Event {
                event: BoughEvent {
                    r#type: EventType::AskQuestion,
                    session_id: Some(SESSION.into()),
                    seq: 2,
                    ts: 2,
                    data: json!({"id": "mine", "sessionId": SESSION, "messageId": "m-mine", "question": "q", "status": "pending", "ts": 1}),
                },
            },
        );
        assert_eq!(current_ask(&state, &[]).unwrap().id, "mine");

        // A branch of mine is mine, resolved through `originId`.
        let mut branch_only = state.clone();
        branch_only.asks = vec![hold("fromBranch", "branch")];
        assert_eq!(current_ask(&branch_only, &[]).unwrap().id, "fromBranch");

        // A SUBAGENT's hold stays answerable only through the delegate list.
        let mut delegate_only = state.clone();
        delegate_only.asks = vec![hold("fromAgent", "agent-1")];
        assert!(current_ask(&delegate_only, &[]).is_none());
        assert_eq!(
            current_ask(&delegate_only, &["agent-1"]).unwrap().id,
            "fromAgent"
        );

        // No conversation open: nothing may claim the composer.
        let mut nothing_open = state.clone();
        nothing_open.current_id = None;
        assert!(current_ask(&nothing_open, &[]).is_none());
    }

    #[test]
    fn settled_line_formats() {
        let turn = |status: Option<bough_core::schema::parts::TurnStatus>, tokens: i64| TurnMeter {
            session_id: SESSION.into(),
            started_at: 0,
            base_tokens: 0,
            base_cost_usd: 0.0,
            tokens,
            cost_usd: 0.0,
            ended_at: Some(14_000),
            status,
        };
        use bough_core::schema::parts::TurnStatus as T;
        assert_eq!(
            settled_line(&turn(Some(T::Done), 3_200), 14_000),
            "✓ 14s · 3.2k tok"
        );
        assert_eq!(settled_line(&turn(Some(T::Done), 0), 14_000), "✓ 14s");
        assert_eq!(
            settled_line(&turn(Some(T::Interrupted), 100), 14_000),
            "⏹ 14s · 100 tok · interrupted"
        );
        assert_eq!(
            settled_line(&turn(Some(T::Error), 0), 14_000),
            "✗ 14s · failed"
        );
        assert_eq!(settled_line(&turn(Some(T::Orphaned), 0), 14_000), "⚠ 14s");
    }

    #[test]
    fn helper_formats_match_the_ts_originals() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1234), "1.2k");
        assert_eq!(fmt_tokens(15_000), "15k");
        assert_eq!(fmt_duration(9_000), "9s");
        assert_eq!(fmt_duration(64_000), "1m04s");
        assert_eq!(fmt_duration(3_720_000), "1h02m");
        assert_eq!(fmt_usd(1.234), "$1.23");
        assert_eq!(fmt_usd(0.0042), "$0.004");
        assert_eq!(plural(1, "schedule"), "1 schedule");
        assert_eq!(plural(3, "artifact"), "3 artifacts");
        assert_eq!(one_line("a\nb"), "a ¶ b");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("ab", 3), "ab");
    }

    #[test]
    fn humanize_retry_reason_lifts_the_buried_message_and_names_the_status() {
        assert_eq!(humanize_retry_reason("", 60), "no reason given");
        assert_eq!(humanize_retry_reason("429", 60), "rate limited");
        let raw = r#"openrouter: 429 {"error":{"message":"Provider returned error"}}"#;
        let out = humanize_retry_reason(raw, 60);
        assert!(out.starts_with("rate limited"), "{out}");
        assert!(out.contains("Provider returned error"), "{out}");
        assert!(!out.contains('{'), "{out}");
        // An unfamiliar reason is shown, just shorter — never classified.
        assert_eq!(
            humanize_retry_reason("weird thing happened", 60),
            "weird thing happened"
        );
        assert!(humanize_retry_reason(&"x".repeat(200), 60).chars().count() <= 60);
    }
}
