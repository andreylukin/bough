//! Invariant: degradation runs in a FIXED reverse order and stops as soon as the draft fits, and
//! degradation of pins, digest or mail is NEVER SILENT — each raises an in-context flag (§5). The
//! ladder is a data-driven list, in one place, so it cannot drift into an `if` chain.
//!
//! | rung | action | flag |
//! |---|---|---|
//! | 1 | drop `tier` sections finest-first and every `DropPriority::Fine` section | — |
//! | 2 | shrink the tail toward `tail_floor_steps`, oldest first | — |
//! | 3 | drop remaining coarse tiers and `DropPriority::Coarse` sections | — |
//! | 4 | collapse pins to titles + count | `PinsDegraded` |
//! | 5 | collapse mail to per-class counts + newest N | `MailDegraded` |
//! | 6 | truncate the digest body to its first paragraph | `DigestDegraded` |
//! | — | still over | `OverBudget` — nothing is dropped silently |
//!
//! `DropPriority::Never` sections and the identity band are never dropped: an answer wake must
//! always be buildable (§5). Rung 3 exists so the ladder is TOTAL (P1-D21).

use bough_plugin_projection::Draft;

use crate::AssemblerConfig;

/// One rung of the ladder.
pub struct Rung {
    /// What the rung is called in a test failure.
    pub name: &'static str,
    /// Apply the rung to the draft in place.
    pub apply: fn(&mut Draft, &AssemblerConfig),
}

/// The ladder, in order. THE readable statement of §5's degradation policy.
pub fn ladder() -> &'static [Rung] {
    todo!("WP-5: degrade::ladder")
}

/// Run the ladder until the draft fits `effective_budget`, or every rung is spent and `OverBudget`
/// is raised.
pub fn degrade(draft: &mut Draft, cfg: &AssemblerConfig, effective_budget: usize) {
    todo!("WP-5: degrade::degrade")
}
