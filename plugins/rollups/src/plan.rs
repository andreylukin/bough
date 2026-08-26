//! Invariant: the planner refuses BEFORE the store does. A range already covered by a block in
//! this crate's namespace is never planned again — superseded blocks included, because a
//! superseded range is still sealed — and a block minted by the §14 bridge is in a foreign
//! namespace and therefore invisible to the overlap check (P4-D13).

use bough_plugin_ledger::{Rollup, RollupId, Seq, TrajId};

use crate::request::SealPlan;
use crate::window::{Window, WindowCfg};

/// The tier tree's shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TierCfg {
    /// §3: fanout ~10. Tier k+1 reduces exactly `fanout` tier-k blocks.
    pub fanout: usize,
    /// The highest tier this deployment builds.
    pub max_tier: u8,
    /// Never seal within this many steps of the head (P4-D11).
    pub lag: usize,
    /// The tier-1 window size the coverage arithmetic is stated against. Mirrors the summarizer
    /// row's `max_window_steps`, so [`coverage`] is a total function of `TierCfg` alone.
    pub max_window_steps: usize,
}

/// The deterministic id of a tier block.
///
/// EXCLUDES `prompt_ver` on purpose (P4-D4): a prompt bump must not re-open a sealed range.
/// `gen` 0 is the original; n>0 is the nth supersession.
pub fn tier_id(_traj: &TrajId, _tier: u8, _from: Seq, _to: Seq, _gen: u32) -> RollupId {
    todo!("WP-1: deterministic tier-block id")
}

/// `true` iff `id` is in this crate's namespace.
///
/// Bridge blocks (`old-feed:…`) are not, and are therefore invisible to the overlap check
/// (P4-D13): the two vocabularies coexist and neither poisons the other's seal-once arithmetic.
pub fn is_ours(_id: &RollupId) -> bool {
    todo!("WP-1: namespace test")
}

/// §3: "tier k covers ~10^k steps". The exact arithmetic, so the property is a unit test rather
/// than a comment: `max_window_steps * fanout^(tier-1)`.
pub fn coverage(_tier: u8, _cfg: &TierCfg) -> usize {
    todo!("WP-1: per-tier coverage")
}

/// The whole plan, from the ledger's own rows.
///
/// `existing` is every rollup on the trajectory, superseded ones INCLUDED — a superseded range is
/// still sealed and is never re-planned.
pub fn plan(
    _existing: &[Rollup],
    _windows: &[Window],
    _head: Seq,
    _upto: Seq,
    _traj: &TrajId,
    _cfg: &TierCfg,
    _wcfg: &WindowCfg,
) -> SealPlan {
    todo!("WP-1: tier planning")
}
