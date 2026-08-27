//! Invariant: a rendered line is CLIPPED to `cols` and never wraps. A timeline whose rows wrap
//! stops being one row per step, and the click map stops naming the step under the cursor.

use bough_plugin_tui_shell::HitId;

use crate::Row;

/// The `HitId` prefix a timeline row is clickable under.
pub const HIT_PREFIX: &str = "tl:";

/// PURE: one rendered line, clipped to `cols`.
///
/// ```text
/// 12:04:31  sol   tool/call     bash(cargo test -p bough)      pr/1204
/// ```
///
/// WP-2.
pub fn line(row: &Row, cols: u16, time_format: &str) -> String {
    let _ = (row, cols, time_format);
    todo!("WP-2: time | agent | kind | summary | refs, clipped to cols")
}

/// PURE: the hit id a row records — `tl:<step id>`.
///
/// WP-2.
pub fn hit_of(row: &Row) -> HitId {
    let _ = row;
    todo!("WP-2: the HIT_PREFIX plus the row's step id")
}

/// The step a `HitId` names, when it is one of ours.
///
/// WP-2.
pub fn step_of_hit(hit: &HitId) -> Option<bough_plugin_ledger::StepId> {
    let _ = hit;
    todo!("WP-2: strip HIT_PREFIX")
}
