//! The mind: persistent agency layered ON the turn loop (specs/mind.md).
//!
//! THE INVARIANT THIS HOLDS: **the driver is a state machine over database
//! facts, and every wake goes through the one wake rule.** Nothing here starts
//! a turn itself — a wakeup is a system note posted through
//! `agents::notes::post_system_note`, which already guarantees at-most-one
//! concurrent turn, queue-behind-a-running-turn, and a-stop-stays-stopped.
//! The driver's own state (streaks, watermarks, the pending wake) lives in
//! `session_state`, so a restart mid-wake recovers by re-deriving, never by
//! remembering. The tick path never calls the LLM and never blocks on a turn;
//! the one LLM consumer (rollups) is spawned off the tick and degrades to
//! nothing without the cheap tier.
//!
//! Backoff is why an idle mind is cheap: every idle wakeup doubles the
//! interval (`base * 2^streak`, capped), and any user message resets it. The
//! failure ceiling is why a broken one is bounded: `MAX_CONSECUTIVE_FAILURES`
//! errored wakeups disable the mind with a recorded note, not an eleventh try.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use tokio_util::sync::CancellationToken;

use crate::agents::notes::{post_system_note, NoteDeps, WakeMode};
use crate::agents::with_db;
use crate::schema::parts::{
    Message, MindRollup, MindStep, MindStepType, Part, Role, Session, TurnStatus,
};
use crate::types::{AppCtx, Clock, SharedDb};

// ---------------------------------------------------------------------------
// Tuning (specs/mind.md §4, §6, §7)
// ---------------------------------------------------------------------------

/// Messages a mind turn replays; older rows reach the model as rollups.
pub const MIND_REPLAY_WINDOW: usize = 40;
/// Steps in the recent-stream note.
pub const MIND_STREAM_TAIL: i64 = 30;
/// Per-step clip inside the recent-stream note.
pub const STREAM_CLIP_CHARS: usize = 500;
/// `step()` content ceiling.
pub const STEP_MAX_CHARS: usize = 4_000;
/// Idle backoff: base and cap.
pub const WAKE_BASE_MS: i64 = 120_000;
pub const WAKE_MAX_MS: i64 = 3_600_000;
/// Failure backoff base (same cap).
pub const FAIL_BASE_MS: i64 = 60_000;
/// Consecutive errored wakeups before the mind is disabled.
pub const MAX_CONSECUTIVE_FAILURES: i64 = 10;
/// Rollup fanout: tier k covers F^k steps.
pub const ROLLUP_FANOUT: i64 = 10;
/// Deepest tier the cascade will mint. 10^6 steps is more life than v1 needs.
pub const MAX_ROLLUP_TIER: i64 = 6;
/// Driver cadence, same as the schedule ticker.
pub const TICK_MS: u64 = 30_000;
/// A pending wake with no turn after this long evaporated (server bounce
/// between the note and the turn); the driver clears it and tries again.
pub const PENDING_STALE_MS: i64 = 600_000;

/// The wake note, verbatim. Stable so the mirror can skip it and the model
/// can key off it; the instructions live in the prompt, not here.
pub const WAKE_PREFIX: &str = "[mind wake]";
pub const WAKE_TEXT: &str = "[mind wake] This is your wakeup. Your life summary and \
recent stream are in the system prompt. Choose exactly ONE function from the mind \
section, carry it out, record it with step(), then stop.";

/// `session_state` keys, root-scoped to the mind session itself.
pub mod keys {
    pub const ENABLED: &str = "mind.enabled";
    pub const PERSONA: &str = "mind.persona";
    pub const IDLE_STREAK: &str = "mind.idle_streak";
    pub const FAIL_STREAK: &str = "mind.fail_streak";
    pub const NEXT_WAKE_AT: &str = "mind.next_wake_at";
    pub const PENDING_SINCE: &str = "mind.pending_since";
    pub const LAST_MIRRORED_ID: &str = "mind.last_mirrored_id";
}

// ---------------------------------------------------------------------------
// Pure pieces
// ---------------------------------------------------------------------------

