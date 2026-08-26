//! §0.2 runtime invariant for `bough-plugin-about-line`:
//!
//! **Every `about/line` cites at least one step that exists, and follows a `completed`
//! `wake/end`.**
//!
//! The line is EVIDENCE, so its state half must be anchored in steps rather than in a
//! recollection; and a preempted wake refreshes nothing, which is exactly what "follows a
//! completed wake/end" checks.

use std::collections::BTreeSet;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::vocabulary::{WakeEnd, WakeEndReason};
use bough_plugin_ledger::{Ledger, Order, Ref, Step, StepQuery};

/// The whole invariant as a pure function of a trajectory's steps, in seq order.
pub fn evaluate(steps: &[Step]) -> Result<(), String> {
    let known: BTreeSet<Ref> = steps
        .iter()
        .map(|s| Ref::new(format!("step:{}", s.id)))
        .collect();

    for (i, step) in steps.iter().enumerate() {
        if step.kind.as_str() != crate::ABOUT_LINE {
            continue;
        }
        if step.cites.is_empty() {
            return Err(format!(
                "`about/line` `{}` cites nothing; the state half must be anchored in steps",
                step.id
            ));
        }
        for cite in step.cites.iter() {
            if !known.contains(&cite.r#ref) {
                return Err(format!(
                    "`about/line` `{}` cites `{}`, which is not a step on this trajectory",
                    step.id, cite.r#ref
                ));
            }
        }
        // The refresh is appended right after `wake/end` (P2-D11), so the nearest preceding
        // `wake/end` of the SAME wake is the one it followed.
        let preceding = steps[..i]
            .iter()
            .rev()
            .find(|s| s.kind.as_str() == "wake/end" && s.wake == step.wake);
        let Some(end) = preceding else {
            return Err(format!(
                "`about/line` `{}` precedes the `wake/end` of wake `{}`; the refresh happens on \
                 the wake-end moment",
                step.id, step.wake
            ));
        };
        let body: WakeEnd = serde_json::from_value((*end.body).clone())
            .map_err(|e| format!("`wake/end` `{}` has an unreadable body: {e}", end.id))?;
        if body.reason != WakeEndReason::Completed {
            return Err(format!(
                "`about/line` `{}` follows a `{:?}` wake; only a completed wake refreshes the \
                 line (§5)",
                step.id, body.reason
            ));
        }
    }
    Ok(())
}

/// The spec `AboutLinePlugin::invariants` returns.
pub fn lines_cite_and_follow_completed_wakes() -> InvariantSpec {
    InvariantSpec {
        name: "about_lines_cite_real_steps_and_follow_completed_wakes",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: "about_lines_cite_real_steps_and_follow_completed_wakes",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        // The row is being torn down: there is nothing to state about a ledger that is gone.
        return Ok(());
    };
    // Every trajectory an agent row points at: the `agents` table is the only enumeration of
    // chains the ledger offers, and a chain with no agent row is nothing this invariant is
    // about.
    let mut trajs: BTreeSet<bough_plugin_ledger::TrajId> = BTreeSet::new();
    for row in ledger.0.agents().await.map_err(|e| fail(e.to_string()))? {
        trajs.insert(row.traj);
    }
    for traj in trajs {
        let steps = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .map_err(|e| fail(e.to_string()))?;
        evaluate(&steps).map_err(fail)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Cite, Class, Seq, StepId, StepType, TrajId, WakeId};
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    fn step(seq: u64, id: &str, kind: &str, body: serde_json::Value, cites: Vec<&str>) -> Step {
        Step {
            id: StepId::new(id),
            traj: TrajId::new("t1"),
            seq: Seq(seq),
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(body),
            cites: Arc::new(
                cites
                    .into_iter()
                    .map(|c| Cite {
                        r#ref: Ref::new(format!("step:{c}")),
                        url: None,
                    })
                    .collect(),
            ),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    fn wake_end(seq: u64, id: &str, reason: WakeEndReason) -> Step {
        step(
            seq,
            id,
            "wake/end",
            serde_json::json!({ "reason": reason, "cause": null, "consumed": [] }),
            vec![],
        )
    }

    fn about(seq: u64, id: &str, cites: Vec<&str>) -> Step {
        step(
            seq,
            id,
            crate::ABOUT_LINE,
            serde_json::json!({ "state": "s", "intent": "i", "of_wake": "w1" }),
            cites,
        )
    }

    #[test]
    fn a_line_after_a_completed_wake_citing_a_real_step_is_fine() {
        let steps = vec![
            step(
                1,
                "s1",
                "thought/text",
                serde_json::json!({ "text": "x" }),
                vec![],
            ),
            wake_end(2, "e1", WakeEndReason::Completed),
            about(3, "a1", vec!["s1"]),
        ];
        assert_eq!(evaluate(&steps), Ok(()));
    }

    #[test]
    fn a_line_citing_nothing_is_a_violation() {
        let steps = vec![
            wake_end(1, "e1", WakeEndReason::Completed),
            about(2, "a1", vec![]),
        ];
        assert!(evaluate(&steps).unwrap_err().contains("cites nothing"));
    }

    #[test]
    fn a_line_citing_a_step_that_does_not_exist_is_a_violation() {
        let steps = vec![
            wake_end(1, "e1", WakeEndReason::Completed),
            about(2, "a1", vec!["ghost"]),
        ];
        assert!(evaluate(&steps).unwrap_err().contains("not a step"));
    }

    /// §5: a preempted wake refreshes nothing.
    #[test]
    fn a_line_after_an_interrupted_wake_is_a_violation() {
        let steps = vec![
            step(
                1,
                "s1",
                "thought/text",
                serde_json::json!({ "text": "x" }),
                vec![],
            ),
            wake_end(2, "e1", WakeEndReason::Interrupted),
            about(3, "a1", vec!["s1"]),
        ];
        assert!(evaluate(&steps)
            .unwrap_err()
            .contains("only a completed wake"));
    }
}
