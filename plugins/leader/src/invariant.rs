//! §0.2 runtime invariant: **`adoption_names_its_unrouted_step`** — every `mail/adopted` step
//! names a `mail/unrouted` step that exists, and no `mail/unrouted` step is adopted twice. It
//! reads the ledger rather than the leader's own bookkeeping: an adoption that consumed an item
//! nobody can find, or consumed one twice, is exactly the silent double-delivery §5 forbids.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use std::collections::{BTreeMap, BTreeSet};

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Order, StepId, StepQuery, StepType, TrajId};

/// One adoption, as read back off the ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    /// The `mail/adopted` step.
    pub step: StepId,
    /// The `mail/unrouted` step it claims to consume.
    pub unrouted: StepId,
}

/// PURE: the clause, against the unrouted ids the ledger actually holds.
pub fn evaluate(obs: &[Obs], unrouted: &BTreeSet<StepId>) -> Result<(), String> {
    let mut consumed_by: BTreeMap<&StepId, Vec<&StepId>> = BTreeMap::new();
    for o in obs {
        if !unrouted.contains(&o.unrouted) {
            return Err(format!(
                "adoption `{}` names `{}`, which is no unrouted item on the queue: an adoption \
                 that consumed an item nobody can find is a silent delivery",
                o.step, o.unrouted
            ));
        }
        consumed_by.entry(&o.unrouted).or_default().push(&o.step);
    }
    for (item, adoptions) in consumed_by {
        if adoptions.len() > 1 {
            let names: Vec<&str> = adoptions.iter().map(|s| s.as_str()).collect();
            return Err(format!(
                "unrouted item `{item}` was adopted {} times ({}): one item, one adoption (§5)",
                adoptions.len(),
                names.join(", ")
            ));
        }
    }
    Ok(())
}

/// The clause above.
pub fn adoption_names_its_unrouted_step() -> InvariantSpec {
    InvariantSpec {
        name: "adoption_names_its_unrouted_step",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

/// Read the unsorted trajectory and evaluate. The trajectory name comes from the bound `mail`
/// seam, so the check follows a re-configured queue rather than a literal spelled twice.
async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let violation = |detail: String| InvariantViolation {
        invariant: "adoption_names_its_unrouted_step",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let ledger = ctx
        .get::<bough_plugin_ledger::Ledger>()
        .map_err(|e| violation(e.to_string()))?;
    let mail = ctx
        .get::<bough_plugin_mail_router::Mail>()
        .map_err(|e| violation(e.to_string()))?;
    let (obs, unrouted) = read(&ledger, &mail.unsorted_traj())
        .await
        .map_err(|e| violation(e.to_string()))?;
    evaluate(&obs, &unrouted).map_err(violation)
}

/// The ledger read the check is a pure function of.
pub async fn read(
    ledger: &bough_plugin_ledger::LedgerHandle,
    traj: &TrajId,
) -> Result<(Vec<Obs>, BTreeSet<StepId>), bough_plugin_ledger::LedgerError> {
    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![
                StepType::new("mail/unrouted"),
                StepType::new("mail/adopted"),
            ],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?;
    let mut obs = Vec::new();
    let mut unrouted = BTreeSet::new();
    for step in steps {
        if step.kind.as_str() == "mail/unrouted" {
            unrouted.insert(step.id.clone());
        } else if let Some(id) = step.body.get("unrouted").and_then(|v| v.as_str()) {
            obs.push(Obs {
                step: step.id.clone(),
                unrouted: StepId::new(id),
            });
        }
    }
    Ok((obs, unrouted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(step: &str, unrouted: &str) -> Obs {
        Obs {
            step: StepId::new(step),
            unrouted: StepId::new(unrouted),
        }
    }

    fn queue(ids: &[&str]) -> BTreeSet<StepId> {
        ids.iter().map(StepId::new).collect()
    }

    #[test]
    fn a_clean_stream_passes() {
        evaluate(
            &[obs("a1", "u1"), obs("a2", "u2")],
            &queue(&["u1", "u2", "u3"]),
        )
        .expect("two adoptions of two distinct items");
    }

    #[test]
    fn an_adoption_of_a_step_that_is_not_on_the_queue_is_reported() {
        let err =
            evaluate(&[obs("a1", "u9")], &queue(&["u1"])).expect_err("`u9` is no unrouted item");
        assert!(err.contains("nobody can find"), "{err}");
    }

    #[test]
    fn one_item_adopted_twice_is_reported() {
        let err = evaluate(&[obs("a1", "u1"), obs("a2", "u1")], &queue(&["u1"]))
            .expect_err("one item, two adoptions");
        assert!(err.contains("adopted 2 times"), "{err}");
    }
}