/// `base * 2^streak`, saturating, capped. Threaded values, no clock.
pub fn backoff_ms(base: i64, streak: i64, cap: i64) -> i64 {
    let shift = streak.clamp(0, 62) as u32;
    base.saturating_mul(1i64.checked_shl(shift).unwrap_or(i64::MAX))
        .min(cap)
}

/// The replay window: the last `n` messages, advanced to the next user/system
/// boundary so no assistant tool pair is ever split. An all-supervisor tail
/// falls back to the plain tail rather than an empty thread.
pub fn window_thread(thread: &[Message], n: usize) -> &[Message] {
    if thread.len() <= n {
        return thread;
    }
    let tail = &thread[thread.len() - n..];
    match tail.iter().position(|m| m.role != Role::Supervisor) {
        Some(at) => &tail[at..],
        None => tail,
    }
}

/// A wakeup was idle when it wrote nothing, or nothing but idle steps.
pub fn wakeup_was_idle(steps: &[MindStep]) -> bool {
    steps.iter().all(|s| s.r#type == MindStepType::Idle)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// A message's text parts, joined. The mirror never copies tool traffic.
fn message_text(m: &Message) -> String {
    m.parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// State accessors — every read re-derivable, every write stamped
// ---------------------------------------------------------------------------

fn read_i64(db: &SharedDb, sid: &str, key: &str) -> Option<i64> {
    with_db(db, |d| d.get_state(sid, key))
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
}

fn write_i64(db: &SharedDb, sid: &str, key: &str, value: i64, now: i64) {
    let _ = with_db(db, |d| d.set_state(sid, key, &value.to_string(), now));
}

fn read_str(db: &SharedDb, sid: &str, key: &str) -> Option<String> {
    with_db(db, |d| d.get_state(sid, key)).ok().flatten()
}

fn write_str(db: &SharedDb, sid: &str, key: &str, value: &str, now: i64) {
    let _ = with_db(db, |d| d.set_state(sid, key, value, now));
}

fn clear_key(db: &SharedDb, sid: &str, key: &str) {
    let _ = with_db(db, |d| d.delete_state(sid, key));
}

pub fn is_enabled(db: &SharedDb, sid: &str) -> bool {
    read_str(db, sid, keys::ENABLED).as_deref() == Some("true")
}

/// Flip the mind on or off. Enabling stamps `next_wake_at = now` so the first
/// wakeup lands on the next tick, and zeroes the streaks — a re-enable is a
/// fresh start, not a resumed backoff.
pub fn set_enabled(db: &SharedDb, sid: &str, enabled: bool, now: i64) {
    write_str(db, sid, keys::ENABLED, if enabled { "true" } else { "false" }, now);
    if enabled {
        write_i64(db, sid, keys::NEXT_WAKE_AT, now, now);
        write_i64(db, sid, keys::IDLE_STREAK, 0, now);
        write_i64(db, sid, keys::FAIL_STREAK, 0, now);
        clear_key(db, sid, keys::PENDING_SINCE);
    }
}

// ---------------------------------------------------------------------------
// The prompt notes (specs/mind.md §4) — volatile, rebuilt every turn
// ---------------------------------------------------------------------------

/// Persona, life summary, recent stream — in that order, skipping whatever is
/// empty. Called by the turn runner for `kind: mind` only.
pub fn prompt_notes(db: &SharedDb, session_id: &str) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();

    if let Some(persona) = read_str(db, session_id, keys::PERSONA) {
        if !persona.trim().is_empty() {
            notes.push(format!("## Who you are\n{}", persona.trim()));
        }
    }

    let rollups: Vec<MindRollup> =
        with_db(db, |d| d.mind_rollups(session_id)).unwrap_or_default();
    if !rollups.is_empty() {
        let mut life = String::from(
            "## Your life so far (tiered summary, coarsest first; a summary is an \
index, not testimony — drill into the raw steps when a span matters)\n",
        );
        for r in &rollups {
            life.push_str(&format!(
                "- [tier {} · steps {}–{}] {}\n",
                r.tier, r.first_step_id, r.last_step_id, r.summary
            ));
        }
        notes.push(life.trim_end().to_string());
    }

    let steps: Vec<MindStep> =
        with_db(db, |d| d.mind_steps_tail(session_id, MIND_STREAM_TAIL)).unwrap_or_default();
    if !steps.is_empty() {
        let mut stream = String::from("## The recent stream (oldest first)\n");
        for s in &steps {
            stream.push_str(&format!(
                "[#{} {} · {}] {}\n",
                s.id,
                s.r#type.as_str(),
                s.source,
                clip(&s.content, STREAM_CLIP_CHARS)
            ));
        }
        notes.push(stream.trim_end().to_string());
    }

    notes
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// Ticker seams, mirroring `schedules::TickerDeps`.
#[derive(Clone, Default)]
pub struct MindTickerDeps {
    /// Defaults to [`TICK_MS`].
    pub interval_ms: Option<u64>,
    /// Injected clock. Absent = `ctx.now`.
    pub now: Option<Clock>,
}

/// The production loop. Same shape and stop guarantee as the schedule ticker:
/// no immediate pass at boot, a throwing tick never kills the interval, and
/// once the stopper returns no further pass runs.
pub fn start_mind_ticker(ctx: &AppCtx) -> impl FnOnce() {
    start_mind_ticker_with(ctx, MindTickerDeps::default())
}

pub fn start_mind_ticker_with(ctx: &AppCtx, deps: MindTickerDeps) -> impl FnOnce() {
    let token = CancellationToken::new();
    let stopper = token.clone();
    let ctx = ctx.clone();
    let period = std::time::Duration::from_millis(deps.interval_ms.unwrap_or(TICK_MS));
    tokio::spawn(async move {
        let start = tokio::time::Instant::now() + period;
        let mut interval = tokio::time::interval_at(start, period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => break,
                _ = interval.tick() => {}
            }
            if token.is_cancelled() {
                break;
            }
            let now = match &deps.now {
                Some(clock) => clock(),
                None => (ctx.now)(),
            };
            tick_minds(&ctx, now);
        }
    });
    move || stopper.cancel()
}

/// One sweep over every enabled mind. Never throws; a session whose tick
/// fails is logged and the sweep continues — one broken mind must not stall
/// the others.
pub fn tick_minds(ctx: &AppCtx, now: i64) {
    let minds: Vec<Session> = with_db(&ctx.db, |d| d.mind_sessions()).unwrap_or_default();
    for session in minds {
        if !is_enabled(&ctx.db, &session.id) {
            continue;
        }
        tick_one(ctx, &session, now);
    }
}

/// The per-session pass, in spec order: settle → mirror → wake (§5).
fn tick_one(ctx: &AppCtx, session: &Session, now: i64) {
    let sid = &session.id;

    // 1. Settle a pending wakeup from what the database says happened to it.
    if let Some(pending_since) = read_i64(&ctx.db, sid, keys::PENDING_SINCE) {
        let turns = with_db(&ctx.db, |d| d.turns_for_session(sid)).unwrap_or_default();
        let latest = turns.last().cloned();
        match latest {
            Some(t) if t.created_at >= pending_since => match t.status {
                TurnStatus::Running => return,
                TurnStatus::Interrupted => {
                    // A stop stays stopped — until `bough mind start`.
                    set_enabled(&ctx.db, sid, false, now);
                    clear_key(&ctx.db, sid, keys::PENDING_SINCE);
                    record_note(
                        ctx,
                        sid,
                        "[mind] Stopped by interrupt. The mind stays off; `bough mind start` \
re-enables it.",
                        now,
                    );
                    return;
                }
                TurnStatus::Error | TurnStatus::Orphaned => {
                    let fails = read_i64(&ctx.db, sid, keys::FAIL_STREAK).unwrap_or(0) + 1;
                    write_i64(&ctx.db, sid, keys::FAIL_STREAK, fails, now);
                    clear_key(&ctx.db, sid, keys::PENDING_SINCE);
                    if fails >= MAX_CONSECUTIVE_FAILURES {
                        write_str(&ctx.db, sid, keys::ENABLED, "false", now);
                        record_note(
                            ctx,
                            sid,
                            &format!(
                                "[mind] Disabled after {fails} consecutive failed wakeups \
(last: {}). `bough mind start` re-enables it.",
                                t.error.as_deref().unwrap_or("no error text")
                            ),
                            now,
                        );
                        return;
                    }
                    write_i64(
                        &ctx.db,
                        sid,
                        keys::NEXT_WAKE_AT,
                        now + backoff_ms(FAIL_BASE_MS, fails, WAKE_MAX_MS),
                        now,
                    );
                }
                TurnStatus::Done => {
                    write_i64(&ctx.db, sid, keys::FAIL_STREAK, 0, now);
                    let steps =
                        with_db(&ctx.db, |d| d.mind_steps_for_turn(&t.id)).unwrap_or_default();
                    let idle = if wakeup_was_idle(&steps) {
                        read_i64(&ctx.db, sid, keys::IDLE_STREAK).unwrap_or(0) + 1
                    } else {
                        0
                    };
                    write_i64(&ctx.db, sid, keys::IDLE_STREAK, idle, now);
                    write_i64(
                        &ctx.db,
                        sid,
                        keys::NEXT_WAKE_AT,
                        now + backoff_ms(WAKE_BASE_MS, idle, WAKE_MAX_MS),
                        now,
                    );
                    clear_key(&ctx.db, sid, keys::PENDING_SINCE);
                    spawn_rollup_mint(ctx, sid, now);
                }
            },
            _ => {
                // No turn materialized for the wake. Give the starter the
                // benefit of the doubt for a while, then let go: the note is
                // persisted either way, and the queued drain will find it.
                if now - pending_since > PENDING_STALE_MS {
                    clear_key(&ctx.db, sid, keys::PENDING_SINCE);
                } else {
                    return;
                }
            }
        }
    }

    // 2. Mirror inbound messages into the trajectory, and let a person
    // talking collapse the backoff.
    if mirror_messages(ctx, sid, now) {
        write_i64(&ctx.db, sid, keys::IDLE_STREAK, 0, now);
        let due = read_i64(&ctx.db, sid, keys::NEXT_WAKE_AT).unwrap_or(now);
        write_i64(&ctx.db, sid, keys::NEXT_WAKE_AT, due.min(now + WAKE_BASE_MS), now);
    }

    // 3. Wake, through the one wake rule.
    if ctx.turn_registry.is_running(sid) {
        return;
    }
    let due = read_i64(&ctx.db, sid, keys::NEXT_WAKE_AT).unwrap_or(now);
    if now < due {
        return;
    }
    let clock: Clock = {
        let at = now;
        Arc::new(move || at)
    };
    let delivery = post_system_note(
        ctx,
        sid,
        WAKE_TEXT,
        &NoteDeps {
            now: Some(clock),
            ..Default::default()
        },
    );
    if delivery.message.is_some() {
        write_i64(&ctx.db, sid, keys::PENDING_SINCE, now, now);
    }
}

/// Record a driver note without waking anything — the driver's own notes must
/// never restart the loop they are reporting on.
fn record_note(ctx: &AppCtx, sid: &str, text: &str, now: i64) {
    let clock: Clock = Arc::new(move || now);
    let _ = post_system_note(
        ctx,
        sid,
        text,
        &NoteDeps {
            now: Some(clock),
            wake: WakeMode::Never,
            ..Default::default()
        },
    );
}

/// Mirror user and system messages newer than the watermark into typed steps.
/// Returns whether any USER message arrived (the backoff reset signal). On a
/// mind with no watermark yet the watermark is set to the newest row without
/// mirroring: the trajectory begins at enable time, not with a transcript dump.
fn mirror_messages(ctx: &AppCtx, sid: &str, now: i64) -> bool {
    let messages: Vec<Message> = with_db(&ctx.db, |d| d.messages_for(sid)).unwrap_or_default();
    let Some(newest) = messages.last().map(|m| m.id.clone()) else {
        return false;
    };
    let watermark = read_str(&ctx.db, sid, keys::LAST_MIRRORED_ID);
    let Some(mark) = watermark else {
        write_str(&ctx.db, sid, keys::LAST_MIRRORED_ID, &newest, now);
        return false;
    };
    let start = messages
        .iter()
        .position(|m| m.id == mark)
        .map(|i| i + 1)
        // A vanished watermark (take-back) restarts from the tail rather than
        // re-mirroring the whole thread.
        .unwrap_or(messages.len());
    let mut saw_user = false;
    for m in &messages[start..] {
        let text = message_text(m);
        match m.role {
            Role::User => {
                saw_user = true;
                if !text.trim().is_empty() {
                    let _ = with_db(&ctx.db, |d| {
                        d.add_mind_step(
                            sid,
                            None,
                            m.created_at,
                            MindStepType::Message,
                            "user",
                            &clip(&text, STEP_MAX_CHARS),
                        )
                    });
                }
            }
            Role::System => {
                // The driver's own wake notes and status notes are loop
                // control, not experience.
                if !text.starts_with(WAKE_PREFIX)
                    && !text.starts_with("[mind]")
                    && !text.trim().is_empty()
                {
                    let _ = with_db(&ctx.db, |d| {
                        d.add_mind_step(
                            sid,
                            None,
                            m.created_at,
                            MindStepType::Observation,
                            "system",
                            &clip(&text, STEP_MAX_CHARS),
                        )
                    });
                }
            }
            Role::Supervisor => {}
        }
    }
    write_str(&ctx.db, sid, keys::LAST_MIRRORED_ID, &newest, now);
    saw_user
}

// ---------------------------------------------------------------------------
// Rollups (specs/mind.md §7) — forward-only, cheap-tier, silent on absence
// ---------------------------------------------------------------------------

/// Sessions with a mint in flight. Two ticks must not race the frontier read
/// and mint the same span twice.
static MINTING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn spawn_rollup_mint(ctx: &AppCtx, sid: &str, now: i64) {
    if ctx.cheap.is_none() {
        return;
    }
    {
        let mut minting = MINTING.lock().unwrap_or_else(|p| p.into_inner());
        if !minting.insert(sid.to_string()) {
            return;
        }
    }
    let ctx = ctx.clone();
    let sid = sid.to_string();
    if tokio::runtime::Handle::try_current().is_err() {
        MINTING
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&sid);
        return;
    }
    tokio::spawn(async move {
        mint_rollups(&ctx, &sid, now).await;
        MINTING
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&sid);
    });
}

