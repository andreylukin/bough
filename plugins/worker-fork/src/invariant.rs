//! §0.2 runtime invariant: **`pinned_prefix_reconstructs`** — every `fork/prefix` step names an
//! agent and a seq at which the parent's projection CAN still be assembled, and the child's
//! `request/header` digest for its one call equals the parent's at that seq. It reads the ledger,
//! not what the provider reported: a pin that quietly diverged from the parent's projection is the
//! exact failure §0.2's reconstruction rule exists to catch.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::InvariantSpec;

/// The clause above.
pub fn pinned_prefix_reconstructs() -> InvariantSpec {
    todo!("WP-6: match each fork/prefix against the child's request header digest")
}
