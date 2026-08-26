//! Invariant (§3): re-accepting or editing a requirement SUPERSEDES its old pin rather than
//! editing one. A pin is a fact with a lifetime; rewriting it in place would make the projection
//! of an old wake unreconstructible.

use bough_plugin_ledger::StepId;

/// PURE: the `supersedes` list for a requirement's new pin, given the pins its claim has set
/// before.
pub fn supersedes_for(_previous: &[StepId]) -> Vec<StepId> {
    todo!("WP-4: every live pin this claim set before")
}
