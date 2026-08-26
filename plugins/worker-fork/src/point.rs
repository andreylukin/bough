//! Invariant (P5-D7): the fork point is RESOLVED. The parent's head when it is outside an open
//! wake, else the last seq that is. Never pauses, never waits, never clips silently — §4 says the
//! parent never pauses, and §3 refuses a fork whose prefix ends inside an open wake, so the only
//! honest answer is to branch below the open wake and REPORT the seq used.

use bough_plugin_ledger::{Seq, Step};

/// PURE: the seq a fork may branch at, given the parent's chain newest-first. `None` for an empty
/// chain.
pub fn fork_point(_steps_desc: &[Step]) -> Option<Seq> {
    todo!("WP-6: the head, or the last seq below a trailing open wake")
}
