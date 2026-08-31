//! Invariant (§5): crash repair reads and writes STEPS ONLY. Rollups are never touched — a
//! half-finished wake is a gap in the chain, not a reason to re-derive anything above it.

use bough_plugin_ledger::{Step, StepId, TrajId, WakeId};
use chrono::{DateTime, Utc};

/// What repair decided to append for one trajectory. Pure, so the whole of V9 is testable
/// without a store: the caller does the appending.
#[derive(Clone, Debug, PartialEq)]
pub struct Repair {
    pub traj: TrajId,
    /// The orphaned wake.
    pub wake: WakeId,
    /// One `tool/result { outcome: unknown }` per `tool/call` the orphaned wake never answered.
    /// These are the STEP ids of those calls; the call id itself is read from the body.
    pub unknown_results: Vec<StepId>,
    /// Whether a `wake/end { reason: interrupted, consumed: [] }` is owed.
    pub close_wake: bool,
    /// The instant the synthesised steps carry. Injected, never read from a clock in here.
    pub at: DateTime<Utc>,
}

/// The only step types [`plan_all`] reads. Kept next to it so the read filter in [`run`] and the
/// pure planner cannot drift apart.
pub const REPAIR_KINDS: [&str; 4] = ["wake/start", "wake/end", "tool/call", "tool/result"];

