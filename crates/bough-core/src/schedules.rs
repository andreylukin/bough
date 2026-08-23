//! The schedule ticker + fire + report-back (port of `src/schedules.ts`) and
//! the spec grammar's pure math (port of `src/hostfn/schedule.ts`'s
//! `parseSpec`/`nextRun` — the CRUD half lands with `hostfn::schedule`).
//!
//! The invariant: **a schedule that missed N slots fires ONCE.** Three
//! load-bearing details: `due_schedules(now)` returns each enabled row once
//! (no catch-up loop); the advance happens BEFORE the fire (a throwing fire
//! must not leave the row due); `now` is threaded in, never read inside.
//! `fire_schedule` never panics (timer-callback context); the report-back
//! note's outcome is read from the DATABASE, not the settled future.
//!
//! The arithmetic half of the same invariant: **`next_run_at` is always
//! computed FROM NOW, never from the stale stored value.** A laptop closed
//! overnight with an `every:30m` schedule wakes up 16 slots behind; advancing
//! from `now` means one run, then the cadence resumes. [`next_run`] measures
//! from the instant it is handed, so the catch-up rule is provable as
//! arithmetic.
//!
//! WHAT FIRING IS: a fresh **root** session titled from the schedule, with the
//! schedule's prompt posted into it as an ordinary user message — collapsed
//! under the creating conversation (`kind: "schedule_run"`) when that
//! conversation still exists, a plain root otherwise.

use std::sync::{Arc, LazyLock, Mutex};

use chrono::{DateTime, Duration, Local, LocalResult, NaiveDateTime, TimeZone};
use futures::future::BoxFuture;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::errors::{BoughError, ErrorKind};
use crate::hostfn::schedule::{ParsedSpec, SPEC_HELP};
use crate::schema::events::{BoughEvent, EventInput, EventType};
use crate::schema::parts::{Message, Part, Role, Schedule, Session, SessionKind};
use crate::types::{AppCtx, Clock, Db};

pub const TICK_MS: u64 = 30_000;

/// Stable marker text the creator's model and UI key off.
pub const SCHEDULE_NOTE_PREFIX: &str = "[schedule fired]";

// ---------------------------------------------------------------------------
// The grammar (pure)
// ---------------------------------------------------------------------------

static EVERY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^every:(\d+)(m|h|d)$").unwrap());
static DAILY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^daily@(\d{1,2}):(\d{2})$").unwrap());

/// Parse a spec string, or `None` when it does not match the grammar.
///
/// N ≥ 1: `every:0m` would parse to a zero interval, and a schedule whose next
/// run is always "now" fires on every single tick forever.
pub fn parse_spec(spec: &str) -> Option<ParsedSpec> {
    if let Some(caps) = EVERY_RE.captures(spec) {
        let n: i64 = caps[1].parse().ok()?;
        if n < 1 {
            return None;
        }
        let unit_ms: i64 = match &caps[2] {
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            _ => unreachable!("the regex admits only m|h|d"),
        };
        return Some(ParsedSpec::Every {
            ms: n.checked_mul(unit_ms)?,
        });
    }
    if let Some(caps) = DAILY_RE.captures(spec) {
        let hh: u8 = caps[1].parse().ok()?;
        let mm: u8 = caps[2].parse().ok()?;
        if hh > 23 || mm > 59 {
            return None;
        }
        return Some(ParsedSpec::Daily { hh, mm });
    }
    None
}

/// Resolve a local wall-clock time to an instant, with the DST cases decided
/// EXPLICITLY (the TS relied on `Date.prototype.setHours`, which ECMA-262 pins
/// to the same two choices):
///
/// - **Ambiguous** (fall-back repeats the hour): the EARLIEST occurrence — the
///   pre-transition offset, the first time the clock shows HH:MM.
/// - **Nonexistent** (spring-forward skips the hour): the instant the
///   pre-transition offset names, which for the one-hour gap every US/EU zone
///   uses is the same instant as wall+1h under the new offset. "daily@02:30"
///   fires at 03:30 on the one morning 02:30 never happens.
fn resolve_local<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime) -> DateTime<Tz> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(earliest, _latest) => earliest,
        LocalResult::None => match tz.from_local_datetime(&(naive + Duration::hours(1))) {
            LocalResult::Single(dt) => dt,
            LocalResult::Ambiguous(earliest, _latest) => earliest,
            // No real zone has two adjacent gaps; interpret as UTC rather
            // than panic in a timer path.
            LocalResult::None => tz.from_utc_datetime(&naive),
        },
    }
}

/// [`next_run_parsed`] against an explicit zone — the seam that makes the DST
/// cases testable deterministically (production uses [`Local`]).
pub fn next_run_in<Tz: TimeZone>(tz: &Tz, spec: ParsedSpec, from: i64) -> i64 {
    match spec {
        ParsedSpec::Every { ms } => from + ms,
        ParsedSpec::Daily { hh, mm } => {
            let local = tz
                .timestamp_millis_opt(from)
                .single()
                .expect("an instant maps to exactly one local time");
            let date = local.date_naive();
            let wall = |d: chrono::NaiveDate| {
                resolve_local(
                    tz,
                    d.and_hms_opt(hh as u32, mm as u32, 0)
                        .expect("parse_spec bounds hh/mm"),
                )
            };
            let today = wall(date);
            // Strictly after, never equal: `next_run` is called at fire time
            // with the firing instant as `from`, and a result equal to `from`
            // would be due again on the very next tick.
            if today.timestamp_millis() > from {
                today.timestamp_millis()
            } else {
                wall(date.succ_opt().expect("not the end of the calendar")).timestamp_millis()
            }
        }
    }
}

/// The next fire time strictly after `from` (epoch ms), for an
/// already-parsed spec. `daily@` resolves in LOCAL wall-clock time — the run
/// stays at HH:MM local on either side of a DST transition, which is what a
/// user who asked for "every morning at nine" means.
pub fn next_run_parsed(spec: ParsedSpec, from: i64) -> i64 {
    next_run_in(&Local, spec, from)
}

/// The next fire time strictly after `from` (epoch ms). Errors with a 400
/// `ScheduleError` naming the grammar on a spec that does not parse.
pub fn next_run(spec: &str, from: i64) -> Result<i64, BoughError> {
    let parsed = parse_spec(spec).ok_or_else(|| {
        BoughError::http(
            400,
            ErrorKind::Schedule,
            format!("invalid schedule spec: {spec} — use {SPEC_HELP}"),
        )
    })?;
    Ok(next_run_parsed(parsed, from))
}

// ---------------------------------------------------------------------------
// Firing
// ---------------------------------------------------------------------------

/// What one firing produced: the fresh session and its prompt message.
#[derive(Clone, Debug)]
pub struct FiredSchedule {
    pub session: Session,
    pub message: Message,
}

/// Where a failure to fire (or to report back) is reported. Tests pass a
/// collector; production logs.
pub type ReportError = Arc<dyn Fn(&BoughError, &Schedule) + Send + Sync>;

/// How a fired run ended — the outcome matrix the report-back note keys off.
/// Mirrors `SubagentResult["status"]` (agents/subagent, row 2.2); the wiring
/// there maps 1:1 when it lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiringStatus {
    Done,
    Error,
    Interrupted,
    Orphaned,
}

/// How the run ended, in words the creator can act on — distinct outcomes,
/// distinct first words. Verbatim product surface.
pub fn ran_text(status: FiringStatus) -> &'static str {
    match status {
        FiringStatus::Done => "finished",
        FiringStatus::Error => "FAILED — its turn errored, and the report below carries the error",
        FiringStatus::Interrupted => "was STOPPED before it finished",
        FiringStatus::Orphaned => "ended without a completed turn",
    }
}

/// What the fired session's turn left behind, read from the DATABASE.
#[derive(Clone, Debug, PartialEq)]
pub struct FiringOutcome {
    pub status: FiringStatus,
    /// The run's final text. Empty = "No report." in the note.
    pub report: String,
}

