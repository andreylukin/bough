//! Invariant (P2-D8's shape): the dormant set is a CACHE of a fold over `agent/dormancy` steps.
//! The last step on the agent's OWN trajectory wins; no step means awake. The fold reads one
//! trajectory and ignores every other, so one lane's sleep can never put another to bed.

use bough_plugin_ledger::Step;

/// PURE: the dormant state implied by an agent's `agent/dormancy` steps, newest first. `None`
/// steps ⇒ awake.
pub fn dormant_from(_steps_desc: &[Step]) -> bool {
    todo!("WP-2: the last dormancy step wins; no step means awake")
}
