//! Invariant (P5-D7): a fork/split/bud point is RESOLVED, never clipped and never waited on. §3
//! refuses a fork whose prefix ends inside an open wake; §4 says the parent never pauses. So the
//! resolver walks DOWN to the last seq outside an open wake and the op reports the seq it used.
//! An EXPLICIT `at_seq` inside an open wake is an error, not a silent adjustment.

use bough_plugin_ledger::{Seq, Step};

/// PURE: the last seq outside an open wake, given the chain newest-first. `None` for an empty
/// chain.
pub fn resolve_point(_steps_desc: &[Step]) -> Option<Seq> {
    todo!("WP-3: walk down past any trailing open wake")
}

/// PURE: whether `at` lies inside a wake that has a `wake/start` and no `wake/end`.
pub fn inside_open_wake(_steps_desc: &[Step], _at: Seq) -> bool {
    todo!("WP-3: pair wake/start with wake/end and test containment")
}
