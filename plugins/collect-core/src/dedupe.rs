//! Invariant: **no two `mail/delivered` steps on one trajectory CITE the same collected ref.**
//! Both collectors' `invariant.rs` wire this same pure check with their own prefix, so the
//! at-least-once ref guard is checked against the ledger rather than documented.
//!
//! It keys on what a step CITES, not on its `refs`: a check-run mail legitimately carries its
//! PR's ref for the router, and that is not a second delivery of the PR.

use std::collections::BTreeMap;

use bough_plugin_ledger::Step;

/// PURE: the check, over the steps of ONE trajectory in seq order.
pub fn no_duplicate_cited_ref(prefix: &str, steps: &[Step]) -> Result<(), String> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for step in steps {
        if step.kind.as_str() != "mail/delivered" {
            continue;
        }
        for cite in step.cites.iter() {
            let r = cite.r#ref.as_str();
            if !r.starts_with(prefix) {
                continue;
            }
            if let Some(first) = seen.insert(r.to_string(), step.id.to_string()) {
                return Err(format!(
                    "`{r}` is delivered twice: steps `{first}` and `{}`; the ref guard exists so \
                     a restart duplicates nothing (§6)",
                    step.id
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bough_plugin_ledger::{Cite, Class, Ref, Seq, Step, StepId, StepType, TrajId, WakeId};
    use chrono::{DateTime, Utc};

    use super::*;

    fn step(id: &str, cites: &[&str], refs: &[&str]) -> Step {
        Step {
            id: StepId::new(id),
            traj: TrajId::new("t1"),
            seq: Seq(1),
            at: DateTime::<Utc>::from_timestamp(0, 0).expect("the epoch"),
            wake: WakeId::new("w1"),
            kind: StepType::new("mail/delivered"),
            class: Class::Evidence,
            body: Arc::new(serde_json::json!({})),
            cites: Arc::new(
                cites
                    .iter()
                    .map(|r| Cite {
                        r#ref: Ref::new(*r),
                        url: None,
                    })
                    .collect(),
            ),
            refs: Arc::new(refs.iter().map(Ref::new).collect()),
            ignorable: false,
        }
    }

    #[test]
    fn distinct_deliveries_pass() {
        let steps = [
            step("s1", &["gh:o/r#12"], &["gh:o/r#12"]),
            step("s2", &["gh:o/r#13"], &["gh:o/r#13"]),
        ];
        assert!(no_duplicate_cited_ref("gh:", &steps).is_ok());
    }

    #[test]
    fn the_same_ref_cited_twice_is_a_violation() {
        let steps = [
            step("s1", &["gh:o/r#12"], &[]),
            step("s2", &["gh:o/r#12"], &[]),
        ];
        let err =
            no_duplicate_cited_ref("gh:", &steps).expect_err("the guard should have caught it");
        assert!(err.contains("delivered twice"), "{err}");
    }

    #[test]
    fn a_router_ref_is_not_a_second_delivery() {
        // A failing check cites the CHECK and carries the PR's ref for the router.
        let steps = [
            step("s1", &["gh:o/r#12"], &["gh:o/r#12"]),
            step(
                "s2",
                &["gh:o/r#12:check:test"],
                &["gh:o/r#12:check:test", "gh:o/r#12"],
            ),
        ];
        assert!(no_duplicate_cited_ref("gh:", &steps).is_ok());
    }

    #[test]
    fn another_collectors_refs_are_not_this_ones_business() {
        let steps = [
            step("s1", &["linear:T-1"], &[]),
            step("s2", &["linear:T-1"], &[]),
        ];
        assert!(no_duplicate_cited_ref("gh:", &steps).is_ok());
    }
}