/// The `agents::subagent::build_result` seam: read how the fired session's
/// turn ended `(ctx, fired_session_id, turn_message_id)`. Absent = the real
/// result builder; tests inject a scripted one.
pub type BuildOutcome =
    Arc<dyn Fn(&AppCtx, &str, &str) -> Result<FiringOutcome, BoughError> + Send + Sync>;

/// The `agents::notes::post_system_note` seam `(ctx, session_id, text)`:
/// persist + announce + wake-if-idle. Never errors (the TS contract). Absent =
/// the real note post; tests inject a recorder.
pub type PostNote = Arc<dyn Fn(&AppCtx, &str, &str) + Send + Sync>;

impl From<crate::agents::subagent::SubagentStatus> for FiringStatus {
    fn from(status: crate::agents::subagent::SubagentStatus) -> FiringStatus {
        use crate::agents::subagent::SubagentStatus as S;
        match status {
            S::Done => FiringStatus::Done,
            S::Error => FiringStatus::Error,
            S::Interrupted => FiringStatus::Interrupted,
            S::Orphaned => FiringStatus::Orphaned,
        }
    }
}

/// What starting the fired turn did — the Rust shape of the TS
/// "`startTurn` returned a Promise / a plain value / threw" trichotomy.
pub enum StartOutcome {
    /// The turn is running; the future resolves when it settles. `Err` = it
    /// failed after starting (reported, then the outcome note still posts —
    /// a failed run is precisely the firing the creator needs to hear about).
    Settles(BoxFuture<'static, Result<(), BoughError>>),
    /// The starter finished synchronously (the TS non-Promise return): settle
    /// immediately.
    Done,
    /// The starter threw synchronously. The firing fails (reported), but the
    /// session and its message SURVIVE — the user can see what was supposed
    /// to run and post into it.
    Failed(BoughError),
}

/// How the fired turn is started. Absent = `ctx.turn_starter()` with the
/// settle future waiting on the bus for the fired session's `turn.finished`.
pub type FireStarter = Arc<dyn Fn(&AppCtx, &Session, &Message) -> StartOutcome + Send + Sync>;

/// The seams [`fire_schedule_with`] takes. All default to production behavior.
#[derive(Clone, Default)]
pub struct FireDeps {
    /// Injected clock. Absent = `ctx.now`.
    pub now: Option<Clock>,
    pub report_error: Option<ReportError>,
    /// Overrides how the fired turn is started (tests script outcomes here).
    pub start: Option<FireStarter>,
    /// The agents-side halves of the report-back; absent = the real
    /// `build_result` / `post_system_note`.
    pub build_outcome: Option<BuildOutcome>,
    pub post_note: Option<PostNote>,
}

fn report_via(deps: &FireDeps, err: &BoughError, schedule: &Schedule) {
    match &deps.report_error {
        Some(f) => f(err, schedule),
        None => tracing::error!(
            "schedule {} ({}) failed to fire: {err}",
            schedule.id,
            schedule.title
        ),
    }
}

fn lock_db(ctx: &AppCtx) -> Result<std::sync::MutexGuard<'_, dyn Db + 'static>, BoughError> {
    ctx.db.lock().map_err(|_| {
        BoughError::http(
            500,
            ErrorKind::Schedule,
            "schedules: the database lock is poisoned",
        )
    })
}

/// Fire one schedule: a fresh root session, the prompt as its first user
/// message, and a turn on it — with production defaults. **Never panics** (it
/// is called from a timer task with nobody to report to); `None` = the firing
/// failed after being reported.
pub fn fire_schedule(ctx: &AppCtx, schedule: &Schedule) -> Option<FiredSchedule> {
    fire_schedule_with(ctx, schedule, &FireDeps::default())
}

/// [`fire_schedule`] with its seams exposed.
///
/// The session is announced before the message, and the message before the
/// turn, so a live TUI renders the new session already carrying its prompt
/// rather than an empty card that fills in a beat later.
pub fn fire_schedule_with(
    ctx: &AppCtx,
    schedule: &Schedule,
    deps: &FireDeps,
) -> Option<FiredSchedule> {
    // The never-panics contract, honored literally: a panicking starter (or a
    // poisoned lock surfacing as one) is caught, reported, and answered None —
    // rows already written (session, message) survive.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        try_fire(ctx, schedule, deps)
    }));
    match caught {
        Ok(Ok(fired)) => Some(fired),
        Ok(Err(err)) => {
            report_via(deps, &err, schedule);
            None
        }
        Err(panic) => {
            let text = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic".to_string());
            report_via(
                deps,
                &BoughError::http(500, ErrorKind::Schedule, format!("firing panicked: {text}")),
                schedule,
            );
            None
        }
    }
}

fn try_fire(
    ctx: &AppCtx,
    schedule: &Schedule,
    deps: &FireDeps,
) -> Result<FiredSchedule, BoughError> {
    let now = deps.now.clone().unwrap_or_else(|| ctx.now.clone());

    // The conversation that created the schedule, when it still exists. A
    // firing COLLAPSES under it (`kind: "schedule_run"`) instead of standing
    // as a root of its own: a daily schedule was adding a top-level
    // conversation a day, none of which the user opened, and the listing is
    // also the switcher. Falls back to a root when there is nobody to collapse
    // under — a schedule made over REST carries no `sessionId`, and a creator
    // can have been deleted since. A collapsed session with no reachable
    // origin would be invisible in every listing, which is a worse failure
    // than an untidy one.
    let creator = match &schedule.session_id {
        Some(sid) => lock_db(ctx)?.get_session(sid)?,
        None => None,
    };
    let session = lock_db(ctx)?.create_session(Session {
        id: uuid::Uuid::new_v4().to_string(),
        title: schedule.title.clone(),
        kind: if creator.is_some() {
            SessionKind::ScheduleRun
        } else {
            SessionKind::Root
        },
        created_at: now(),
        // A fired session inherits nothing: no parent thread. The prompt is
        // the whole briefing. The lineage edge below is for VISIBILITY and
        // carries no context.
        parent_id: None,
        origin_id: creator.as_ref().map(|c| c.id.clone()),
        // No `originMessageId`: nothing in a thread asked for this — the
        // clock did — so the run hangs under the conversation, not a turn.
        origin_message_id: None,
        workspace: schedule.workspace.clone(),
        origin_dir: schedule.workspace.clone(),
        base: None,
        model: None,
        effort: None,
        draft: None,
        context_tokens: None,
        cached_tokens: None,
        last_llm_at: None,
        outcome_ok: None,
        description: None,
    })?;
    ctx.bus.publish(EventInput {
        r#type: EventType::SessionCreated,
        session_id: Some(session.id.clone()),
        data: serde_json::to_value(&session).unwrap_or_default(),
    });

    let message = lock_db(ctx)?.create_message(Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session.id.clone(),
        role: Role::User,
        parts: vec![Part::Text {
            text: schedule.prompt.clone(),
        }],
        pending: false,
        created_at: now(),
    })?;
    index_quietly(ctx, &message);
    ctx.bus.publish(EventInput {
        r#type: EventType::MessageStarted,
        session_id: Some(session.id.clone()),
        data: serde_json::to_value(&message).unwrap_or_default(),
    });

    // Fire and forget, like the HTTP post path: the turn runs for minutes and
    // there is no response to hold open. An absent starter is not an error —
    // the session is still there with its prompt.
    let outcome = match &deps.start {
        Some(start) => Some(start(ctx, &session, &message)),
        None => ctx.turn_starter().map(|starter| {
            starter.start_turn(ctx, &session, &message);
            // The Rust `TurnStarter` is fire-and-forget (no promise to chain),
            // so "the run settled" is observed on the bus: the runner
            // publishes exactly one `turn.finished` per turn for the fired
            // session. NOTE (delta from the TS port): a turn that fails to
            // START never publishes it, and that firing's note is simply
            // never posted — the same silence TS had for a never-settling
            // promise.
            StartOutcome::Settles(turn_settled(ctx.bus.clone(), session.id.clone()))
        }),
    };
    if let Some(outcome) = outcome {
        // The firing's other half: when the run settles, its outcome goes back
        // to the conversation that created the schedule as a system note,
        // which wakes that model if it is idle. Settled EITHER way — a failed
        // run is precisely the firing the creator needs to hear about — and
        // read from the database afterwards, so the note reports how the turn
        // actually ended rather than how it was meant to.
        match outcome {
            StartOutcome::Failed(err) => return Err(err),
            StartOutcome::Done => settle(ctx, schedule, &session.id, deps),
            StartOutcome::Settles(fut) => {
                let ctx = ctx.clone();
                let schedule = schedule.clone();
                let deps = deps.clone();
                let fired_id = session.id.clone();
                tokio::spawn(async move {
                    if let Err(err) = fut.await {
                        report_via(&deps, &err, &schedule);
                    }
                    settle(&ctx, &schedule, &fired_id, &deps);
                });
            }
        }
    }
    Ok(FiredSchedule { session, message })
}

