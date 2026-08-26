//! §0.2 runtime invariants for the claims seam, both read from the LEDGER rather than from what
//! the seam recorded about itself.
//!
//! 1. **`decided_once`** — no `claim/proposed` has both an accepted and a rejected step, and none
//!    has two of either. A claim is decided once.
//! 2. **`accepted_requirement_has_a_pin`** — every `claim/accepted` whose proposal was a
//!    `Requirement` is followed by a `pin/set` citing it. §3's "accepted requirements are pins" in
//!    its durable form.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] for both (P1-D14).

use bough_kernel::InvariantSpec;

/// Clause 1.
pub fn decided_once() -> InvariantSpec {
    todo!("WP-4: group claim/* by claim id and assert one decision each")
}

/// Clause 2.
pub fn accepted_requirement_has_a_pin() -> InvariantSpec {
    todo!("WP-4: pair each accepted requirement with its pin/set")
}
