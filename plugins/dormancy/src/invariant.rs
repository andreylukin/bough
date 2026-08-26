//! §0.2 runtime invariant, and the DURABLE form of "a dormant agent gets no ticks and no wakes"
//! (§1): **`no_wake_while_dormant`** — no `wake/start` step exists on an agent's trajectory at a
//! seq where the `agent/dormancy` fold says it was dormant. It reads the ledger, not what the
//! admission listener reported about itself: a loop Provider that forgot to dispatch the
//! waterfall is exactly the case this has to catch.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::InvariantSpec;

/// The clause above.
pub fn no_wake_while_dormant() -> InvariantSpec {
    todo!("WP-2: replay agent/dormancy against wake/start per trajectory")
}
