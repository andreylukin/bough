//! §0.2 runtime invariant: **`adoption_names_its_unrouted_step`** — every `mail/adopted` step
//! names a `mail/unrouted` step that exists, and no `mail/unrouted` step is adopted twice. It
//! reads the ledger rather than the leader's own bookkeeping: an adoption that consumed an item
//! nobody can find, or consumed one twice, is exactly the silent double-delivery §5 forbids.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::InvariantSpec;

/// The clause above.
pub fn adoption_names_its_unrouted_step() -> InvariantSpec {
    todo!("WP-5: pair each mail/adopted with its mail/unrouted, at most once")
}
