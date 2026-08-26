//! §0.2 runtime invariant for `bough-plugin-tools`:
//!
//! **Every `tool/result` has a matching `tool/call` in the SAME wake and the same step, and no
//! call is answered twice.**
//!
//! This is §0.2's own worked example. The check is a fold over the observed `ledger/step` stream,
//! per fiber and bounded.

use std::collections::HashMap;
use std::sync::OnceLock;

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{StepType, WakeId};
use parking_lot::Mutex;

/// How many observations one fiber keeps. Bounded on purpose (§0.2): an invariant that grows
/// without limit is a leak, not a check.
const PER_FIBER_CAP: usize = 4096;

/// One observed step, reduced to what the invariant is about.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub wake: WakeId,
    pub kind: StepType,
    /// `tool/call` and `tool/result` both carry one.
    pub call: String,
    pub step_index: u32,
}

fn streams() -> &'static Mutex<Vec<Obs>> {
    static S: OnceLock<Mutex<Vec<Obs>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record one step.
pub fn record(obs: Obs) {
    let mut s = streams().lock();
    let fiber = obs.fiber;
    s.push(obs);
    // Bound per fiber, oldest first.
    let count = s.iter().filter(|o| o.fiber == fiber).count();
    if count > PER_FIBER_CAP {
        let mut to_drop = count - PER_FIBER_CAP;
        s.retain(|o| {
            if o.fiber == fiber && to_drop > 0 {
                to_drop -= 1;
                false
            } else {
                true
            }
        });
    }
}

/// Forget everything recorded for `fiber`.
pub fn forget(fiber: FiberUid) {
    streams().lock().retain(|o| o.fiber != fiber);
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    streams().lock().clone()
}

/// The whole invariant as a pure function of the observed stream.
///
/// Two fibers are two streams (Phase 1's lesson): a call in one fiber never answers a result in
/// another.
pub fn evaluate(stream: &[Obs]) -> Result<(), String> {
    // (fiber, call id) -> the call's (wake, step_index)
    let mut calls: HashMap<(FiberUid, String), (WakeId, u32)> = HashMap::new();
    let mut answered: HashMap<(FiberUid, String), ()> = HashMap::new();
    for o in stream {
        let key = (o.fiber, o.call.clone());
        match o.kind.as_str() {
            "tool/call" => {
                if calls.insert(key, (o.wake.clone(), o.step_index)).is_some() {
                    return Err(format!("tool call `{}` was issued twice", o.call));
                }
            }
            "tool/result" => {
                let Some((wake, step_index)) = calls.get(&key) else {
                    return Err(format!(
                        "tool/result for call `{}` has no tool/call in the same wake",
                        o.call
                    ));
                };
                if wake != &o.wake || *step_index != o.step_index {
                    return Err(format!(
                        "tool/result for call `{}` is in wake `{}` step {} but its tool/call is in wake `{}` step {}",
                        o.call, o.wake, o.step_index, wake, step_index
                    ));
                }
                if answered.insert(key, ()).is_some() {
                    return Err(format!("tool call `{}` was answered twice", o.call));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The spec `ToolsPlugin::invariants` returns.
pub fn calls_and_results_pair_within_a_step() -> InvariantSpec {
    InvariantSpec {
        name: "tool_calls_and_results_pair_within_a_step",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "tool_calls_and_results_pair_within_a_step",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(fiber: u64, wake: &str, kind: &str, call: &str, step: u32) -> Obs {
        Obs {
            fiber: FiberUid(fiber),
            wake: WakeId::new(wake),
            kind: StepType::new(kind),
            call: call.to_string(),
            step_index: step,
        }
    }

    #[test]
    fn a_matching_pair_is_clean() {
        let s = vec![
            obs(1, "w1", "tool/call", "c1", 3),
            obs(1, "w1", "tool/result", "c1", 3),
        ];
        assert!(evaluate(&s).is_ok());
    }

    #[test]
    fn a_result_without_a_call_is_a_violation() {
        let s = vec![obs(1, "w1", "tool/result", "c1", 3)];
        assert!(evaluate(&s).unwrap_err().contains("no tool/call"));
    }

    #[test]
    fn a_result_in_another_wake_is_a_violation() {
        let s = vec![
            obs(1, "w1", "tool/call", "c1", 3),
            obs(1, "w2", "tool/result", "c1", 3),
        ];
        assert!(evaluate(&s).unwrap_err().contains("wake"));
    }

    #[test]
    fn a_result_in_another_step_of_the_same_wake_is_a_violation() {
        let s = vec![
            obs(1, "w1", "tool/call", "c1", 3),
            obs(1, "w1", "tool/result", "c1", 4),
        ];
        assert!(evaluate(&s).unwrap_err().contains("step"));
    }

    #[test]
    fn answering_a_call_twice_is_a_violation() {
        let s = vec![
            obs(1, "w1", "tool/call", "c1", 3),
            obs(1, "w1", "tool/result", "c1", 3),
            obs(1, "w1", "tool/result", "c1", 3),
        ];
        assert!(evaluate(&s).unwrap_err().contains("answered twice"));
    }

    #[test]
    fn two_fibers_are_two_streams() {
        // Fiber 2's result must not be paired with fiber 1's call.
        let s = vec![
            obs(1, "w1", "tool/call", "c1", 3),
            obs(2, "w1", "tool/result", "c1", 3),
        ];
        assert!(evaluate(&s).is_err());
    }
}
