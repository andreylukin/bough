//! §0.2 runtime invariant, and the DURABLE form of "a dormant agent gets no ticks and no wakes"
//! (§1): **`no_wake_while_dormant`** — no `wake/start` step exists on an agent's trajectory at a
//! seq where the `agent/dormancy` fold says it was dormant. It reads the ledger, not what the
//! admission listener reported about itself: a loop Provider that forgot to dispatch the
//! waterfall is exactly the case this has to catch.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use std::collections::BTreeMap;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, Step, StepQuery};

/// The name the violation carries.
pub const NAME: &str = "no_wake_while_dormant";

/// PURE: replay `agent/dormancy` against `wake/start`, per trajectory, in seq order.
///
/// A `wake/start` at a seq ABOVE the last `agent/dormancy { dormant: true }` and below the
/// reactivation that followed it is a wake that should never have existed.
pub fn evaluate(steps: &[Step]) -> Result<(), String> {
    let mut ordered: Vec<&Step> = steps.iter().collect();
    ordered.sort_by_key(|s| (s.traj.to_string(), s.seq));
    let mut dormant: BTreeMap<String, bool> = BTreeMap::new();
    for step in ordered {
        let traj = step.traj.to_string();
        match step.kind.as_str() {
            crate::STEP_TYPE => {
                let is = step
                    .body
                    .get("dormant")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                dormant.insert(traj, is);
            }
            "wake/start" if *dormant.get(&traj).unwrap_or(&false) => {
                return Err(format!(
                    "wake `{}` started on `{}` at seq {} while the agent was dormant",
                    step.wake, step.traj, step.seq.0
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The clause above.
pub fn no_wake_while_dormant() -> InvariantSpec {
    InvariantSpec {
        name: "no_wake_while_dormant",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let violation = |detail: String| InvariantViolation {
        invariant: "no_wake_while_dormant",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let ledger = match ctx.try_get::<Ledger>() {
        Ok(Some(l)) => l,
        Ok(None) => return Err(violation("no ledger bound".to_string())),
        Err(e) => return Err(violation(e.to_string())),
    };
    let steps = ledger
        .0
        .steps(&StepQuery::default())
        .await
        .map_err(|e| violation(e.to_string()))?;
    evaluate(&steps).map_err(violation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, Seq, StepId, StepType, TrajId, WakeId};

    fn step(traj: &str, seq: u64, kind: &str, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(format!("{traj}-{seq}")),
            traj: TrajId::new(traj),
            seq: Seq(seq),
            at: chrono::Utc::now(),
            wake: WakeId::new(format!("w{seq}")),
            kind: StepType::new(kind),
            class: Class::Evidence,
            body: std::sync::Arc::new(body),
            cites: Default::default(),
            refs: Default::default(),
            ignorable: false,
        }
    }

    fn sleep(traj: &str, seq: u64) -> Step {
        step(
            traj,
            seq,
            crate::STEP_TYPE,
            serde_json::json!({ "dormant": true }),
        )
    }
    fn wake_up(traj: &str, seq: u64) -> Step {
        step(
            traj,
            seq,
            crate::STEP_TYPE,
            serde_json::json!({ "dormant": false }),
        )
    }
    fn wake_start(traj: &str, seq: u64) -> Step {
        step(traj, seq, "wake/start", serde_json::json!({}))
    }

    #[test]
    fn a_wake_started_while_dormant_is_reported() {
        let planted = vec![sleep("lane/sol", 1), wake_start("lane/sol", 2)];
        let detail = evaluate(&planted).expect_err("a wake under dormancy must be reported");
        assert!(detail.contains("lane/sol"), "{detail}");
        assert!(detail.contains("dormant"), "{detail}");
        assert!(detail.contains("seq 2"), "{detail}");
    }

    #[test]
    fn a_clean_stream_passes() {
        // Awake, then asleep with no wake, then reactivated and woken: the whole life.
        let clean = vec![
            wake_start("lane/sol", 1),
            sleep("lane/sol", 2),
            wake_up("lane/sol", 5),
            wake_start("lane/sol", 6),
            // Another lane asleep at the same time, and never woken.
            sleep("lane/terra", 3),
        ];
        assert_eq!(evaluate(&clean), Ok(()));
        // One lane's sleep never condemns another's wake.
        let mixed = vec![sleep("lane/terra", 1), wake_start("lane/sol", 2)];
        assert_eq!(evaluate(&mixed), Ok(()));
    }
}
