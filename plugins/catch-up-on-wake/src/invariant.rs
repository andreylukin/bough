//! §0.2 runtime invariant for `bough-plugin-catch-up-on-wake`:
//!
//! **No agent is asked for two catch-up wakes attributable to ONE power event.** A second one
//! would re-read mail the first already consumed and put the same evidence in front of a model
//! twice. The check is a fold over this row's own observed `request_wake` stream, per fiber, keyed
//! by the wake's timestamp — so two genuinely different wakes are not confused for one.
//!
//! Eligibility (kind, disposed) is NOT checked here: it is decided before the request by
//! [`crate::eligible`], a pure function with its own tests, and a fold over past requests cannot
//! re-derive what an agent's status was at the moment it was asked.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_agents::AgentId;
use chrono::{DateTime, Utc};

/// One observed catch-up request.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub agent: AgentId,
    /// The `DidWake`'s own timestamp — what makes "one power event" identifiable.
    pub event_at: DateTime<Utc>,
    pub started: bool,
}

static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// How many observations ONE fiber keeps. One wake records one entry per agent.
const PER_FIBER_CAP: usize = 1024;

/// Record one moment. Called by [`crate::CatchUpOnWake::on_wake`].
pub fn record(obs: Obs) {
    let mut seen = SEEN.lock();
    let fiber = obs.fiber;
    seen.push(obs);
    let count = seen.iter().filter(|o| o.fiber == fiber).count();
    if count > PER_FIBER_CAP {
        let mut to_drop = count - PER_FIBER_CAP;
        seen.retain(|o| {
            if o.fiber == fiber && to_drop > 0 {
                to_drop -= 1;
                false
            } else {
                true
            }
        });
    }
}

/// Forget everything recorded for `fiber`, as an inverse of `apply`.
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| o.fiber != fiber);
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Drop the recorded stream. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
}

/// PURE: the fold the check runs.
pub fn check_stream(seen: &[Obs]) -> Result<(), String> {
    use std::collections::BTreeSet;
    let mut once: BTreeSet<(FiberUid, String, DateTime<Utc>)> = BTreeSet::new();
    for obs in seen {
        let key = (obs.fiber, obs.agent.to_string(), obs.event_at);
        if !once.insert(key) {
            return Err(format!(
                "agent `{}` was asked for a second catch-up wake for the wake at {}",
                obs.agent, obs.event_at
            ));
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "no_agent_gets_two_catch_up_wakes_for_one_power_event",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    check_stream(&seen()).map_err(|detail| InvariantViolation {
        invariant: "no_agent_gets_two_catch_up_wakes_for_one_power_event",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fibers() -> (FiberUid, FiberUid) {
        let core = bough_kernel::KernelCore::new();
        (core.new_fiber_uid(), core.new_fiber_uid())
    }

    fn obs(fiber: FiberUid, agent: &str, at: DateTime<Utc>) -> Obs {
        Obs {
            fiber,
            agent: AgentId::new(agent),
            event_at: at,
            started: true,
        }
    }

    #[test]
    fn one_request_per_agent_per_wake_is_clean() {
        let (f, _) = fibers();
        let at = Utc::now();
        assert_eq!(
            check_stream(&[obs(f, "a", at), obs(f, "b", at)]),
            Ok(()),
            "two agents, one wake"
        );
    }

    #[test]
    fn the_same_agent_over_two_wakes_is_clean() {
        let (f, _) = fibers();
        let first = Utc::now();
        let second = first + chrono::Duration::hours(8);
        assert_eq!(
            check_stream(&[obs(f, "a", first), obs(f, "a", second)]),
            Ok(())
        );
    }

    #[test]
    fn a_second_request_for_one_wake_is_a_violation() {
        let (f, _) = fibers();
        let at = Utc::now();
        let detail =
            check_stream(&[obs(f, "a", at), obs(f, "a", at)]).expect_err("must be reported");
        assert!(detail.contains("second catch-up"), "{detail}");
    }

    #[test]
    fn a_reload_is_not_a_violation_of_its_predecessor() {
        let (a, b) = fibers();
        let at = Utc::now();
        assert_eq!(check_stream(&[obs(a, "x", at), obs(b, "x", at)]), Ok(()));
    }
}
