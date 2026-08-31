//! §0.2 runtime invariant for `bough-plugin-old-feed-adapter`:
//!
//! **No step this row appends carries a `cmd:` / `bough:command:` ref, and no `mail/delivered`
//! step exists with two identical `jungler:event:` refs.** The first half asserts §14's rule that
//! command memory is priming and never mail; the second is the at-least-once ref guard, checked
//! against the ledger rather than documented.

use std::collections::{BTreeMap, BTreeSet};

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, Order, Step, StepQuery};

/// The prefixes a step may never carry. Command memory is competence memory: it is queried, never
/// delivered, so a `cmd:` ref anywhere in the ledger means someone made it model-visible.
pub const FORBIDDEN_PREFIXES: [&str; 2] = ["cmd:", "bough:command:"];

/// The prefix a delivered jungler event cites.
pub const EVENT_PREFIX: &str = "jungler:event:";

/// PURE: the check, over the steps this row appended.
pub fn check_steps(appended: &[Step]) -> Result<(), String> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for step in appended {
        for r in step.refs.iter() {
            let r = r.as_str();
            if let Some(bad) = FORBIDDEN_PREFIXES.iter().find(|p| r.starts_with(**p)) {
                return Err(format!(
                    "step `{}` ({}) carries `{r}`; a `{bad}` ref is command memory, which is \
                     PRIMING and never mail (§14)",
                    step.id, step.kind
                ));
            }
        }
        if step.kind.as_str() != "mail/delivered" {
            continue;
        }
        // Per STEP, not per ref: one step citing one event twice is the same fact once.
        let events: BTreeSet<&str> = step
            .refs
            .iter()
            .map(|r| r.as_str())
            .filter(|r| r.starts_with(EVENT_PREFIX))
            .collect();
        for e in events {
            if let Some(first) = seen.insert(e.to_string(), step.id.to_string()) {
                return Err(format!(
                    "`{e}` is delivered twice: steps `{first}` and `{}`; the ref guard exists so \
                     a restart duplicates nothing (§14)",
                    step.id
                ));
            }
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "no_command_ref_and_no_duplicate_jungler_event",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: "no_command_ref_and_no_duplicate_jungler_event",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        // The row is being torn down: there is nothing to state about a ledger that is gone.
        return Ok(());
    };
    // Delivery is per agent, and so is duplication: two agents legitimately receive the same
    // event, and only a second copy on ONE chain is a violation.
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
        check_steps(&steps).map_err(fail)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, Ref, Seq, StepId, StepType, TrajId, WakeId};
    use chrono::{DateTime, Utc};
    use std::sync::Arc;

    fn step(id: &str, kind: &str, refs: &[&str]) -> Step {
        Step {
            id: StepId::new(id),
            traj: TrajId::new("t1"),
            seq: Seq(1),
            at: DateTime::<Utc>::from_timestamp(0, 0).expect("the epoch"),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class: Class::Evidence,
            body: Arc::new(serde_json::json!({})),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(refs.iter().map(Ref::new).collect()),
            ignorable: false,
        }
    }

    #[test]
    fn clean_delivered_mail_passes() {
        let steps = [
            step("s1", "mail/delivered", &["jungler:event:1"]),
            step("s2", "mail/delivered", &["jungler:event:2"]),
        ];
        assert!(check_steps(&steps).is_ok());
    }

    #[test]
    fn a_command_ref_is_a_violation() {
        let steps = [step("s1", "mail/delivered", &["cmd:cargo test"])];
        let err = check_steps(&steps).expect_err("a `cmd:` ref is never mail");
        assert!(err.contains("PRIMING"), "{err}");
    }

    #[test]
    fn the_same_event_delivered_twice_is_a_violation() {
        let steps = [
            step("s1", "mail/delivered", &["jungler:event:7"]),
            step("s2", "mail/delivered", &["jungler:event:7"]),
        ];
        let err = check_steps(&steps).expect_err("the ref guard should have dropped the second");
        assert!(err.contains("delivered twice"), "{err}");
    }

    #[test]
    fn one_step_citing_one_event_once_is_not_a_duplicate() {
        let steps = [step(
            "s1",
            "mail/delivered",
            &["jungler:event:7", "gh:bough/rebuild#4"],
        )];
        assert!(check_steps(&steps).is_ok());
    }
}