/// Resolves when the bus announces `turn.finished` for `session_id` —
/// production's "the fired run settled" signal.
fn turn_settled(
    bus: Arc<crate::bus::Bus>,
    session_id: String,
) -> BoxFuture<'static, Result<(), BoughError>> {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx = Mutex::new(Some(tx));
    let sid = session_id.clone();
    let id = bus.subscribe(Arc::new(move |event: &BoughEvent| {
        if event.r#type == EventType::TurnFinished
            && event.session_id.as_deref() == Some(sid.as_str())
        {
            if let Ok(mut guard) = tx.lock() {
                if let Some(tx) = guard.take() {
                    let _ = tx.send(());
                }
            }
        }
    }));
    let bus_out = bus.clone();
    Box::pin(async move {
        let _ = rx.await;
        bus_out.unsubscribe(id);
        Ok(())
    })
}

/// Run the report-back, reporting (never propagating) its failure — it is a
/// completion callback with nobody to throw to.
fn settle(ctx: &AppCtx, schedule: &Schedule, fired_session_id: &str, deps: &FireDeps) {
    if let Err(err) = note_firing_outcome(ctx, schedule, fired_session_id, deps) {
        report_via(deps, &err, schedule);
    }
}

/// The report-back note, exactly three lines joined with `\n`. Verbatim
/// product surface — the creator's model and the TUI both key off it.
pub fn firing_note_text(
    title: &str,
    fired_session_id: &str,
    status: FiringStatus,
    report: &str,
) -> String {
    let report_line = if report.is_empty() {
        "No report.".to_string()
    } else {
        format!("Report:\n{report}")
    };
    format!(
        "{SCHEDULE_NOTE_PREFIX} \"{title}\" {} (session {fired_session_id}).\n{report_line}\n\
         Act on it only if it needs something — the run is its own session, and this note is \
         its outcome.",
        ran_text(status)
    )
}

/// Tell the creating conversation how a firing went. A schedule with no
/// creating session reports to nobody, and that is the correct outcome, not
/// an error. The outcome is read from the DATABASE (the fired session's last
/// turn row), not from the settled future, so the note and the transcript can
/// never disagree.
fn note_firing_outcome(
    ctx: &AppCtx,
    schedule: &Schedule,
    fired_session_id: &str,
    deps: &FireDeps,
) -> Result<(), BoughError> {
    let Some(creator_id) = &schedule.session_id else {
        return Ok(());
    };
    let last_turn = lock_db(ctx)?.turns_for_session(fired_session_id)?.pop();
    let outcome = match last_turn {
        Some(turn) => Some(match &deps.build_outcome {
            Some(build) => build(ctx, fired_session_id, &turn.message_id)?,
            None => {
                // The real result builder (agents/subagent). With no
                // `changed_files` callback the future resolves on first poll —
                // it awaits nothing — so `block_on` here cannot stall the
                // runtime.
                let result = futures::executor::block_on(crate::agents::subagent::build_result(
                    &ctx.db,
                    fired_session_id,
                    &turn.message_id,
                    None,
                    crate::agents::subagent::InterruptCause::default(),
                ));
                FiringOutcome {
                    status: result.status.into(),
                    report: result.report,
                }
            }
        }),
        None => None,
    };
    let status = outcome
        .as_ref()
        .map(|o| o.status)
        .unwrap_or(FiringStatus::Orphaned);
    let report = outcome.map(|o| o.report).unwrap_or_default();
    let text = firing_note_text(&schedule.title, fired_session_id, status, &report);
    match &deps.post_note {
        Some(post) => post(ctx, creator_id, &text),
        None => {
            // The real note post (agents/notes) — persist, announce, and the
            // ONE wake rule. `postSystemNote` answers `dropped` for a missing
            // creator, and that is the correct outcome, not an error.
            let note_deps = crate::agents::notes::NoteDeps {
                now: deps.now.clone(),
                ..Default::default()
            };
            crate::agents::notes::post_system_note(ctx, creator_id, &text, &note_deps);
        }
    }
    Ok(())
}

/// Indexing failure is a degraded search, never a lost firing.
fn index_quietly(ctx: &AppCtx, message: &Message) {
    let result = match ctx.db.lock() {
        Ok(db) => db.index_message(message),
        Err(_) => return,
    };
    if let Err(err) = result {
        tracing::error!("failed to index message {}: {err}", message.id);
    }
}

// ---------------------------------------------------------------------------
// One tick
// ---------------------------------------------------------------------------

/// One ticker pass at `now`. Returns the schedules that fired, in order.
///
/// Read the two statements in the loop together — they are the catch-up rule:
/// [`Db::mark_schedule_run`] stamps `last_run_at = now` and `next_run_at =
/// next_run(spec, now)` **before** `fire` runs, so the row is no longer due
/// whatever happens next, and the new time is measured from this instant
/// rather than from the slot that was missed.
///
/// `fire` is a parameter rather than a call to [`fire_schedule`] so a test can
/// drive the loop with a counter and prove the burst does not happen without a
/// database full of sessions or a single LLM call. A failing `fire` is
/// reported and swallowed — one schedule's failure must not skip the rest of
/// the pass ([`fire_schedule`] itself never fails; a test fake or a future
/// caller might).
pub fn tick_schedules(
    db: &dyn Db,
    now: i64,
    fire: &mut dyn FnMut(&Schedule) -> Result<(), BoughError>,
) -> Result<Vec<Schedule>, BoughError> {
    let due = db.due_schedules(now)?;
    for schedule in &due {
        db.mark_schedule_run(&schedule.id, now, next_run(&schedule.spec, now)?)?;
        if let Err(err) = fire(schedule) {
            tracing::error!(
                "schedule {} ({}) failed to fire: {err}",
                schedule.id,
                schedule.title
            );
        }
    }
    Ok(due)
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// The seams the ticker takes on top of [`FireDeps`].
#[derive(Clone, Default)]
pub struct TickerDeps {
    /// Defaults to [`TICK_MS`].
    pub interval_ms: Option<u64>,
    /// Defaults to [`fire_schedule_with`]. Tests observe firings here.
    pub fire: Option<Arc<dyn Fn(&AppCtx, &Schedule) + Send + Sync>>,
    pub fire_deps: FireDeps,
}

/// The production loop: [`tick_schedules`] on a ~30s interval, with production
/// defaults. Returns a stopper. Must be called inside a tokio runtime.
pub fn start_schedule_ticker(ctx: &AppCtx) -> impl FnOnce() {
    start_schedule_ticker_with(ctx, TickerDeps::default())
}

/// [`start_schedule_ticker`] with its seams exposed.
///
/// A spawned tokio task never keeps the runtime open the way a Node timer
/// held the process (unref is free here), but the stopper still matters:
/// `bough exec` and tests tear the ticker down deliberately. No immediate
/// pass at boot — the first tick lands one interval in, which gives a server
/// that is still recovering orphaned turns a moment before it starts opening
/// new sessions; a schedule due right now waits at most 30 seconds.
///
/// A throwing tick must not kill the interval — the next pass may well work,
/// and a silently dead ticker is a feature that stops existing with no signal.
pub fn start_schedule_ticker_with(ctx: &AppCtx, deps: TickerDeps) -> impl FnOnce() {
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
                // `biased` + the re-check below give the stopper the
                // `clearInterval` guarantee: once it returns, no further pass
                // runs. An unbiased select polls branches in random order and
                // can take a ready tick over a ready cancellation.
                biased;
                _ = token.cancelled() => break,
                _ = interval.tick() => {}
            }
            if token.is_cancelled() {
                break;
            }
            let now = match &deps.fire_deps.now {
                Some(clock) => clock(),
                None => (ctx.now)(),
            };
            // Collect under the lock, fire after it: `fire_schedule` reads the
            // db itself, and the mutex is not reentrant. The advance has
            // already happened for every collected row, so this ordering keeps
            // the advance-before-fire guarantee intact.
            let mut to_fire: Vec<Schedule> = Vec::new();
            let ticked = match ctx.db.lock() {
                Ok(db) => tick_schedules(&*db, now, &mut |s| {
                    to_fire.push(s.clone());
                    Ok(())
                }),
                Err(_) => Err(BoughError::http(
                    500,
                    ErrorKind::Schedule,
                    "schedules: the database lock is poisoned",
                )),
            };
            if let Err(err) = ticked {
                tracing::error!("schedule tick failed: {err}");
                continue;
            }
            for schedule in &to_fire {
                match &deps.fire {
                    Some(fire) => fire(&ctx, schedule),
                    None => {
                        fire_schedule_with(&ctx, schedule, &deps.fire_deps);
                    }
                }
            }
        }
    });
    move || stopper.cancel()
}

