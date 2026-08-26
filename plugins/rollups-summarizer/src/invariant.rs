//! §0.2 runtime invariant: this row returns the rollups SEAM's two specs
//! ([`bough_plugin_rollups::invariant::seal_once`] and
//! [`bough_plugin_rollups::invariant::tiers_are_an_index`]) rather than declaring its own, so both
//! providers of `rollups` are judged by one statement of the contract (P4-D1).
//!
//! What lives here is the RECORDING side: the provider hands each sealed block to the seam's
//! observation record the moment `rollup/sealed` is appended.

use bough_plugin_rollups::invariant::Obs;

/// Record one sealed block on the seam's stream.
pub fn observe(obs: Obs) {
    bough_plugin_rollups::invariant::record(obs);
}
