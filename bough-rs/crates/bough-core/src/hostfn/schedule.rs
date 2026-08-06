//! Schedule spec grammar + validated CRUD (port of `src/hostfn/schedule.ts`),
//! shared by REST and the `schedule.*` host fn — one validated path.
//!
//! "`next_run_at` is always computed FROM NOW, never from the stale stored
//! value." `sessionId` is stamped from ctx, NEVER taken from the wire.
//!
//! STUB (wave 2, row 2.8): the porter adds `parse_spec`/`next_run` (grammar
//! `every:<N><m|h|d>` N ≥ 1, or `daily@HH:MM` local wall-clock, DST-absorbing
//! calendar math via chrono) and the CRUD verbs.

/// The exact string error messages embed; a REST test asserts
/// `every:<N><m|h|d>` appears in the 400 body.
pub const SPEC_HELP: &str = "every:<N><m|h|d> with N \u{2265} 1 (every:30m, every:2h, every:1d) or daily@HH:MM in local wall-clock time (daily@09:00)";

/// A parsed schedule spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParsedSpec {
    Every { ms: i64 },
    Daily { hh: u8, mm: u8 },
}
