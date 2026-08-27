//! §0.2 runtime invariant for `bough-plugin-tui-timeline`:
//!
//! **The rendered row set is a subset of the queried step set, and is strictly non-decreasing in
//! `(at, traj, seq)`.** A timeline that invented a row, or reordered two, is reported: those are
//! exactly the two ways a chronology can lie, and neither is visible to the type system.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::StepId;

/// The invariant's name, as a violation reports it.
pub const NAME: &str = "every_rendered_row_was_queried_and_the_order_never_goes_back";

/// What the last frame put on screen, and what the query that fed it returned. Called from
/// `Pane::render`; allocation-only, no I/O.
///
/// WP-2.
pub fn record(rendered: &[crate::Row], queried: &[StepId]) {
    let _ = (rendered, queried);
    todo!("WP-2: record the rendered rows and the queried step ids")
}

/// Forget the recorded frame. The row's disposal path.
///
/// WP-2.
pub fn forget() {
    todo!("WP-2: clear the recorded frame")
}

/// PURE: the check. Both halves — subset, and non-decreasing order.
///
/// WP-2.
pub fn check_rows(rendered: &[crate::Row], queried: &[StepId]) -> Result<(), String> {
    let _ = (rendered, queried);
    todo!("WP-2: subset of `queried`, and non-decreasing in (at, traj, seq)")
}

/// The specs this crate contributes.
///
/// WP-2: return the spec once [`check_rows`] and the recorder land.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}

#[allow(dead_code)]
async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let _ = (ctx, Cadence::OnQuiesce, NAME);
    todo!("WP-2: read the recorded frame and call check_rows")
}
