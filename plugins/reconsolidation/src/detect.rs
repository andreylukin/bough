//! Invariant: detection is PURE and the model never widens it. Pairing is arithmetic over refs;
//! staleness is arithmetic over an INJECTED `now`; and [`stale`] never returns a kind in
//! [`bough_plugin_rollups::NEVER_EXPIRABLE`], whatever the config says (V7).

use bough_plugin_ledger::vocabulary::ClaimProposed;
use bough_plugin_ledger::{Cite, Step};
use chrono::{DateTime, Utc};

use crate::{Candidate, Pair, ReconConfig};

/// Evidence steps sharing a ref, newest-vs-older, capped and deterministic.
pub fn pairs(_steps: &[Step], _max: usize) -> Vec<Pair> {
    todo!("WP-3: contradiction pairing")
}

/// Stale by age. Never returns a `NEVER_EXPIRABLE` kind, whatever the config says.
pub fn stale(_steps: &[Step], _now: DateTime<Utc>, _cfg: &ReconConfig) -> Vec<Candidate> {
    todo!("WP-3: stale-evidence candidates")
}

/// The `claim/proposed` body for a judged contradiction.
///
/// Cites BOTH steps, so the claim is evidence-backed the moment it is appended.
pub fn contradiction_claim(_pair: &Pair, _verdict: &str) -> (ClaimProposed, Vec<Cite>) {
    todo!("WP-3: contradiction claim body")
}
