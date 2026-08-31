//! §0.2 runtime invariant for `bough-plugin-residents`:
//!
//! **At most one catch-up wake per agent per activation.** A second catch-up would re-read mail
//! the first already consumed and put the same evidence in front of a model twice; the check is a
//! fold over this row's own observed `request_wake` stream, per fiber.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::AgentName;

/// One observed catch-up request.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub agent: AgentName,
    pub started: bool,
}

/// The recorded stream. Per-fiber, so a reload is not a violation of its predecessor (§0.3).
static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// How many observations ONE fiber keeps. One catch-up pass records one entry per agent, so the
/// bound is generous; it exists because an unbounded record is a leak rather than a check.
const PER_FIBER_CAP: usize = 1024;

/// Record one moment. Called by the catch-up pass.
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

/// PURE: the fold the check runs. A second catch-up for one (fiber, agent) is the violation.
pub fn check_stream(seen: &[Obs]) -> Result<(), String> {
    use std::collections::BTreeSet;
    let mut once: BTreeSet<(FiberUid, String)> = BTreeSet::new();
    for obs in seen {
        let key = (obs.fiber, obs.agent.to_string());
        if !once.insert(key) {
            return Err(format!(
                "agent `{}` was asked for a second catch-up wake in one activation",
                obs.agent
            ));
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "at_most_one_catch_up_wake_per_agent_per_activation",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    check_stream(&seen()).map_err(|detail| InvariantViolation {
        invariant: "at_most_one_catch_up_wake_per_agent_per_activation",
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

    fn obs(fiber: FiberUid, agent: &str) -> Obs {
        Obs {
            fiber,
            agent: AgentName::new(agent),
            started: true,
        }
    }

    #[test]
    fn one_catch_up_per_agent_is_clean() {
        let (f, _) = fibers();
        assert_eq!(check_stream(&[obs(f, "sol"), obs(f, "terra")]), Ok(()));
    }

    #[test]
    fn a_second_catch_up_for_one_agent_is_a_violation() {
        let (f, _) = fibers();
        let detail = check_stream(&[obs(f, "sol"), obs(f, "sol")])
            .expect_err("a second catch-up must be reported");
        assert!(detail.contains("sol"), "{detail}");
    }

    #[test]
    fn a_reload_is_not_a_violation_of_its_predecessor() {
        let (a, b) = fibers();
        assert_eq!(check_stream(&[obs(a, "sol"), obs(b, "sol")]), Ok(()));
    }
}
