//! §0.2 runtime invariant for `bough-plugin-hooks-exec`:
//!
//! **No hook point is invoked more than once per dispatch of its event.** A `hook/fired` row cites
//! the step that caused it, so two rows with the same `(point, exec, cited step)` mean the same
//! event drove the same executable twice — which is exactly the retry loop §7 forbids, and the
//! shape a quarantine bug would take.

use std::collections::BTreeSet;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, Order, Step, StepQuery, StepType};

const NAME: &str = "no_hook_point_fires_twice_for_one_event";

/// PURE: the whole check over a trajectory's steps, in seq order.
pub fn evaluate(steps: &[Step]) -> Result<(), String> {
    let mut seen: BTreeSet<(String, String, String)> = BTreeSet::new();
    for step in steps {
        if step.kind.as_str() != crate::HOOK_FIRED {
            continue;
        }
        let Ok(body) = serde_json::from_value::<crate::HookFired>((*step.body).clone()) else {
            return Err(format!("`hook/fired` `{}` has an unreadable body", step.id));
        };
        for cite in step.cites.iter() {
            let key = (
                body.point.clone(),
                body.exec.clone(),
                cite.r#ref.as_str().to_string(),
            );
            if !seen.insert(key) {
                return Err(format!(
                    "hook point `{}` fired `{}` twice for `{}`; a hook is invoked once per \
                     dispatch and a failure is never retried into a loop (§7)",
                    body.point, body.exec, cite.r#ref
                ));
            }
        }
    }
    Ok(())
}

/// The spec [`crate::HooksExecPlugin::invariants`] returns.
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
    let mut trajs = BTreeSet::new();
    for row in ledger.0.agents().await.map_err(|e| fail(e.to_string()))? {
        trajs.insert(row.traj);
    }
    for traj in trajs {
        let steps = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj],
                kinds: vec![StepType::new(crate::HOOK_FIRED)],
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
    use bough_plugin_ledger::{Cite, Class, Ref, Seq, StepId, TrajId, WakeId};
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    fn fired(id: &str, point: &str, exec: &str, cites: &str) -> Step {
        Step {
            id: StepId::new(id),
            traj: TrajId::new("t1"),
            seq: Seq(1),
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            wake: WakeId::new("w1"),
            kind: StepType::new(crate::HOOK_FIRED),
            class: Class::Thought,
            body: Arc::new(
                serde_json::to_value(crate::HookFired {
                    point: point.into(),
                    exec: exec.into(),
                    actions: vec![],
                    outcomes: vec![],
                    ms: 1,
                    ok: true,
                })
                .unwrap(),
            ),
            cites: Arc::new(vec![Cite {
                r#ref: Ref::new(cites),
                url: None,
            }]),
            refs: Arc::new(Default::default()),
            ignorable: true,
        }
    }

    #[test]
    fn one_firing_per_event_holds() {
        assert!(evaluate(&[
            fired("f1", "boot", "/h", "step:s1"),
            fired("f2", "boot", "/h", "step:s2"),
            fired("f3", "mail/delivered", "/h", "step:s1"),
        ])
        .is_ok());
    }

    #[test]
    fn the_same_point_firing_twice_for_one_event_is_a_violation() {
        let err = evaluate(&[
            fired("f1", "boot", "/h", "step:s1"),
            fired("f2", "boot", "/h", "step:s1"),
        ])
        .expect_err("violation");
        assert!(err.contains("twice"), "{err}");
    }
}