// ---------------------------------------------------------------------------
// Tests — the pure math, ported from src/hostfn/schedule.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// `Date.UTC(2026, 0, 15, 12, 0, 0)`.
    fn t0() -> i64 {
        Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
            .unwrap()
            .timestamp_millis()
    }

    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 3_600_000;

    fn local_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn parse_spec_accepts_every_n_m_h_d() {
        assert_eq!(
            parse_spec("every:30m"),
            Some(ParsedSpec::Every { ms: 30 * MINUTE })
        );
        assert_eq!(
            parse_spec("every:2h"),
            Some(ParsedSpec::Every { ms: 2 * HOUR })
        );
        assert_eq!(
            parse_spec("every:1d"),
            Some(ParsedSpec::Every { ms: 86_400_000 })
        );
    }

    #[test]
    fn parse_spec_accepts_daily_hh_mm() {
        assert_eq!(
            parse_spec("daily@09:00"),
            Some(ParsedSpec::Daily { hh: 9, mm: 0 })
        );
        assert_eq!(
            parse_spec("daily@9:05"),
            Some(ParsedSpec::Daily { hh: 9, mm: 5 })
        );
        assert_eq!(
            parse_spec("daily@23:59"),
            Some(ParsedSpec::Daily { hh: 23, mm: 59 })
        );
    }

    #[test]
    fn parse_spec_rejects_everything_else() {
        for bad in [
            "",
            "every:0m", // N ≥ 1 — a zero interval is always due, on every tick, forever
            "every:m",
            "every:5s",
            "every:5w",
            "every: 5m",
            "EVERY:5m",
            "daily@24:00",
            "daily@09:60",
            "daily@9",
            "daily@09:00:00",
            "0 9 * * *", // cron is NOT the grammar
            "hourly",
        ] {
            assert_eq!(parse_spec(bad), None, "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn next_run_for_every_adds_the_interval_to_the_instant_it_is_given() {
        assert_eq!(next_run("every:30m", t0()).unwrap(), t0() + 30 * MINUTE);
        // The invariant, stated as arithmetic: five hours of downtime does not
        // compound. Whatever `from` is, the answer is exactly one interval
        // later.
        assert_eq!(
            next_run("every:30m", t0() + 5 * HOUR).unwrap(),
            t0() + 5 * HOUR + 30 * MINUTE
        );
    }

    #[test]
    fn next_run_for_daily_lands_at_the_next_local_wall_clock_occurrence() {
        // Local time, so the assertion is built with the local constructor
        // rather than UTC.
        let morning = local_ms(2026, 1, 15, 8, 0);
        let nine = local_ms(2026, 1, 15, 9, 0);
        assert_eq!(next_run("daily@09:00", morning).unwrap(), nine);

        // Already past today → tomorrow, same wall clock.
        let afternoon = local_ms(2026, 1, 15, 14, 0);
        assert_eq!(
            next_run("daily@09:00", afternoon).unwrap(),
            local_ms(2026, 1, 16, 9, 0)
        );

        // Exactly at the slot is NOT "now again": strictly after, or the row
        // stays due.
        assert_eq!(
            next_run("daily@09:00", nine).unwrap(),
            local_ms(2026, 1, 16, 9, 0)
        );
    }

    #[test]
    fn next_run_errors_on_a_spec_that_does_not_parse() {
        let err = next_run("weekly", t0()).unwrap_err();
        assert_eq!(err.status(), 400);
        assert_eq!(err.name(), "ScheduleError");
        let message = err.to_string();
        assert!(
            message.contains("invalid schedule spec: weekly"),
            "message: {message}"
        );
        // The grammar is in the message — the model's next move is to write
        // another spec, and "invalid" alone gets a second guess, not a fix.
        assert!(message.contains("every:<N><m|h|d>"), "message: {message}");
    }

    // ---- DST, pinned against a fixed zone (chrono-tz) ----------------------
    //
    // US DST in 2026: spring forward Sun Mar 8 02:00→03:00, fall back Sun
    // Nov 1 02:00→01:00 (America/Los_Angeles: PST = UTC-8, PDT = UTC-7).

    use chrono_tz::America::Los_Angeles as LA;

    fn utc_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn daily_stays_at_the_wall_clock_across_the_spring_forward_transition() {
        // From Sat Mar 7 10:00 PST (18:00Z), the next 09:00 is Sun Mar 8
        // 09:00 PDT (16:00Z) — 23 real hours later, same wall clock.
        let from = utc_ms(2026, 3, 7, 18, 0);
        assert_eq!(
            next_run_in(&LA, ParsedSpec::Daily { hh: 9, mm: 0 }, from),
            utc_ms(2026, 3, 8, 16, 0)
        );
    }

    #[test]
    fn daily_stays_at_the_wall_clock_across_the_fall_back_transition() {
        // From Sat Oct 31 10:00 PDT (17:00Z), the next 09:00 is Sun Nov 1
        // 09:00 PST (17:00Z) — 25 real hours later, same wall clock.
        let from = utc_ms(2026, 10, 31, 17, 0);
        assert_eq!(
            next_run_in(&LA, ParsedSpec::Daily { hh: 9, mm: 0 }, from),
            utc_ms(2026, 11, 1, 17, 0)
        );
    }

    #[test]
    fn a_nonexistent_wall_time_resolves_forward_across_the_gap() {
        // 02:30 never happens on Mar 8 2026 in LA; the run lands on the
        // instant the pre-transition offset names — 03:30 PDT (10:30Z).
        let from = utc_ms(2026, 3, 8, 8, 0); // Mar 8 00:00 PST
        assert_eq!(
            next_run_in(&LA, ParsedSpec::Daily { hh: 2, mm: 30 }, from),
            utc_ms(2026, 3, 8, 10, 30)
        );
    }

    #[test]
    fn an_ambiguous_wall_time_resolves_to_the_earliest_occurrence() {
        // 01:30 happens twice on Nov 1 2026 in LA; the run is the FIRST one —
        // 01:30 PDT (08:30Z), not 01:30 PST (09:30Z).
        let from = utc_ms(2026, 11, 1, 7, 0); // Nov 1 00:00 PDT
        assert_eq!(
            next_run_in(&LA, ParsedSpec::Daily { hh: 1, mm: 30 }, from),
            utc_ms(2026, 11, 1, 8, 30)
        );
    }
}

// ---------------------------------------------------------------------------
// Tests — ticker + firing + report-back, ported from src/schedules.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod fire_tests {
    use super::*;
    use crate::bus::Bus;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{is_collapsed_kind, Turn, TurnStatus};
    use crate::types::{AppCtx, HostState, SharedDb, TurnStarter};
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::RwLock;

    /// `Date.UTC(2026, 0, 15, 12, 0, 0)`.
    fn t0() -> i64 {
        chrono::Utc
            .with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
            .unwrap()
            .timestamp_millis()
    }

    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 3_600_000;

    struct Fx {
        ctx: AppCtx,
        events: Arc<Mutex<Vec<BoughEvent>>>,
        /// What the recording turn starter was handed: `(sessionId, messageId)`.
        started: Arc<Mutex<Vec<(String, String)>>>,
    }

    struct RecordingStarter(Arc<Mutex<Vec<(String, String)>>>);
    impl TurnStarter for RecordingStarter {
        fn start_turn(&self, _ctx: &AppCtx, session: &Session, message: &Message) {
            self.0
                .lock()
                .unwrap()
                .push((session.id.clone(), message.id.clone()));
        }
    }

    struct PanickingStarter;
    impl TurnStarter for PanickingStarter {
        fn start_turn(&self, _ctx: &AppCtx, _session: &Session, _message: &Message) {
            panic!("no turn for you");
        }
    }

    fn fixture_bare() -> Fx {
        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let bus = Arc::new(Bus::new(Arc::new(t0)));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.clone())
        }));
        let started = Arc::new(Mutex::new(Vec::new()));
        let ctx = AppCtx {
            db,
            bus,
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now: Arc::new(t0),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: Arc::new(crate::turn::queue::TurnRegistry::new()),
            model_defaults_path: None,
        };
        Fx {
            ctx,
            events,
            started,
        }
    }

    fn fixture() -> Fx {
        let f = fixture_bare();
        let starter: Arc<dyn TurnStarter> = Arc::new(RecordingStarter(f.started.clone()));
        *f.ctx.starter.write().unwrap() = Some(starter);
        f
    }

    /// A schedule straight into the database — the CRUD has its own tests.
    fn seed(f: &Fx, spec: &str, next_run_at: i64, session_id: Option<&str>) -> Schedule {
        f.ctx
            .db
            .lock()
            .unwrap()
            .create_schedule(Schedule {
                id: uuid::Uuid::new_v4().to_string(),
                title: "deploy check".into(),
                prompt: "check the deploy and report".into(),
                workspace: None,
                session_id: session_id.map(String::from),
                spec: spec.into(),
                enabled: true,
                created_at: t0(),
                last_run_at: None,
                next_run_at,
            })
            .unwrap()
    }

    fn make_session(f: &Fx, id: &str) {
        f.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: id.into(),
                title: "main".into(),
                kind: SessionKind::Root,
                created_at: t0(),
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: None,
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
                description: None,
            })
            .unwrap();
    }

    /// Persist the rows a real run would leave in the fired session: a
    /// supervisor message carrying `report` and a settled turn row.
    ///
    /// The suites that only need the turn row use `play_run_raw`; this fuller
    /// shape is kept for the report-carrying firings.
    #[allow(dead_code)]
    fn play_run(f: &Fx, session_id: &str, status: TurnStatus, report: &str) -> String {
        let db = f.ctx.db.lock().unwrap();
        let sup = db
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.into(),
                role: Role::Supervisor,
                parts: if report.is_empty() {
                    vec![]
                } else {
                    vec![Part::Text {
                        text: report.into(),
                    }]
                },
                pending: false,
                created_at: t0() + 1,
            })
            .unwrap();
        db.create_turn(Turn {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            message_id: sup.id.clone(),
            status,
            step: "done".into(),
            created_at: t0(),
            updated_at: t0() + 1,
            error: None,
            usage: None,
        })
        .unwrap();
        sup.id
    }

    /// The `build_result` fake: turn status → firing status, report = the
    /// supervisor message's text parts (the row-2.2 adapter will do exactly
    /// this through `agents::subagent::build_result`).
    fn fake_build() -> BuildOutcome {
        Arc::new(|ctx: &AppCtx, _session_id: &str, message_id: &str| {
            let db = ctx.db.lock().unwrap();
            let status = match db.turn_for_message(message_id)?.map(|t| t.status) {
                Some(TurnStatus::Done) => FiringStatus::Done,
                Some(TurnStatus::Error) => FiringStatus::Error,
                Some(TurnStatus::Interrupted) => FiringStatus::Interrupted,
                _ => FiringStatus::Orphaned,
            };
            let report = db
                .get_message(message_id)?
                .map(|m| {
                    m.parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            Ok(FiringOutcome { status, report })
        })
    }

    /// The `post_system_note` fake: persist a system note, announce it, wake
    /// the (idle) session through the ctx starter — the row-2.3 semantics the
    /// tests need observable.
    fn fake_post() -> PostNote {
        Arc::new(|ctx: &AppCtx, session_id: &str, text: &str| {
            let message = {
                let db = ctx.db.lock().unwrap();
                if db.get_session(session_id).unwrap().is_none() {
                    return; // dropped — the correct outcome, not an error
                }
                db.create_message(Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    session_id: session_id.into(),
                    role: Role::System,
                    parts: vec![Part::Text { text: text.into() }],
                    pending: false,
                    created_at: t0() + 2,
                })
                .unwrap()
            };
            ctx.bus.publish(EventInput {
                r#type: EventType::MessageStarted,
                session_id: Some(session_id.into()),
                data: serde_json::to_value(&message).unwrap(),
            });
            if let Some(starter) = ctx.turn_starter() {
                let session = ctx
                    .db
                    .lock()
                    .unwrap()
                    .get_session(session_id)
                    .unwrap()
                    .unwrap();
                starter.start_turn(ctx, &session, &message);
            }
        })
    }

    fn collecting_report() -> (ReportError, Arc<Mutex<Vec<String>>>) {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        (
            Arc::new(move |err: &BoughError, _s: &Schedule| {
                sink.lock().unwrap().push(err.to_string())
            }),
            errors,
        )
    }

    fn count_fire(
        fired: Arc<Mutex<Vec<String>>>,
    ) -> impl FnMut(&Schedule) -> Result<(), BoughError> {
        move |s: &Schedule| {
            fired.lock().unwrap().push(s.id.clone());
            Ok(())
        }
    }

    // ---- catch-up — the invariant -------------------------------------------

    #[test]
    fn a_ticker_down_through_five_slots_fires_once_then_resumes_cadence() {
        let f = fixture();
        let schedule = seed(&f, "every:1h", t0() + HOUR, None);
        let fired = Arc::new(Mutex::new(Vec::new()));
        let mut fire = count_fire(fired.clone());
        let db = f.ctx.db.lock().unwrap();

        // Not due yet.
        assert!(tick_schedules(&*db, t0() + 30 * MINUTE, &mut fire)
            .unwrap()
            .is_empty());

        // The server was down from T0+1h to T0+6h — five slots came and went.
        let back = t0() + 6 * HOUR;
        assert_eq!(
            tick_schedules(&*db, back, &mut fire)
                .unwrap()
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>(),
            vec![schedule.id.clone()]
        );
        assert_eq!(
            fired.lock().unwrap().clone(),
            vec![schedule.id.clone()],
            "five missed slots must not become five runs"
        );

        // The row advanced FROM NOW, not from the stale value: had it advanced
        // from the stored T0+1h it would be due at T0+2h — already past — and
        // the next tick would fire again, and again, for five more ticks.
        let after = db.get_schedule(&schedule.id).unwrap().unwrap();
        assert_eq!(after.last_run_at, Some(back));
        assert_eq!(after.next_run_at, back + HOUR);

        // Every tick between now and the next slot is quiet.
        for at in [back + 1, back + MINUTE, back + 59 * MINUTE] {
            assert!(tick_schedules(&*db, at, &mut fire).unwrap().is_empty());
        }
        assert_eq!(fired.lock().unwrap().len(), 1);

        // Then the cadence resumes, exactly once.
        assert_eq!(
            tick_schedules(&*db, back + HOUR, &mut fire).unwrap().len(),
            1
        );
        assert_eq!(fired.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_daily_schedule_missed_for_a_week_also_fires_once() {
        let f = fixture();
        let at = Local
            .with_ymd_and_hms(2026, 1, 15, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let schedule = seed(&f, "daily@09:00", at, None);
        let fired = Arc::new(Mutex::new(Vec::new()));
        let mut fire = count_fire(fired.clone());
        let db = f.ctx.db.lock().unwrap();

        // A week later, at 09:30 local.
        let back = Local
            .with_ymd_and_hms(2026, 1, 22, 9, 30, 0)
            .unwrap()
            .timestamp_millis();
        tick_schedules(&*db, back, &mut fire).unwrap();
        assert_eq!(fired.lock().unwrap().clone(), vec![schedule.id.clone()]);
        // Next occurrence is tomorrow at 09:00 — today's 09:00 is already past.
        assert_eq!(
            db.get_schedule(&schedule.id).unwrap().unwrap().next_run_at,
            Local
                .with_ymd_and_hms(2026, 1, 23, 9, 0, 0)
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn the_advance_happens_before_the_fire_so_a_throwing_fire_cannot_hot_loop() {
        let f = fixture();
        let schedule = seed(&f, "every:30m", t0(), None);
        let attempts = Arc::new(Mutex::new(0));
        let counter = attempts.clone();
        let mut boom = move |_s: &Schedule| {
            *counter.lock().unwrap() += 1;
            Err(BoughError::http(500, ErrorKind::Schedule, "fire failed"))
        };
        let db = f.ctx.db.lock().unwrap();

        // The failure is swallowed — one bad schedule must not abort the pass
        // — and the row is advanced anyway, so the next tick 30 seconds later
        // does not fire it again.
        assert_eq!(tick_schedules(&*db, t0(), &mut boom).unwrap().len(), 1);
        assert_eq!(*attempts.lock().unwrap(), 1);
        assert_eq!(
            db.get_schedule(&schedule.id).unwrap().unwrap().next_run_at,
            t0() + 30 * MINUTE
        );
        assert!(tick_schedules(&*db, t0() + 30_000, &mut boom)
            .unwrap()
            .is_empty());
        assert_eq!(*attempts.lock().unwrap(), 1);
    }

    #[test]
    fn one_pass_fires_every_due_schedule_and_skips_the_disabled_ones() {
        let f = fixture();
        let a = seed(&f, "every:30m", t0(), None);
        let b = seed(&f, "every:30m", t0() - HOUR, None);
        let disabled = seed(&f, "every:30m", t0() - HOUR, None);
        {
            let db = f.ctx.db.lock().unwrap();
            let row = db.get_schedule(&disabled.id).unwrap().unwrap();
            db.update_schedule(&Schedule {
                enabled: false,
                ..row
            })
            .unwrap();
        }
        seed(&f, "every:30m", t0() + HOUR, None); // later

        let db = f.ctx.db.lock().unwrap();
        let fired = tick_schedules(&*db, t0(), &mut |_s| Ok(())).unwrap();
        let mut got: Vec<String> = fired.iter().map(|s| s.id.clone()).collect();
        let mut want = vec![a.id, b.id];
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }

    // ---- firing -------------------------------------------------------------

    #[tokio::test]
    async fn firing_opens_a_fresh_root_session_carrying_the_prompt_and_starts_a_turn() {
        let f = fixture();
        let mut schedule = seed(&f, "every:30m", t0() + 30 * MINUTE, None);
        schedule.workspace = Some("/work/repo".into());
        f.ctx.db.lock().unwrap().update_schedule(&schedule).unwrap();
        let schedule = f
            .ctx
            .db
            .lock()
            .unwrap()
            .get_schedule(&schedule.id)
            .unwrap()
            .unwrap();

        let fired = fire_schedule(&f.ctx, &schedule).expect("the firing succeeds");

        let db = f.ctx.db.lock().unwrap();
        let session = db.get_session(&fired.session.id).unwrap().unwrap();
        assert_eq!(session.kind, SessionKind::Root);
        assert_eq!(
            session.parent_id, None,
            "a fired session inherits no thread"
        );
        assert_eq!(session.title, schedule.title);
        assert_eq!(
            db.get_session_runtime(&session.id)
                .unwrap()
                .workspace
                .as_deref(),
            Some("/work/repo")
        );

        // The prompt is the session's first user message — the whole briefing,
        // since the session sees none of the creating conversation.
        let thread = db.thread_for(&session.id).unwrap();
        assert_eq!(thread.len(), 1);
        assert_eq!(thread[0].role, Role::User);
        assert_eq!(
            thread[0].parts,
            vec![Part::Text {
                text: schedule.prompt.clone()
            }]
        );
        assert!(!thread[0].pending);

        // And a turn was asked for, on that session, with that message.
        assert_eq!(
            f.started.lock().unwrap().clone(),
            vec![(session.id.clone(), thread[0].id.clone())]
        );

        // Announced in order: the session, then the message on it.
        let kinds: Vec<EventType> = f.events.lock().unwrap().iter().map(|e| e.r#type).collect();
        assert_eq!(
            kinds,
            vec![EventType::SessionCreated, EventType::MessageStarted]
        );
    }

    #[tokio::test]
    async fn a_firing_collapses_under_the_conversation_that_created_the_schedule() {
        let f = fixture();
        make_session(&f, "creator");
        let schedule = seed(&f, "every:30m", t0() + 30 * MINUTE, Some("creator"));

        let fired = fire_schedule(&f.ctx, &schedule).unwrap();
        let db = f.ctx.db.lock().unwrap();
        let session = db.get_session(&fired.session.id).unwrap().unwrap();

        // A run is not a conversation the user started, so it does not get a
        // row beside them: a daily schedule was adding one a day to the switcher.
        assert_eq!(session.kind, SessionKind::ScheduleRun);
        assert_eq!(
            session.origin_id.as_deref(),
            Some("creator"),
            "the lineage edge is what makes it reachable"
        );
        assert_eq!(
            db.list_sessions()
                .unwrap()
                .into_iter()
                .filter(|s| !is_collapsed_kind(s.kind))
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            vec!["creator".to_string()],
            "the top level still holds exactly the one conversation the user made"
        );
        assert_eq!(
            db.sessions_by_origin("creator")
                .unwrap()
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>(),
            vec![session.id.clone()]
        );

        // Collapsed, but NOT inherited: the prompt is still the whole briefing.
        assert_eq!(session.parent_id, None, "a firing inherits no thread");
        assert_eq!(
            session.origin_message_id, None,
            "the clock asked for it, not a turn"
        );
        let thread = db.thread_for(&session.id).unwrap();
        assert_eq!(thread.len(), 1);
        assert_eq!(
            thread[0].parts,
            vec![Part::Text {
                text: schedule.prompt.clone()
            }]
        );
    }

    #[tokio::test]
    async fn a_firing_with_nobody_to_collapse_under_stands_on_its_own() {
        let f = fixture();
        // A schedule created over REST carries no sessionId…
        let orphan = fire_schedule(&f.ctx, &seed(&f, "every:30m", t0(), None)).unwrap();
        assert_eq!(
            f.ctx
                .db
                .lock()
                .unwrap()
                .get_session(&orphan.session.id)
                .unwrap()
                .unwrap()
                .kind,
            SessionKind::Root
        );

        // …and a creator can be gone by the time the schedule fires.
        let gone = fire_schedule(
            &f.ctx,
            &seed(&f, "every:30m", t0(), Some("deleted-long-ago")),
        )
        .unwrap();
        let session = f
            .ctx
            .db
            .lock()
            .unwrap()
            .get_session(&gone.session.id)
            .unwrap()
            .unwrap();
        assert_eq!(session.kind, SessionKind::Root);
        assert_eq!(session.origin_id, None);
    }

    #[tokio::test]
    async fn firing_without_a_workspace_leaves_the_session_unpinned() {
        let f = fixture();
        let fired = fire_schedule(&f.ctx, &seed(&f, "every:30m", t0(), None)).unwrap();
        assert_eq!(
            f.ctx
                .db
                .lock()
                .unwrap()
                .get_session_runtime(&fired.session.id)
                .unwrap()
                .workspace,
            None
        );
    }

    #[test]
    fn firing_never_throws_when_the_turn_starter_fails() {
        let f = fixture();
        let (report, errors) = collecting_report();
        let deps = FireDeps {
            report_error: Some(report),
            start: Some(Arc::new(|_ctx: &AppCtx, _s: &Session, _m: &Message| {
                StartOutcome::Failed(BoughError::http(
                    500,
                    ErrorKind::Schedule,
                    "no turn for you",
                ))
            })),
            ..Default::default()
        };
        let fired = fire_schedule_with(&f.ctx, &seed(&f, "every:30m", t0(), None), &deps);
        // The session and its message survive — the user can see what was
        // meant to run.
        assert!(fired.is_none());
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert_eq!(f.ctx.db.lock().unwrap().list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn firing_never_panics_even_when_the_starter_panics() {
        let f = fixture();
        *f.ctx.starter.write().unwrap() = Some(Arc::new(PanickingStarter));
        let (report, errors) = collecting_report();
        let deps = FireDeps {
            report_error: Some(report),
            ..Default::default()
        };
        let fired = fire_schedule_with(&f.ctx, &seed(&f, "every:30m", t0(), None), &deps);
        assert!(fired.is_none());
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert!(errors.lock().unwrap()[0].contains("no turn for you"));
        // The rows written before the panic survive.
        assert_eq!(f.ctx.db.lock().unwrap().list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn firing_with_no_turn_starter_wired_still_records_the_session() {
        let f = fixture_bare();
        let fired = fire_schedule(&f.ctx, &seed(&f, "every:30m", t0(), None)).unwrap();
        assert_eq!(
            f.ctx
                .db
                .lock()
                .unwrap()
                .thread_for(&fired.session.id)
                .unwrap()
                .len(),
            1
        );
    }

    // ---- reporting back -----------------------------------------------------

    #[test]
    fn the_note_text_is_verbatim() {
        assert_eq!(
            firing_note_text("deploy check", "s-9", FiringStatus::Done, "Bench passed."),
            "[schedule fired] \"deploy check\" finished (session s-9).\n\
             Report:\nBench passed.\n\
             Act on it only if it needs something — the run is its own session, and this note \
             is its outcome."
        );
        assert_eq!(
            firing_note_text("deploy check", "s-9", FiringStatus::Orphaned, ""),
            "[schedule fired] \"deploy check\" ended without a completed turn (session s-9).\n\
             No report.\n\
             Act on it only if it needs something — the run is its own session, and this note \
             is its outcome."
        );
        assert!(firing_note_text("t", "s", FiringStatus::Error, "boom")
            .contains("FAILED — its turn errored, and the report below carries the error"));
        assert!(firing_note_text("t", "s", FiringStatus::Interrupted, "")
            .contains("was STOPPED before it finished"));
    }

    #[test]
    fn a_firings_outcome_is_posted_back_to_the_creating_conversation_and_wakes_it() {
        let f = fixture();
        make_session(&f, "creator");
        let schedule = seed(&f, "every:30m", t0() + 30 * MINUTE, Some("creator"));

        // The starter plays the runner for the fired session: it persists the
        // turn row and the supervisor message a real run would leave, so the
        // note is assembled from what the database actually holds.
        let fx_db = f.ctx.db.clone();
        let started = f.started.clone();
        let deps = FireDeps {
            start: Some(Arc::new(
                move |_ctx: &AppCtx, session: &Session, message: &Message| {
                    started
                        .lock()
                        .unwrap()
                        .push((session.id.clone(), message.id.clone()));
                    let db = fx_db.lock().unwrap();
                    let sup = db
                        .create_message(Message {
                            id: uuid::Uuid::new_v4().to_string(),
                            session_id: session.id.clone(),
                            role: Role::Supervisor,
                            parts: vec![Part::Text {
                                text: "Bench passed: 14/16 solved.".into(),
                            }],
                            pending: false,
                            created_at: t0() + 1,
                        })
                        .unwrap();
                    db.create_turn(Turn {
                        id: uuid::Uuid::new_v4().to_string(),
                        session_id: session.id.clone(),
                        message_id: sup.id,
                        status: TurnStatus::Done,
                        step: "done".into(),
                        created_at: t0(),
                        updated_at: t0() + 1,
                        error: None,
                        usage: None,
                    })
                    .unwrap();
                    StartOutcome::Done
                },
            )),
            build_outcome: Some(fake_build()),
            post_note: Some(fake_post()),
            ..Default::default()
        };

        let fired = fire_schedule_with(&f.ctx, &schedule, &deps).expect("fires");

        // The note landed in the CREATOR, prefixed and carrying the run's
        // final text.
        let notes = f.ctx.db.lock().unwrap().thread_for("creator").unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].role, Role::System);
        let Part::Text { text } = &notes[0].parts[0] else {
            panic!("expected a text part")
        };
        assert!(
            text.starts_with(&format!(
                "[schedule fired] \"deploy check\" finished (session {})",
                fired.session.id
            )),
            "{text}"
        );
        assert!(text.contains("Bench passed: 14/16 solved."), "{text}");

        // …and woke it: the creator was idle, so the note started a turn there.
        assert_eq!(
            f.started
                .lock()
                .unwrap()
                .iter()
                .map(|(s, _)| s.clone())
                .collect::<Vec<_>>(),
            vec![fired.session.id.clone(), "creator".to_string()]
        );
    }

    #[test]
    fn the_default_report_back_uses_the_real_result_builder_and_note_post() {
        // No fakes: build_outcome/post_note absent = the real
        // agents::subagent::build_result + agents::notes::post_system_note.
        let f = fixture();
        make_session(&f, "creator");
        let schedule = seed(&f, "every:30m", t0() + 30 * MINUTE, Some("creator"));
        let fx_db = f.ctx.db.clone();
        let started = f.started.clone();
        let deps = FireDeps {
            start: Some(Arc::new(
                move |_ctx: &AppCtx, session: &Session, message: &Message| {
                    started
                        .lock()
                        .unwrap()
                        .push((session.id.clone(), message.id.clone()));
                    let db = fx_db.lock().unwrap();
                    let sup = db
                        .create_message(Message {
                            id: uuid::Uuid::new_v4().to_string(),
                            session_id: session.id.clone(),
                            role: Role::Supervisor,
                            parts: vec![Part::Text {
                                text: "All checks green.".into(),
                            }],
                            pending: false,
                            created_at: t0() + 1,
                        })
                        .unwrap();
                    db.create_turn(Turn {
                        id: uuid::Uuid::new_v4().to_string(),
                        session_id: session.id.clone(),
                        message_id: sup.id,
                        status: TurnStatus::Done,
                        step: "done".into(),
                        created_at: t0(),
                        updated_at: t0() + 1,
                        error: None,
                        usage: None,
                    })
                    .unwrap();
                    StartOutcome::Done
                },
            )),
            ..Default::default()
        };

        let fired = fire_schedule_with(&f.ctx, &schedule, &deps).expect("fires");

        let notes = f.ctx.db.lock().unwrap().thread_for("creator").unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].role, Role::System);
        let Part::Text { text } = &notes[0].parts[0] else {
            panic!("expected a text part")
        };
        assert_eq!(
            *text,
            firing_note_text(
                "deploy check",
                &fired.session.id,
                FiringStatus::Done,
                "All checks green."
            )
        );
        // The real post applies the one wake rule: the idle creator was woken.
        assert_eq!(
            f.started
                .lock()
                .unwrap()
                .iter()
                .map(|(s, _)| s.clone())
                .collect::<Vec<_>>(),
            vec![fired.session.id.clone(), "creator".to_string()]
        );
    }

    #[tokio::test]
    async fn a_failed_run_reports_as_failed_the_firing_the_creator_most_needs_to_hear() {
        let f = fixture();
        make_session(&f, "creator");
        let schedule = seed(&f, "every:30m", t0() + 30 * MINUTE, Some("creator"));
        let (report, errors) = collecting_report();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let done_tx = Arc::new(Mutex::new(Some(done_tx)));

        let fx_db = f.ctx.db.clone();
        let posted_signal = done_tx.clone();
        let post = fake_post();
        let deps =
            FireDeps {
                report_error: Some(report),
                start: Some(Arc::new(
                    move |_ctx: &AppCtx, session: &Session, _m: &Message| {
                        play_run_raw(&fx_db, &session.id, TurnStatus::Error);
                        // The starter's own settling REJECTS — the run errored.
                        StartOutcome::Settles(Box::pin(futures::future::ready(Err(
                            BoughError::http(500, ErrorKind::Schedule, "the run errored"),
                        ))))
                    },
                )),
                build_outcome: Some(fake_build()),
                post_note: Some(Arc::new(move |ctx: &AppCtx, sid: &str, text: &str| {
                    post(ctx, sid, text);
                    if let Some(tx) = posted_signal.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                })),
                ..Default::default()
            };

        fire_schedule_with(&f.ctx, &schedule, &deps).expect("fires");
        tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .expect("the note posts")
            .unwrap();

        let notes = f.ctx.db.lock().unwrap().thread_for("creator").unwrap();
        let note = notes
            .first()
            .expect("the failed firing still owes the creator its note");
        let Part::Text { text } = &note.parts[0] else {
            panic!("expected a text part")
        };
        assert!(text.contains("FAILED"), "{text}");
        // The starter's rejection is reported too — the note does not swallow it.
        assert_eq!(errors.lock().unwrap().len(), 1);
    }

    /// `play_run` without the fixture, for closures that only hold the db.
    fn play_run_raw(db: &SharedDb, session_id: &str, status: TurnStatus) {
        let db = db.lock().unwrap();
        let sup = db
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.into(),
                role: Role::Supervisor,
                parts: vec![],
                pending: false,
                created_at: t0() + 1,
            })
            .unwrap();
        db.create_turn(Turn {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            message_id: sup.id,
            status,
            step: "x".into(),
            created_at: t0(),
            updated_at: t0() + 1,
            error: None,
            usage: None,
        })
        .unwrap();
    }

    #[test]
    fn a_firing_whose_schedule_has_no_creating_conversation_notes_nobody() {
        let f = fixture_bare();
        let deps = FireDeps {
            build_outcome: Some(fake_build()),
            post_note: Some(fake_post()),
            ..Default::default()
        };
        let fired = fire_schedule_with(&f.ctx, &seed(&f, "every:30m", t0(), None), &deps);
        assert!(fired.is_some());
        // Only the fired session's own prompt message was announced — no
        // system note.
        let roles: Vec<String> = f
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.r#type == EventType::MessageStarted)
            .map(|e| e.data["role"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(roles, vec!["user".to_string()]);
    }

    #[tokio::test]
    async fn the_default_settle_waits_for_the_fired_turns_finish_on_the_bus() {
        let f = fixture(); // recording starter — fire-and-forget, like production
        make_session(&f, "creator");
        let schedule = seed(&f, "every:30m", t0() + 30 * MINUTE, Some("creator"));
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let done_tx = Arc::new(Mutex::new(Some(done_tx)));
        let posted_signal = done_tx.clone();
        let post = fake_post();
        let deps = FireDeps {
            build_outcome: Some(fake_build()),
            post_note: Some(Arc::new(move |ctx: &AppCtx, sid: &str, text: &str| {
                post(ctx, sid, text);
                if let Some(tx) = posted_signal.lock().unwrap().take() {
                    let _ = tx.send(());
                }
            })),
            ..Default::default()
        };

        let fired = fire_schedule_with(&f.ctx, &schedule, &deps).expect("fires");
        // No note yet: the run has not settled.
        assert_eq!(
            f.ctx
                .db
                .lock()
                .unwrap()
                .thread_for("creator")
                .unwrap()
                .len(),
            0
        );

        // The run ends: rows land, then the runner announces turn.finished.
        play_run_raw(&f.ctx.db, &fired.session.id, TurnStatus::Done);
        f.ctx.bus.publish(EventInput {
            r#type: EventType::TurnFinished,
            session_id: Some(fired.session.id.clone()),
            data: serde_json::json!({}),
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .expect("the note posts after turn.finished")
            .unwrap();
        let notes = f.ctx.db.lock().unwrap().thread_for("creator").unwrap();
        assert_eq!(notes.len(), 1);
        // The settle unsubscribed its bus listener; only the fixture's
        // collector remains.
        assert_eq!(f.ctx.bus.size(), 1);
    }

    // ---- the loop -----------------------------------------------------------

    #[tokio::test]
    async fn the_real_ticker_fires_once_however_many_times_it_ticks_past_a_missed_slot() {
        let f = fixture();
        seed(&f, "every:1h", t0() + HOUR, None);

        // The clock jumps six hours ahead of the schedule's next slot and
        // STAYS there, so every tick in the window sees a row that was due
        // five slots ago. A loop that caught up slot by slot — or one that
        // failed to advance the row — would fire on each of them.
        let clock = Arc::new(AtomicI64::new(t0() + 6 * HOUR));
        let now: Clock = {
            let c = clock.clone();
            Arc::new(move || c.load(Ordering::SeqCst))
        };
        let fired = Arc::new(Mutex::new(Vec::<String>::new()));
        let deps = |fired: Arc<Mutex<Vec<String>>>, now: Clock| TickerDeps {
            interval_ms: Some(2),
            fire: Some(Arc::new(move |_ctx: &AppCtx, s: &Schedule| {
                fired.lock().unwrap().push(s.id.clone());
            })),
            fire_deps: FireDeps {
                now: Some(now),
                ..Default::default()
            },
        };

        let stop = start_schedule_ticker_with(&f.ctx, deps(fired.clone(), now.clone()));
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        stop();
        assert_eq!(
            fired.lock().unwrap().len(),
            1,
            "expected exactly one firing"
        );

        // Advance past the next slot and it fires again — the ticker is
        // alive, not stuck.
        clock.fetch_add(HOUR, Ordering::SeqCst);
        let stop2 = start_schedule_ticker_with(&f.ctx, deps(fired.clone(), now));
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        stop2();
        assert_eq!(fired.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_tickers_stopper_ends_it() {
        let f = fixture();
        seed(&f, "every:30m", t0() - HOUR, None);
        let clock = Arc::new(AtomicI64::new(t0()));
        let now: Clock = {
            let c = clock.clone();
            Arc::new(move || c.load(Ordering::SeqCst))
        };
        let fired = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = fired.clone();
        let stop = start_schedule_ticker_with(
            &f.ctx,
            TickerDeps {
                interval_ms: Some(2),
                fire: Some(Arc::new(move |_ctx: &AppCtx, s: &Schedule| {
                    sink.lock().unwrap().push(s.id.clone());
                })),
                fire_deps: FireDeps {
                    now: Some(now),
                    ..Default::default()
                },
            },
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        stop();
        let after = fired.lock().unwrap().len();
        // Make the schedule due again; a stopped ticker must not notice.
        clock.fetch_add(10 * HOUR, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(fired.lock().unwrap().len(), after);
        assert_eq!(after, 1);
    }
}