/// Decide the repair for one trajectory's tail: one [`Repair`] per orphaned wake, oldest first.
///
/// EVERY unclosed wake is repaired, not only the trailing one. §5's checkpoint-and-answer opens
/// the answer wake BEFORE the wake it interrupted has closed, so a crash during a preemption
/// leaves two wakes open at once; a plan that looked only at the last `wake/start` closed the
/// answer wake and left the interrupted one open forever.
pub fn plan_all(tail: &[Step], now: DateTime<Utc>) -> Vec<Repair> {
    let mut ordered: Vec<&Step> = tail.iter().collect();
    ordered.sort_by_key(|s| s.seq);

    let mut out: Vec<Repair> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for start in ordered.iter().filter(|s| s.kind.as_str() == "wake/start") {
        let wake = start.wake.clone();
        if seen.contains(&wake.as_str()) {
            continue;
        }
        seen.push(start.wake.as_str());
        let closed = ordered
            .iter()
            .any(|s| s.wake == wake && s.kind.as_str() == "wake/end");
        if closed {
            continue;
        }

        let in_wake: Vec<&&Step> = ordered.iter().filter(|s| s.wake == wake).collect();
        let answered: Vec<String> = in_wake
            .iter()
            .filter(|s| s.kind.as_str() == "tool/result")
            .filter_map(|s| {
                s.body
                    .get("call")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .collect();
        let unknown_results: Vec<StepId> = in_wake
            .iter()
            .filter(|s| s.kind.as_str() == "tool/call")
            .filter(|s| {
                let call = s.body.get("call").and_then(|v| v.as_str()).unwrap_or("");
                !answered.iter().any(|a| a == call)
            })
            .map(|s| s.id.clone())
            .collect();

        out.push(Repair {
            traj: start.traj.clone(),
            wake,
            unknown_results,
            close_wake: true,
            at: now,
        });
    }
    out
}

/// The trailing orphan alone. Kept because the unit tests read one plan at a time; `run` uses
/// [`plan_all`].
pub fn plan(tail: &[Step], now: DateTime<Utc>) -> Option<Repair> {
    plan_all(tail, now).pop()
}

/// Apply the plan for every trajectory that owns an agent row: this is what `apply` runs at boot
/// when `repair_on_boot`.
///
/// It reads and writes STEPS ONLY (§5). `TOOL_OUTCOME_UNKNOWN` — `tool/result.outcome ==
/// "unknown"` — is synthesised first, so a wake never closes over a call that has no answer.
pub async fn run(
    ledger: &bough_plugin_ledger::LedgerHandle,
    now: DateTime<Utc>,
) -> Result<Vec<Repair>, String> {
    use bough_plugin_ledger::{Append, Class, StepQuery, StepType};

    let agents = ledger.0.agents().await.map_err(|e| e.to_string())?;
    let mut done = Vec::new();
    for row in agents {
        let tail = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![row.traj.clone()],
                // Only the four types repair reasons about. This is not an optimisation: repair
                // runs inside `apply`, BEFORE every other row has declared its own step types, and
                // an unfiltered read refuses any row whose type is not yet registered
                // (`UnknownStepTypeOnRead`, §3/P1-D7). A tree that had once written a step owned by
                // a later-applying row could therefore never boot a second time. The kinds below
                // are ledger-core vocabulary, always declared, so the filter is total.
                kinds: REPAIR_KINDS.iter().map(|k| StepType::new(*k)).collect(),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        for plan in plan_all(&tail, now) {
            for step_id in &plan.unknown_results {
                let call = tail
                    .iter()
                    .find(|s| &s.id == step_id)
                    .map(|s| s.body.as_ref().clone())
                    .unwrap_or(serde_json::Value::Null);
                let body = serde_json::json!({
                    "call": call.get("call").cloned().unwrap_or(serde_json::Value::String(String::new())),
                    "name": call.get("name").cloned().unwrap_or(serde_json::Value::String(String::new())),
                    "outcome": "unknown",
                    "content": "the harness restarted before this call reported an outcome",
                    "concludes_wake": false,
                    // The synthesised result belongs to the SAME step as the call it answers, or the
                    // tools invariant would read it as a cross-step pairing.
                    "step_index": call.get("step_index").cloned()
                        .unwrap_or(serde_json::Value::Number(0.into())),
                });
                ledger
                    .0
                    .append(Append {
                        traj: plan.traj.clone(),
                        wake: plan.wake.clone(),
                        kind: StepType::new("tool/result"),
                        class: Class::Thought,
                        body,
                        cites: vec![],
                        at: plan.at,
                        id: None,
                    })
                    .await
                    .map_err(|e| e.to_string())?;
            }
            if plan.close_wake {
                ledger
                    .0
                    .append(Append {
                        traj: plan.traj.clone(),
                        wake: plan.wake.clone(),
                        kind: StepType::new("wake/end"),
                        class: Class::Thought,
                        // `interrupted` is the one reason no loop emits (§5).
                        body: serde_json::json!({ "reason": "interrupted", "cause": null,
                                              "consumed": [] }),
                        cites: vec![],
                        at: plan.at,
                        id: None,
                    })
                    .await
                    .map_err(|e| e.to_string())?;
            }
            done.push(plan);
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{at, step, wake_end, wake_of};

    fn orphan_tail() -> Vec<Step> {
        let w = wake_of("w9");
        vec![
            step(10, &w, "wake/start", serde_json::json!({})),
            step(11, &w, "step/start", serde_json::json!({ "index": 0 })),
            step(
                12,
                &w,
                "tool/call",
                serde_json::json!({ "call": "c1", "name": "bash" }),
            ),
            step(
                13,
                &w,
                "tool/call",
                serde_json::json!({ "call": "c2", "name": "bash" }),
            ),
            step(
                14,
                &w,
                "tool/result",
                serde_json::json!({ "call": "c1", "outcome": "ok", "content": "" }),
            ),
        ]
    }

    #[test]
    fn an_orphaned_trailing_wake_closes_as_interrupted() {
        let plan = plan(&orphan_tail(), at(99)).expect("an orphaned wake is repaired");
        assert!(plan.close_wake);
        assert_eq!(plan.wake, wake_of("w9"));
        assert_eq!(plan.at, at(99), "the clock is injected, never read in here");
    }

    #[test]
    fn a_call_without_a_result_gets_tool_outcome_unknown() {
        let plan = plan(&orphan_tail(), at(99)).unwrap();
        // c1 was answered; only c2 is owed a synthesised result.
        assert_eq!(plan.unknown_results.len(), 1);
        assert_eq!(plan.unknown_results[0].as_str(), "s13");
    }

    #[test]
    fn every_open_wake_is_repaired_not_only_the_trailing_one() {
        // §5's checkpoint-and-answer: the answer wake opens BEFORE the interrupted one closes.
        let w1 = wake_of("w1");
        let w2 = wake_of("w2");
        let tail = vec![
            step(10, &w1, "wake/start", serde_json::json!({})),
            step(11, &w1, "step/start", serde_json::json!({ "index": 0 })),
            step(12, &w2, "wake/start", serde_json::json!({})),
            step(13, &w2, "step/start", serde_json::json!({ "index": 0 })),
        ];
        let plans = plan_all(&tail, at(99));
        let wakes: Vec<String> = plans.iter().map(|p| p.wake.to_string()).collect();
        assert_eq!(
            wakes,
            vec!["w1".to_string(), "w2".to_string()],
            "a crash during a preemption leaves TWO wakes open and both are closed"
        );
        assert!(plans.iter().all(|p| p.close_wake));
    }

    #[test]
    fn a_closed_wake_is_left_alone() {
        let mut tail = orphan_tail();
        tail.push(wake_end(15, &wake_of("w9"), "completed", &[]));
        assert_eq!(plan(&tail, at(99)), None);
        // The rollup half of V9 is NOT proven here: this is a pure planner and a Debug-string
        // check on a struct with no rollup field cannot fail for any implementation. The real
        // evidence is `crates/bough/tests/exec_headless.rs::repair_at_boot::
        // booting_exec_closes_an_orphaned_wake_and_leaves_rollups_alone`, which seals a rollup,
        // boots `bough exec` and re-reads it.
    }
}
