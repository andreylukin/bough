//! §0.2 runtime invariant for `bough-plugin-wards-rhai`:
//!
//! **Every `ward/fired` step accounts for itself**: it cites the step it fired on, it carries one
//! outcome line per action it returned, and it never fires on another `ward/fired`.
//!
//! Purity of the script, checked against the JOURNAL rather than trusted. An action executed
//! without a recorded outcome, or an outcome with no action, would mean the executor and the
//! journal had drifted — and the whole reason `evaluate` is pure is so they cannot.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, Order, Step, StepQuery, StepType};

use crate::vocabulary::{WardFired, WARD_FIRED};

const NAME: &str = "ward_firings_cite_their_step_and_account_for_every_action";

/// The whole invariant as a pure function of the `ward/fired` steps and the steps they name.
pub fn evaluate(steps: &[Step]) -> Result<(), String> {
    for step in steps {
        if step.kind.as_str() != WARD_FIRED {
            continue;
        }
        let body: WardFired = serde_json::from_value((*step.body).clone())
            .map_err(|e| format!("`ward/fired` `{}` has an unreadable body: {e}", step.id))?;
        if body.actions.len() != body.outcomes.len() {
            return Err(format!(
                "`ward/fired` `{}` returned {} actions and recorded {} outcomes; every action the \
                 executor ran is accounted for",
                step.id,
                body.actions.len(),
                body.outcomes.len()
            ));
        }
        if step.cites.is_empty() {
            return Err(format!(
                "`ward/fired` `{}` cites nothing; a firing names the step it fired on",
                step.id
            ));
        }
        let on = steps.iter().find(|s| s.seq == body.on);
        if let Some(on) = on {
            if on.kind.as_str() == WARD_FIRED {
                return Err(format!(
                    "ward `{}` fired on another ward's firing (`{}`); a ward never sees the ward \
                     journal",
                    body.ward, on.id
                ));
            }
        }
    }
    Ok(())
}

/// The spec `WardHostPlugin::invariants` returns.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }]
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        // The row is being torn down: there is nothing to state about a ledger that is gone.
        return Ok(());
    };
    let mut trajs = std::collections::BTreeSet::new();
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
    // A firing whose trajectory has no agent row is still a firing: check those too, by kind.
    let firings = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new(WARD_FIRED)],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .map_err(|e| fail(e.to_string()))?;
    evaluate(&firings).map_err(fail)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Cite, Class, Ref, Seq, StepId, TrajId, WakeId};
    use bough_plugin_runtime_actions::RuntimeAction;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    fn firing(body: WardFired, cites: Vec<&str>) -> Step {
        Step {
            id: StepId::new("f1"),
            traj: TrajId::new("t1"),
            seq: Seq(2),
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            wake: WakeId::new("w1"),
            kind: StepType::new(WARD_FIRED),
            class: Class::Thought,
            body: Arc::new(serde_json::to_value(body).unwrap()),
            cites: Arc::new(
                cites
                    .into_iter()
                    .map(|c| Cite {
                        r#ref: Ref::new(c),
                        url: None,
                    })
                    .collect(),
            ),
            refs: Default::default(),
            ignorable: true,
        }
    }

    fn hint() -> RuntimeAction {
        RuntimeAction::Hint {
            agent: "sol".into(),
            text: "x".into(),
        }
    }

    #[test]
    fn a_firing_with_one_outcome_per_action_holds() {
        let step = firing(
            WardFired {
                ward: "reviews".into(),
                on: Seq(1),
                actions: vec![hint()],
                outcomes: vec!["did: hinted `sol`".into()],
                ops: 12,
                ms: 1,
            },
            vec!["step:s1"],
        );
        assert_eq!(evaluate(&[step]), Ok(()));
    }

    #[test]
    fn an_action_with_no_outcome_is_a_violation() {
        let step = firing(
            WardFired {
                ward: "reviews".into(),
                on: Seq(1),
                actions: vec![hint(), hint()],
                outcomes: vec!["did: hinted `sol`".into()],
                ops: 12,
                ms: 1,
            },
            vec!["step:s1"],
        );
        let e = evaluate(&[step]).unwrap_err();
        assert!(e.contains("accounted for"), "{e}");
    }

    #[test]
    fn a_firing_that_cites_nothing_is_a_violation() {
        let step = firing(
            WardFired {
                ward: "reviews".into(),
                on: Seq(1),
                actions: vec![],
                outcomes: vec![],
                ops: 1,
                ms: 0,
            },
            vec![],
        );
        assert!(evaluate(&[step]).unwrap_err().contains("cites nothing"));
    }
}
