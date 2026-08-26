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

/// Decide the repair for one trajectory's tail.
///
/// The tail is the trajectory's last steps in seq order. A wake is orphaned when it has a
/// `wake/start` and no `wake/end`; only the TRAILING wake can be orphaned, because the single
/// writer closes a wake before the next one opens.
pub fn plan(tail: &[Step], now: DateTime<Utc>) -> Option<Repair> {
    let mut ordered: Vec<&Step> = tail.iter().collect();
    ordered.sort_by_key(|s| s.seq);
    let last_start = ordered
        .iter()
        .rev()
        .find(|s| s.kind.as_str() == "wake/start")?;
    let wake = last_start.wake.clone();
    let closed = ordered
        .iter()
        .any(|s| s.wake == wake && s.kind.as_str() == "wake/end");
    if closed {
        return None;
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

    Some(Repair {
        traj: last_start.traj.clone(),
        wake,
        unknown_results,
        close_wake: true,
        at: now,
    })
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
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let Some(plan) = plan(&tail, now) else {
            continue;
        };
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
    fn a_closed_wake_is_left_alone_and_repair_never_touches_rollups() {
        let mut tail = orphan_tail();
        tail.push(wake_end(15, &wake_of("w9"), "completed", &[]));
        assert_eq!(plan(&tail, at(99)), None);
        // The plan's whole surface is steps: there is nowhere for a rollup to be named, which is
        // §5's "never touches rollups" as a type rather than as a promise.
        let fields = format!("{:?}", plan(&orphan_tail(), at(99)).unwrap());
        assert!(!fields.contains("rollup"));
    }
}
