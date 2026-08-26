//! §0.2 runtime invariant for the projection seam:
//!
//! **`model_visible_is_ledgered`** — every [`SectionCites`] entry of every projection assembled
//! this session names a step or rollup id that EXISTS in the ledger.
//!
//! §3 lists model-visible ⟺ ledgered among the LEDGER invariants; it is implemented here because
//! the ledger Definition cannot see a projection section without depending on `projection`, which
//! would invert the seam (P1-D22, §0.2: consumers depend on Definitions, never the reverse). The
//! rule is §3's, unchanged; only its home moves. The check reads the ledger through the injected
//! handle, so it holds wherever the provider is mounted.
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{RollupId, StepId};
use parking_lot::Mutex;

use crate::section::{SectionCites, SectionId};

/// One assembled section's citation record, as observed.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub section: SectionId,
    pub cites: SectionCites,
}

/// What the assembler recorded this session, in assembly order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one assembled section. Called by the assembler at the end of `assemble`.
pub fn record(obs: Obs) {
    todo!("WP-4: projection invariant::record")
}

/// Everything recorded so far.
pub fn seen() -> Vec<Obs> {
    todo!("WP-4: projection invariant::seen")
}

/// Drop the record. Test setup only.
pub fn clear() {
    todo!("WP-4: projection invariant::clear")
}

/// Forget everything recorded for `fiber`; a RELOAD keeps the `FiberUid`.
pub fn forget(fiber: FiberUid) {
    todo!("WP-4: projection invariant::forget")
}

/// The spec the assembler's `Plugin::invariants()` returns.
pub fn model_visible_is_ledgered(plugin: &'static str) -> InvariantSpec {
    todo!("WP-4: projection invariant::model_visible_is_ledgered")
}

/// The rule as a pure function: every cited id must appear in the ids the ledger holds.
pub fn evaluate(
    stream: &[Obs],
    known_steps: &[StepId],
    known_rollups: &[RollupId],
) -> Result<(), String> {
    todo!("WP-4: projection invariant::evaluate")
}

#[allow(dead_code)]
fn _cadence_is_on_quiesce() -> Cadence {
    Cadence::OnQuiesce
}

#[allow(dead_code)]
fn _violation(ctx: &Context, plugin: &'static str, detail: String) -> InvariantViolation {
    InvariantViolation {
        invariant: "model_visible_is_ledgered",
        plugin,
        entry: ctx.entry_id().clone(),
        detail,
    }
}
