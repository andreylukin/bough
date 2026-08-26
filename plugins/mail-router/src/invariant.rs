//! §0.2 runtime invariant for the mail seam. Both clauses read the LEDGER — the authoritative
//! relation — and not a record the router kept of its own behaviour: a router that dropped an
//! event without telling anyone is exactly the case an invariant has to catch.
//!
//! 1. **`unrouted_matched_nobody`** — every `mail/unrouted` step's refs match no `agents` row that
//!    already existed when it was written. The honest, weaker form of "zero matches at the time":
//!    the row history is not reconstructible, so an unrouted step whose refs match a row created
//!    BEFORE it is reported.
//! 2. **`one_delivery_per_recipient`** — every routed envelope produced exactly one
//!    `mail/delivered` step per recipient: never zero (a stranded owner) and never two (a double
//!    consumption).
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] for both (P1-D14).

use bough_kernel::InvariantSpec;

/// Clause 1.
pub fn unrouted_matched_nobody() -> InvariantSpec {
    todo!("WP-1: scan mail/unrouted against the agents rows that predate each step")
}

/// Clause 2.
pub fn one_delivery_per_recipient() -> InvariantSpec {
    todo!("WP-1: group mail/delivered by envelope fingerprint and assert one per recipient")
}
