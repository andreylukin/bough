//! §0.2 runtime invariant: **`a_reset_rebuilds_and_never_reseals`** — for every `drift/reset`
//! observed, the `about/line` it names has an EMPTY intent half, and the count of `rollup/sealed`
//! observations of kind `tier` is unchanged across the reset (§8).
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::StepId;
use parking_lot::Mutex;

/// One reset, as observed.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub reset_step: StepId,
    pub about_line: StepId,
    /// The intent half of the about-line the reset appended. Must be empty.
    pub intent: String,
    /// Sealed `tier` rollups on the trajectory before and after. Must be equal.
    pub tiers_before: usize,
    pub tiers_after: usize,
}

/// What the row recorded this session, in reset order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one reset.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Everything recorded this session.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Forget the record. Tests only.
pub fn reset() {
    SEEN.lock().clear();
}

/// PURE: judge observed resets. Written as a function of data so a planted violation is a unit
/// test rather than a live run.
pub fn evaluate(_obs: &[Obs]) -> Result<(), String> {
    todo!("WP-4: reset evaluation")
}

/// §8: a reset rebuilds and never reseals.
pub fn a_reset_rebuilds_and_never_reseals() -> InvariantSpec {
    InvariantSpec {
        name: "a_reset_rebuilds_and_never_reseals",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-4: reset check")
}
