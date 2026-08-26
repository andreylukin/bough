//! §0.2 runtime invariants for the rollups seam. Both are returned by the PROVIDERS'
//! `Plugin::invariants()`; the Definition owns the statement so two providers cannot disagree
//! about what "sealed once" means.
//!
//! 1. **`seal_once`** — over the observed `ledger/step` stream filtered to `rollup/sealed`: no two
//!    observations name the same `(traj, tier, from_seq, to_seq, gen)`, and no observation names a
//!    `(traj, tier, from_seq, to_seq)` whose generation is not exactly one above the highest
//!    already seen for it. This is the event-stream half V1 asks for; the ledger's own
//!    `seal_once` (a `superseded_by` transition happens at most once) is the row half.
//! 2. **`tiers_are_an_index`** — for every `rollup/sealed` observed, every id in the block's
//!    `beneath` and `evidence` resolves to a row that exists in the store at quiesce.
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] for both (P1-D14; the kernel dispatches no
//! other).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{RollupId, Seq, StepId, TrajId};
use parking_lot::Mutex;

/// One sealed block, as observed on the `ledger/step` stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub rollup: RollupId,
    pub traj: TrajId,
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    /// The generation encoded in the block's deterministic id (0 for the original).
    pub generation: u32,
    /// Ids the block's `beneath` and `evidence` name; `tiers_are_an_index` resolves them.
    pub beneath_steps: Vec<StepId>,
    pub beneath_rollups: Vec<RollupId>,
}

/// What the providers recorded this session, in seal order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one sealed block. Called by a provider after `rollup/sealed` is appended.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Everything recorded this session.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Forget the record. Tests only; the runner never calls it.
pub fn reset() {
    SEEN.lock().clear();
}

/// PURE: judge a stream of observations against the seal-once statement.
///
/// Written as a function of data so a planted violation is a unit test rather than a live run.
pub fn evaluate_seal_once(_obs: &[Obs]) -> Result<(), String> {
    todo!("WP-1: seal-once stream evaluation")
}

/// The event-stream half of §3's seal-once.
pub fn seal_once() -> InvariantSpec {
    InvariantSpec {
        name: "a_range_is_sealed_once_and_generations_never_skip",
        plugin: "rollups",
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check_seal_once(ctx)),
    }
}

/// §3: tiers are an INDEX — every ref a sealed block names resolves.
pub fn tiers_are_an_index() -> InvariantSpec {
    InvariantSpec {
        name: "every_ref_a_sealed_block_names_resolves",
        plugin: "rollups",
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check_index(ctx)),
    }
}

async fn check_seal_once(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-1: seal-once check")
}

async fn check_index(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-1: tiers-are-an-index check")
}
