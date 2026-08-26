//! Invariant: `step_refs` is DERIVED, never caller-supplied, and derived by THIS function in every
//! provider — which is why the two providers' matching indexes cannot diverge (§3).

use std::collections::BTreeSet;

use crate::id::Ref;
use crate::step::Cite;

/// Every `ref` / `refs` value found at ANY depth of `body` (a string, or an array of strings).
/// Deterministic, order-independent, allocation-bounded. Non-string values are ignored.
pub fn body_refs(body: &serde_json::Value) -> BTreeSet<Ref> {
    todo!("WP-1: refs::body_refs")
}

/// The union of every cite's `ref` and [`body_refs`]. This is a step's canonical ref set.
pub fn derive_step_refs(cites: &[Cite], body: &serde_json::Value) -> BTreeSet<Ref> {
    todo!("WP-1: refs::derive_step_refs")
}
