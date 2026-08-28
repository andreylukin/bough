//! §0.2 runtime invariant for `bough-plugin-collector-linear`:
//!
//! **No two `mail/delivered` steps on one trajectory CITE the same `linear:` ref, and the API key
//! appears in no step this row wrote.** The first is the at-least-once ref guard checked against
//! the ledger rather than documented; the second is P6-D7, checked by scanning what this row
//! delivered for the configured secret.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_collect_core::no_duplicate_cited_ref;
use bough_plugin_ledger::{Ledger, Order, Step, StepQuery};

/// The ref prefix this row delivers under.
pub const PREFIX: &str = "linear:";

const NAME: &str = "no_duplicate_linear_delivery_and_no_leaked_key";

/// PURE: the secret appears in NOTHING this row wrote. An empty key checks nothing (a machine
/// without a Linear key is a supported deployment).
pub fn no_key_in_steps(key: &str, steps: &[Step]) -> Result<(), String> {
    if key.trim().is_empty() {
        return Ok(());
    }
    for step in steps {
        if step.body.to_string().contains(key) {
            return Err(format!(
                "step `{}` carries the Linear API key; the key never appears in a step, a report, \
                 a log line or `--dump-config` (P6-D7)",
                step.id
            ));
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
    // The check needs the secret, and `check` is a fn pointer that captures nothing: the row
    // records the keys it activated with (never their values anywhere else) so the invariant can
    // scan for them.
    let keys = crate::active_keys();
    for row in ledger.0.agents().await.map_err(|e| fail(e.to_string()))? {
        let steps = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![row.traj],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .map_err(|e| fail(e.to_string()))?;
        no_duplicate_cited_ref(PREFIX, &steps).map_err(fail)?;
        for key in &keys {
            no_key_in_steps(key, &steps).map_err(fail)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bough_plugin_ledger::{Class, Ref, Seq, StepId, StepType, TrajId, WakeId};
    use chrono::{DateTime, Utc};

    use super::*;

    fn step(body: serde_json::Value) -> Step {
        Step {
            id: StepId::new("s1"),
            traj: TrajId::new("t1"),
            seq: Seq(1),
            at: DateTime::<Utc>::from_timestamp(0, 0).expect("the epoch"),
            wake: WakeId::new("w1"),
            kind: StepType::new("mail/delivered"),
            class: Class::Evidence,
            body: Arc::new(body),
            cites: Arc::new(Vec::new()),
            refs: Arc::new([Ref::new("linear:T-1")].into_iter().collect()),
            ignorable: false,
        }
    }

    #[test]
    fn a_step_carrying_the_key_is_a_violation() {
        let steps = [step(serde_json::json!({ "summary": "lin_api_SECRET" }))];
        let err = no_key_in_steps("lin_api_SECRET", &steps).expect_err("a leak");
        assert!(err.contains("Linear API key"), "{err}");
        assert!(
            !err.contains("lin_api_SECRET"),
            "the error must not leak it either: {err}"
        );
    }

    #[test]
    fn an_absent_key_checks_nothing() {
        let steps = [step(serde_json::json!({ "summary": "anything" }))];
        assert!(no_key_in_steps("", &steps).is_ok());
    }

    #[test]
    fn clean_mail_passes() {
        let steps = [step(serde_json::json!({ "summary": "a ticket moved" }))];
        assert!(no_key_in_steps("lin_api_SECRET", &steps).is_ok());
    }
}
