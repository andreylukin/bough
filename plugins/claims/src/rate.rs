//! Invariant: a rate over ZERO decisions is not a number. §8's claim-rejection signal stays
//! Inactive when the window holds no decided claim, because "0% rejected" and "nothing decided"
//! are different facts and only one of them is a drift signal.

use bough_plugin_ledger::Step;

/// A rejection rate in `0.0..=1.0`, with the counts that produced it so a caller can render both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    pub rejected: usize,
    pub decided: usize,
    pub rate: f64,
}

/// PURE over the steps it is handed: rejected / decided, where an EDIT counts as an acceptance.
/// `None` when nothing in the window was decided.
pub fn rejection_rate(_steps: &[Step]) -> Option<Rate> {
    todo!("WP-4: count claim/accepted and claim/rejected in the window")
}
