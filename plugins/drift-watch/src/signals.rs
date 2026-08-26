//! Invariant: every signal is a PURE function of step data, so the whole signal surface is
//! unit-tested without a ledger — and a signal that cannot be computed yet reports
//! [`crate::SignalState::Inactive`] rather than a zero that reads like a measurement (§16).

use bough_plugin_ledger::Step;

use crate::{DriftConfig, DriftFlag, Signals, Stat, ToolShare};

/// Mean, variance, coefficient of variation and the two percentiles of a sample.
pub fn stat(_samples: &[usize]) -> Stat {
    todo!("WP-4: sample statistics")
}

/// Tool-use distribution over `tool/call` steps, most-used first.
pub fn shares(_steps: &[Step]) -> Vec<ToolShare> {
    todo!("WP-4: tool-use distribution")
}

/// Normalised Shannon entropy: 0.0 for one tool, 1.0 for uniform use.
pub fn entropy(_shares: &[ToolShare]) -> f64 {
    todo!("WP-4: normalised entropy")
}

/// What the signals flag, given the thresholds.
pub fn flags(_signals: &Signals, _cfg: &DriftConfig) -> Vec<DriftFlag> {
    todo!("WP-4: flags")
}

/// The claim-rejection signal. Wired, and INACTIVE until Phase 5's accept/reject surface exists.
pub fn claim_rejection(_steps: &[Step]) -> crate::SignalState {
    todo!("WP-4: claim-rejection signal, inactive until Phase 5")
}
