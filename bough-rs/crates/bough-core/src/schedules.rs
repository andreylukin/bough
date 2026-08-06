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
//! v1 STUB (root.md §8): [`parse_spec`]/[`next_run`] ship here with wave 1;
//! the ticker, firing, report-back and REST land with wave 2 row 2.8 — until
//! then [`fire_schedule`] is wired to `None` and the ticker never starts.

use std::sync::LazyLock;

use chrono::{DateTime, Duration, Local, LocalResult, NaiveDateTime, TimeZone};
use regex::Regex;

use crate::errors::{BoughError, ErrorKind};
use crate::hostfn::schedule::{ParsedSpec, SPEC_HELP};
use crate::schema::parts::{Message, Schedule, Session};
use crate::types::{AppCtx, Db};

pub const TICK_MS: u64 = 30_000;

/// Stable marker text the creator's model and UI key off.
pub const SCHEDULE_NOTE_PREFIX: &str = "[schedule fired]";

// ---------------------------------------------------------------------------
// The grammar (pure)
// ---------------------------------------------------------------------------

static EVERY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^every:(\d+)(m|h|d)$").unwrap());
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
        return Some(ParsedSpec::Every { ms: n.checked_mul(unit_ms)? });
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
                resolve_local(tz, d.and_hms_opt(hh as u32, mm as u32, 0)
                    .expect("parse_spec bounds hh/mm"))
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
// Ticker + fire — v1 STUBS (wave 2, row 2.8)
// ---------------------------------------------------------------------------

/// What one firing produced: the fresh session and its prompt message.
#[derive(Clone, Debug)]
pub struct FiredSchedule {
    pub session: Session,
    pub message: Message,
}

/// Fire one schedule: fresh session + prompt message + turn. **Never
/// panics** (timer-callback context); `None` = the firing failed after being
/// reported.
///
/// v1 STUB: firing is a wave-2 behavior (row 2.8); until then no schedule
/// fires and this honestly reports "nothing fired".
pub fn fire_schedule(_ctx: &AppCtx, _schedule: &Schedule) -> Option<FiredSchedule> {
    None
}

/// One ticker pass over the due rows: advance each row FROM `now` first, then
/// hand it to `fire` — the advance-before-fire order is what makes a throwing
/// fire safe. Returns the due schedules in order; `fire` is a parameter for
/// testability.
///
/// v1 STUB: the ticker never runs in v1, so no row is ever due through this
/// path; the pass reports an empty tick.
pub fn tick_schedules(
    _db: &dyn Db,
    _now: i64,
    _fire: &mut dyn FnMut(&Schedule),
) -> Vec<Schedule> {
    Vec::new()
}

/// Start the interval loop; returns a stopper. No immediate pass at boot
/// (first tick lands one interval in — gives orphan-recovery a moment).
///
/// v1 STUB: no timer is started; the stopper is a no-op.
pub fn start_schedule_ticker(_ctx: &AppCtx) -> impl FnOnce() {
    || {}
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
        Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap().timestamp_millis()
    }

    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 3_600_000;

    fn local_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap().timestamp_millis()
    }

    #[test]
    fn parse_spec_accepts_every_n_m_h_d() {
        assert_eq!(parse_spec("every:30m"), Some(ParsedSpec::Every { ms: 30 * MINUTE }));
        assert_eq!(parse_spec("every:2h"), Some(ParsedSpec::Every { ms: 2 * HOUR }));
        assert_eq!(parse_spec("every:1d"), Some(ParsedSpec::Every { ms: 86_400_000 }));
    }

    #[test]
    fn parse_spec_accepts_daily_hh_mm() {
        assert_eq!(parse_spec("daily@09:00"), Some(ParsedSpec::Daily { hh: 9, mm: 0 }));
        assert_eq!(parse_spec("daily@9:05"), Some(ParsedSpec::Daily { hh: 9, mm: 5 }));
        assert_eq!(parse_spec("daily@23:59"), Some(ParsedSpec::Daily { hh: 23, mm: 59 }));
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
        assert_eq!(next_run("daily@09:00", afternoon).unwrap(), local_ms(2026, 1, 16, 9, 0));

        // Exactly at the slot is NOT "now again": strictly after, or the row
        // stays due.
        assert_eq!(next_run("daily@09:00", nine).unwrap(), local_ms(2026, 1, 16, 9, 0));
    }

    #[test]
    fn next_run_errors_on_a_spec_that_does_not_parse() {
        let err = next_run("weekly", t0()).unwrap_err();
        assert_eq!(err.status(), 400);
        assert_eq!(err.name(), "ScheduleError");
        let message = err.to_string();
        assert!(message.contains("invalid schedule spec: weekly"), "message: {message}");
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
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp_millis()
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
