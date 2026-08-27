//! Invariant (P2-D8's shape): the dormant set is a CACHE of a fold over `agent/dormancy` steps.
//! The last step on the agent's OWN trajectory wins; no step means awake. The fold reads one
//! trajectory and ignores every other, so one lane's sleep can never put another to bed.

use bough_plugin_ledger::{Step, TrajId};

/// PURE: the dormant state implied by an agent's `agent/dormancy` steps. The slice may be in any
/// order — the SEQ decides, never the position — and no step means awake.
pub fn dormant_from(steps_desc: &[Step]) -> bool {
    steps_desc
        .iter()
        .filter(|s| s.kind.as_str() == crate::STEP_TYPE)
        .max_by_key(|s| s.seq)
        .and_then(|s| s.body.get("dormant").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// The same fold restricted to ONE trajectory: what a caller holding a mixed stream needs.
pub fn dormant_from_traj(steps: &[Step], traj: &TrajId) -> bool {
    let mine: Vec<Step> = steps.iter().filter(|s| s.traj == *traj).cloned().collect();
    dormant_from(&mine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, Seq, StepId, StepType, WakeId};

    fn step(traj: &str, seq: u64, dormant: bool) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new(traj),
            seq: Seq(seq),
            at: chrono::Utc::now(),
            wake: WakeId::new("w1"),
            kind: StepType::new(crate::STEP_TYPE),
            class: Class::Evidence,
            body: std::sync::Arc::new(serde_json::json!({ "dormant": dormant })),
            cites: Default::default(),
            refs: Default::default(),
            ignorable: false,
        }
    }

    #[test]
    fn the_last_dormancy_step_wins() {
        let asleep = step("lane/sol", 1, true);
        let awake = step("lane/sol", 2, false);
        assert!(dormant_from(std::slice::from_ref(&asleep)));
        assert!(!dormant_from(&[asleep.clone(), awake.clone()]));
        // Position in the slice is not the order; the seq is.
        assert!(!dormant_from(&[awake.clone(), asleep.clone()]));
        assert!(dormant_from(&[awake, asleep, step("lane/sol", 3, true)]));
    }

    #[test]
    fn no_step_means_awake() {
        assert!(!dormant_from(&[]));
        // A stream of OTHER step types is still no dormancy step.
        let mut other = step("lane/sol", 9, true);
        other.kind = StepType::new("thought/text");
        assert!(!dormant_from(&[other]));
    }

    #[test]
    fn the_fold_ignores_other_trajectories() {
        let stream = vec![step("lane/sol", 1, true), step("lane/terra", 2, false)];
        assert!(
            dormant_from_traj(&stream, &TrajId::new("lane/sol")),
            "terra waking up must not wake sol"
        );
        assert!(!dormant_from_traj(&stream, &TrajId::new("lane/terra")));
        assert!(!dormant_from_traj(&stream, &TrajId::new("lane/nobody")));
    }
}