/// Mint every rollup the trajectory has earned: full spans of `F` at tier 1,
/// then cascade. A declined summary aborts the pass silently — the next
/// settle retries the same span, because the frontier only advances on a
/// minted row.
pub async fn mint_rollups(ctx: &AppCtx, sid: &str, now: i64) {
    let Some(cheap) = ctx.cheap.clone() else {
        return;
    };
    // Tier 1: spans of raw steps.
    loop {
        let frontier = with_db(&ctx.db, |d| d.mind_rollup_frontier(sid, 1)).unwrap_or(0);
        let steps = with_db(&ctx.db, |d| d.mind_steps_after(sid, frontier, ROLLUP_FANOUT))
            .unwrap_or_default();
        if (steps.len() as i64) < ROLLUP_FANOUT {
            break;
        }
        let body = steps
            .iter()
            .map(|s| format!("[{} · {}] {}", s.r#type.as_str(), s.source, clip(&s.content, 400)))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "Summarize this span of an agent's inner life in 2-3 plain sentences. Keep \
concrete names, decisions and outcomes; drop filler. Steps, oldest first:\n{body}"
        );
        let Some(summary) = cheap.summary(&prompt).await else {
            return;
        };
        let (first, last) = (steps.first().unwrap().id, steps.last().unwrap().id);
        if with_db(&ctx.db, |d| {
            d.add_mind_rollup(sid, 1, first, last, summary.trim(), now)
        })
        .is_err()
        {
            return;
        }
    }
    // The cascade: tier k from F tier-(k−1) rollups.
    for tier in 2..=MAX_ROLLUP_TIER {
        loop {
            let frontier = with_db(&ctx.db, |d| d.mind_rollup_frontier(sid, tier)).unwrap_or(0);
            let children = with_db(&ctx.db, |d| d.mind_rollups_after(sid, tier - 1, frontier))
                .unwrap_or_default();
            if (children.len() as i64) < ROLLUP_FANOUT {
                break;
            }
            let children = &children[..ROLLUP_FANOUT as usize];
            let body = children
                .iter()
                .map(|r| format!("[steps {}–{}] {}", r.first_step_id, r.last_step_id, r.summary))
                .collect::<Vec<_>>()
                .join("\n");
            let prompt = format!(
                "Condense these consecutive period summaries of an agent's life into 2-3 \
plain sentences covering the whole span. Keep the few most concrete anchors.\n{body}"
            );
            let Some(summary) = cheap.summary(&prompt).await else {
                return;
            };
            let (first, last) = (
                children.first().unwrap().first_step_id,
                children.last().unwrap().last_step_id,
            );
            if with_db(&ctx.db, |d| {
                d.add_mind_rollup(sid, tier, first, last, summary.trim(), now)
            })
            .is_err()
            {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — the pure math and the driver over a real in-memory db
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testkit::{seed_session, shared_db, SeedOpts};
    use crate::schema::parts::SessionKind;

    fn message(id: &str, session: &str, role: Role, text: &str, at: i64) -> Message {
        Message {
            id: id.into(),
            session_id: session.into(),
            role,
            parts: vec![Part::Text { text: text.into() }],
            pending: false,
            created_at: at,
        }
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_ms(120_000, 0, WAKE_MAX_MS), 120_000);
        assert_eq!(backoff_ms(120_000, 1, WAKE_MAX_MS), 240_000);
        assert_eq!(backoff_ms(120_000, 3, WAKE_MAX_MS), 960_000);
        assert_eq!(backoff_ms(120_000, 20, WAKE_MAX_MS), WAKE_MAX_MS);
        // A pathological streak must not overflow.
        assert_eq!(backoff_ms(120_000, i64::MAX, WAKE_MAX_MS), WAKE_MAX_MS);
    }

    #[test]
    fn window_starts_at_a_user_or_system_boundary() {
        let mut thread: Vec<Message> = Vec::new();
        for i in 0..10 {
            let role = if i % 2 == 0 { Role::User } else { Role::Supervisor };
            thread.push(message(&format!("m{i}"), "s", role, "x", i));
        }
        // Window of 3 starts on a supervisor row (m7) → advanced to m8 (user).
        let w = window_thread(&thread, 3);
        assert_eq!(w.first().unwrap().id, "m8");
        // A window big enough for everything is everything.
        assert_eq!(window_thread(&thread, 100).len(), 10);
        // All-supervisor tails fall back to the plain tail.
        let all_sup: Vec<Message> = (0..5)
            .map(|i| message(&format!("m{i}"), "s", Role::Supervisor, "x", i))
            .collect();
        assert_eq!(window_thread(&all_sup, 2).len(), 2);
    }

    #[test]
    fn idleness_is_no_steps_or_only_idle_steps() {
        let step = |t: MindStepType| MindStep {
            id: 1,
            session_id: "s".into(),
            turn_id: None,
            ts: 0,
            r#type: t,
            source: "self".into(),
            content: "x".into(),
        };
        assert!(wakeup_was_idle(&[]));
        assert!(wakeup_was_idle(&[step(MindStepType::Idle)]));
        assert!(!wakeup_was_idle(&[step(MindStepType::Idle), step(MindStepType::Thought)]));
    }

    #[test]
    fn enabling_resets_streaks_and_stamps_a_due_wake() {
        let db = shared_db();
        let s = seed_session(
            &db,
            SeedOpts {
                kind: Some(SessionKind::Mind),
                ..Default::default()
            },
        );
        write_i64(&db, &s.id, keys::IDLE_STREAK, 7, 1);
        set_enabled(&db, &s.id, true, 5_000);
        assert!(is_enabled(&db, &s.id));
        assert_eq!(read_i64(&db, &s.id, keys::NEXT_WAKE_AT), Some(5_000));
        assert_eq!(read_i64(&db, &s.id, keys::IDLE_STREAK), Some(0));
        set_enabled(&db, &s.id, false, 6_000);
        assert!(!is_enabled(&db, &s.id));
    }

    #[test]
    fn prompt_notes_render_persona_rollups_and_stream_in_order() {
        let db = shared_db();
        let s = seed_session(
            &db,
            SeedOpts {
                kind: Some(SessionKind::Mind),
                ..Default::default()
            },
        );
        write_str(&db, &s.id, keys::PERSONA, "curious, terse", 1);
        with_db(&db, |d| {
            d.add_mind_step(&s.id, None, 1, MindStepType::Thought, "self", "first thought")
        })
        .unwrap();
        with_db(&db, |d| d.add_mind_rollup(&s.id, 1, 1, 10, "an early era", 2)).unwrap();
        let notes = prompt_notes(&db, &s.id);
        assert_eq!(notes.len(), 3);
        assert!(notes[0].contains("curious, terse"));
        assert!(notes[1].contains("an early era"));
        assert!(notes[1].contains("steps 1–10"));
        assert!(notes[2].contains("first thought"));
        // An empty mind renders nothing rather than empty headings.
        let bare = seed_session(&db, SeedOpts::default());
        assert!(prompt_notes(&db, &bare.id).is_empty());
    }

    #[test]
    fn the_mirror_types_user_and_system_rows_and_skips_wake_notes() {
        let db = shared_db();
        let s = seed_session(
            &db,
            SeedOpts {
                kind: Some(SessionKind::Mind),
                ..Default::default()
            },
        );
        let ctx = crate::agents::testkit::app_ctx(&db);
        // First pass only sets the watermark.
        with_db(&db, |d| {
            d.create_message(message("m1", &s.id, Role::User, "old history", 1))
        })
        .unwrap();
        assert!(!mirror_messages(&ctx, &s.id, 10));
        assert!(with_db(&db, |d| d.mind_steps_tail(&s.id, 10))
            .unwrap()
            .is_empty());
        // New rows after the watermark mirror with types; wake notes do not.
        with_db(&db, |d| {
            d.create_message(message("m2", &s.id, Role::User, "hello mind", 2))
        })
        .unwrap();
        with_db(&db, |d| {
            d.create_message(message("m3", &s.id, Role::System, WAKE_TEXT, 3))
        })
        .unwrap();
        with_db(&db, |d| {
            d.create_message(message("m4", &s.id, Role::System, "[job exited] tests green", 4))
        })
        .unwrap();
        assert!(mirror_messages(&ctx, &s.id, 20));
        let steps = with_db(&db, |d| d.mind_steps_tail(&s.id, 10)).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].r#type, MindStepType::Message);
        assert_eq!(steps[0].source, "user");
        assert_eq!(steps[1].r#type, MindStepType::Observation);
        assert_eq!(steps[1].source, "system");
        // Idempotent: nothing new, nothing mirrored twice.
        assert!(!mirror_messages(&ctx, &s.id, 30));
        assert_eq!(with_db(&db, |d| d.mind_steps_tail(&s.id, 10)).unwrap().len(), 2);
    }

    #[test]
    fn rollup_frontier_math_survives_partial_minting() {
        let db = shared_db();
        let s = seed_session(
            &db,
            SeedOpts {
                kind: Some(SessionKind::Mind),
                ..Default::default()
            },
        );
        for i in 0..12 {
            with_db(&db, |d| {
                d.add_mind_step(&s.id, None, i, MindStepType::Thought, "self", "t")
            })
            .unwrap();
        }
        assert_eq!(with_db(&db, |d| d.mind_rollup_frontier(&s.id, 1)).unwrap(), 0);
        let span = with_db(&db, |d| d.mind_steps_after(&s.id, 0, ROLLUP_FANOUT)).unwrap();
        assert_eq!(span.len(), 10);
        with_db(&db, |d| {
            d.add_mind_rollup(&s.id, 1, span[0].id, span[9].id, "era one", 100)
        })
        .unwrap();
        let frontier = with_db(&db, |d| d.mind_rollup_frontier(&s.id, 1)).unwrap();
        assert_eq!(frontier, span[9].id);
        // Only two uncovered steps remain — not enough for the next mint.
        assert_eq!(
            with_db(&db, |d| d.mind_steps_after(&s.id, frontier, ROLLUP_FANOUT))
                .unwrap()
                .len(),
            2
        );
    }
}
