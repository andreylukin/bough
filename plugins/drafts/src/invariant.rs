//! §0.2 runtime invariant for `bough-plugin-drafts`:
//!
//! **No `draft/*` step is ever followed by an `action/intent` row naming the same audience, and
//! no draft step is `Class::Evidence`.** A draft is the FINISHED act: the absence of an outward
//! act after one is the whole feature, so it is checked against committed rows rather than read
//! off this crate's (empty) call graph.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Class, Ledger, Order, Step, StepQuery, StepType};

const NAME: &str = "a_draft_is_never_followed_by_an_act_on_the_same_audience";

/// The step kinds this check reads.
fn kinds() -> Vec<StepType> {
    vec![
        StepType::new(crate::DRAFT_MESSAGE),
        StepType::new(crate::DRAFT_TICKET),
        StepType::new("action/intent"),
    ]
}

/// PURE: the check over one trajectory's rows, in seq order.
pub fn check_steps(steps: &[Step]) -> Result<(), String> {
    let mut drafted: Vec<(String, String)> = Vec::new(); // (audience, step id)
    for step in steps {
        match step.kind.as_str() {
            crate::DRAFT_MESSAGE | crate::DRAFT_TICKET => {
                // P6-D4: a draft is the agent's own composition, never a truth claim.
                if step.class == Class::Evidence {
                    return Err(format!(
                        "draft step `{}` is evidence; a draft is a thought (P6-D4)",
                        step.id
                    ));
                }
                if let Some(audience) = step.body.get("audience").and_then(|v| v.as_str()) {
                    drafted.push((audience.to_string(), step.id.to_string()));
                }
            }
            "action/intent" => {
                let target = step
                    .body
                    .get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                // The audience is free text (`slack:#eng`) and a canonical action target is not,
                // so the match is by CONTAINMENT either way: a target that merely mentions the
                // drafted audience is exactly the confusion worth failing on.
                if let Some((audience, draft)) = drafted.iter().find(|(a, _)| {
                    !a.is_empty() && (target.contains(a.as_str()) || a.contains(target))
                }) {
                    return Err(format!(
                        "draft step `{draft}` addressed `{audience}` and action/intent step `{}` \
                         then acted on `{target}`: a draft is the finished act (§7)",
                        step.id
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        return Ok(());
    };
    let agents = ledger.0.agents().await.map_err(|e| fail(e.to_string()))?;
    for agent in agents {
        let steps = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![agent.traj.clone()],
                kinds: kinds(),
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .map_err(|e| fail(e.to_string()))?;
        check_steps(&steps).map_err(fail)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Seq, StepId, TrajId, WakeId};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn step(id: &str, seq: u64, kind: &str, class: Class, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(id),
            traj: TrajId::new("t1"),
            seq: Seq(seq),
            at: chrono::Utc::now(),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class,
            body: Arc::new(body),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    fn draft(id: &str, seq: u64, audience: &str) -> Step {
        step(
            id,
            seq,
            crate::DRAFT_MESSAGE,
            Class::Thought,
            serde_json::json!({ "draft": id, "audience": audience, "subject": "s", "body": "b" }),
        )
    }

    #[test]
    fn a_draft_alone_passes() {
        assert!(check_steps(&[draft("d1", 1, "slack:#eng")]).is_ok());
    }

    #[test]
    fn an_act_on_the_drafted_audience_is_a_violation() {
        let steps = vec![
            draft("d1", 1, "slack:#eng"),
            step(
                "a1",
                2,
                "action/intent",
                Class::Thought,
                serde_json::json!({
                    "action": "act1", "idem_key": "k", "kind": "bot_thread_op",
                    "target": "slack:#eng/1", "payload_digest": "d",
                }),
            ),
        ];
        let err = check_steps(&steps).expect_err("the draft was the finished act");
        assert!(err.contains("slack:#eng"), "{err}");
    }

    /// A sanctioned act on an unrelated target is not the thing this checks.
    #[test]
    fn an_act_on_another_target_passes() {
        let steps = vec![
            draft("d1", 1, "slack:#eng"),
            step(
                "a1",
                2,
                "action/intent",
                Class::Thought,
                serde_json::json!({
                    "action": "act1", "idem_key": "k", "kind": "open_pr",
                    "target": "gh:o/r#12", "payload_digest": "d",
                }),
            ),
        ];
        assert!(check_steps(&steps).is_ok());
    }

    #[test]
    fn a_draft_appended_as_evidence_is_a_violation() {
        let mut d = draft("d1", 1, "slack:#eng");
        d.class = Class::Evidence;
        let err = check_steps(&[d]).expect_err("a draft is a thought");
        assert!(err.contains("evidence"), "{err}");
    }
}
