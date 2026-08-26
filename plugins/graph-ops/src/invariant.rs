//! §0.2 runtime invariants for the graph seam. All three read the LEDGER and the `agents` rows —
//! the authoritative relations — rather than what an op reported about itself.
//!
//! 1. **`split_has_two_edges`** — every `graph/split` step is preceded by exactly two ancestor
//!    edges and two `fork/end-seed` markers naming its `at_seq`.
//! 2. **`merge_is_reconciled`** — every `graph/merge` step is preceded by exactly two `merge`
//!    edges and one reconciliation rollup whose `src_trajs` are both parents.
//! 3. **`absorbed_row_is_gone`** — no `agents` row named by a `graph/merge` as `absorbed` exists
//!    afterwards. Both TRAJECTORIES remain: §3 deletes the losing row, never its past.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] for all three (P1-D14).

use bough_kernel::InvariantSpec;

/// Clause 1.
pub fn split_has_two_edges() -> InvariantSpec {
    todo!("WP-3: pair each graph/split with its edges and end-seeds")
}

/// Clause 2.
pub fn merge_is_reconciled() -> InvariantSpec {
    todo!("WP-3: pair each graph/merge with two merge edges and one reconciliation rollup")
}

/// Clause 3.
pub fn absorbed_row_is_gone() -> InvariantSpec {
    todo!("WP-3: assert no agents row survives for an absorbed name")
}
