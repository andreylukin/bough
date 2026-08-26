//! §0.2 runtime invariant: **`a_pass_adds_and_never_edits`** — over the observed stream, every
//! step a pass appends is of a kind in `{claim/proposed, memory/expired, rollup/sealed,
//! about/line}`; and at quiesce, no `steps`/`edges` row hash observed before the first pass has
//! changed (§8: "never edits sealed rows or raw steps").
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{RowHash, StepId, StepType};
use parking_lot::Mutex;

/// One pass, as observed.
#[derive(Clone, Debug)]
pub struct Obs {
    pub pass: crate::ReconPassId,
    /// Every step the pass appended, in append order.
    pub appended: Vec<(StepId, StepType)>,
    /// Row hashes read BEFORE the pass ran; the check re-reads them at quiesce.
    pub before: Vec<RowHash>,
}

/// What the row recorded this session, in pass order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one pass.
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

/// PURE: judge observed passes against the adds-and-never-edits statement, given the row hashes
/// as they stand now. Written as a function of data so a planted edit is a unit test.
pub fn evaluate(_obs: &[Obs], _now: &[RowHash]) -> Result<(), String> {
    todo!("WP-3: adds-and-never-edits evaluation")
}

/// §8: a pass adds and never edits.
pub fn a_pass_adds_and_never_edits() -> InvariantSpec {
    InvariantSpec {
        name: "a_pass_adds_and_never_edits",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-3: adds-and-never-edits check")
}
