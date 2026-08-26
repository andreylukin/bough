//! §0.2 runtime invariants for the ledger. Both providers return these specs from
//! `Plugin::invariants()`, so they run against whichever provider is mounted.
//!
//! 1. **`append_only_rows_never_change`** — across one session, no `steps`/`edges`/`rollups` row
//!    hash changes and no row id disappears.
//! 2. **`seal_once`** — a rollup's `superseded_by` transitions at most once, NULL → value, never
//!    back.
//! 3. **`seq_strictly_grows_per_trajectory`** — over the observed `ledger/step` stream, within a
//!    trajectory each step's seq is exactly its predecessor's + 1.
//! 4. **`wake_step_enclosure`** — every `step/start`..`step/end` pair lies inside a
//!    `wake/start`..`wake/end` pair of the same wake, and every step carries a wake id.
//!
//! Each check is a pure function over an observation record (`evaluate(&[Obs]) -> Result<(),
//! String>`) plus a store read, exactly as `hello`'s is. The record is cleared per fiber LIFE by
//! an inverse the provider's `apply` registers, because a RELOAD keeps the `FiberUid`.
//!
//! Every cadence is [`Cadence::OnQuiesce`] (P1-D14): Phase 0 left `Interval`/`OnEvent`
//! undispatched and Phase 1 takes no kernel change.

use std::collections::BTreeMap;

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

use crate::id::{RollupId, Seq, StepType, TrajId, WakeId};
use crate::query::RowHash;

/// One observation the `ledger/step` listener recorded. The invariants are statements about THIS
/// stream, so a check reads exactly what was observed and nothing else.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub traj: TrajId,
    pub seq: Seq,
    pub wake: WakeId,
    pub kind: StepType,
}

/// What the listener has seen, in arrival order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Row hashes as first observed, per session. Populated at each quiesce and compared against.
static HASHES: Mutex<Option<BTreeMap<(&'static str, String), String>>> = Mutex::new(None);

/// Every `superseded_by` transition observed, in order.
static SUPERSESSIONS: Mutex<Vec<(RollupId, RollupId)>> = Mutex::new(Vec::new());

/// Record one observation. Called from the listener the provider's `apply` registers.
pub fn record(obs: Obs) {
    todo!("WP-1: invariant::record")
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    todo!("WP-1: invariant::seen")
}

/// Drop the recorded stream. Test setup only.
pub fn clear() {
    todo!("WP-1: invariant::clear")
}

/// Forget everything recorded for `fiber`, so a reload starts a fresh stream.
pub fn forget(fiber: FiberUid) {
    todo!("WP-1: invariant::forget")
}

/// The four specs a provider's `Plugin::invariants()` returns. `plugin` is the provider's catalog
/// name, so a violation report names the row the reader will actually go looking at.
pub fn specs(plugin: &'static str) -> Vec<InvariantSpec> {
    todo!("WP-1: invariant::specs")
}

/// **`seq_strictly_grows_per_trajectory`**, as a pure function of the observed stream.
pub fn evaluate_seq(stream: &[Obs]) -> Result<(), String> {
    todo!("WP-1: invariant::evaluate_seq")
}

/// **`wake_step_enclosure`**, as a pure function of the observed stream.
pub fn evaluate_enclosure(stream: &[Obs]) -> Result<(), String> {
    todo!("WP-1: invariant::evaluate_enclosure")
}

/// **`append_only_rows_never_change`**, as a pure function of two row-hash snapshots. A row that
/// changed its hash, and a row id that disappeared, are both violations; a rollup whose
/// `superseded_by` moved is NOT, because the hash excludes that column.
pub fn evaluate_row_hashes(first: &[RowHash], now: &[RowHash]) -> Result<(), String> {
    todo!("WP-1: invariant::evaluate_row_hashes")
}

/// **`seal_once`**, as a pure function of the observed supersessions plus the current rows.
pub fn evaluate_seal_once(
    rows: &[RowHash],
    observed: &[(RollupId, RollupId)],
) -> Result<(), String> {
    todo!("WP-1: invariant::evaluate_seal_once")
}

/// Shared shape of the four checks: run the pure evaluation, wrap a failure as a violation.
fn violation(
    ctx: &Context,
    invariant: &'static str,
    plugin: &'static str,
    detail: String,
) -> InvariantViolation {
    InvariantViolation {
        invariant,
        plugin,
        entry: ctx.entry_id().clone(),
        detail,
    }
}

/// Suppresses the unused-import warning while the bodies are `todo!()`; the real checks use it.
#[allow(dead_code)]
fn _cadence_is_on_quiesce() -> Cadence {
    Cadence::OnQuiesce
}
