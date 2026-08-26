//! Invariant: detection is PURE and the model never widens it. Pairing is arithmetic over refs;
//! staleness is arithmetic over an INJECTED `now`; and [`stale`] never returns a kind in
//! [`bough_plugin_rollups::NEVER_EXPIRABLE`], whatever the config says (V7).

use std::collections::BTreeSet;

use bough_plugin_ledger::vocabulary::ClaimProposed;
use bough_plugin_ledger::{Cite, Class, Ref, Step};
use chrono::{DateTime, Utc};

use crate::{Candidate, Pair, ReconConfig, StaleReason};

/// Evidence steps sharing a ref, newest-vs-older, capped and deterministic.
///
/// The input order is irrelevant: the run is sorted by `seq` first, so a shuffled query result
/// pairs exactly as an ordered one does.
pub fn pairs(steps: &[Step], max: usize) -> Vec<Pair> {
    let mut evidence: Vec<&Step> = steps
        .iter()
        .filter(|s| s.class == Class::Evidence)
        .collect();
    // `(seq, id)`: seq alone is unique per trajectory, but a pass may read more than one.
    evidence.sort_by(|a, b| a.seq.cmp(&b.seq).then_with(|| a.id.cmp(&b.id)));

    let mut out = Vec::new();
    // Newest first, and for each newest its nearest older partner first: a cap therefore keeps
    // the pairs most likely to matter rather than an arbitrary prefix of the cartesian product.
    for j in (0..evidence.len()).rev() {
        for i in (0..j).rev() {
            let shared: Vec<Ref> = evidence[j]
                .refs
                .intersection(&evidence[i].refs)
                .cloned()
                .collect();
            if shared.is_empty() {
                continue;
            }
            out.push(Pair {
                older: evidence[i].id.clone(),
                newer: evidence[j].id.clone(),
                shared,
            });
            if out.len() == max {
                return out;
            }
        }
    }
    out
}

/// Stale by age. Never returns a `NEVER_EXPIRABLE` kind, whatever the config says.
pub fn stale(steps: &[Step], now: DateTime<Utc>, cfg: &ReconConfig) -> Vec<Candidate> {
    // The runtime lock, second after `resolve::validate`: a config that names a pin kind is
    // refused at boot, and were it ever to reach here it would still expire nothing (V7).
    let allowed: BTreeSet<&str> = cfg
        .expirable_kinds
        .iter()
        .map(String::as_str)
        .filter(|k| !bough_plugin_rollups::NEVER_EXPIRABLE.contains(k))
        .collect();

    let mut out: Vec<Candidate> = steps
        .iter()
        .filter(|s| allowed.contains(s.kind.as_str()))
        .filter_map(|s| {
            let age_days = (now - s.at).num_days();
            (age_days >= cfg.stale_after_days).then(|| Candidate {
                step: s.id.clone(),
                kind: s.kind.clone(),
                age_days,
                why: StaleReason::Age,
            })
        })
        .collect();
    // Oldest first, then by id: deterministic under any input order.
    out.sort_by(|a, b| {
        b.age_days
            .cmp(&a.age_days)
            .then_with(|| a.step.cmp(&b.step))
    });
    out
}

/// The `claim` id a contradiction carries. Deterministic in the pair, so the same contradiction
/// found twice is recognisably the same claim.
pub fn claim_id(pair: &Pair) -> String {
    format!("contradiction:{}:{}", pair.older, pair.newer)
}

/// The `claim/proposed` body for a judged contradiction.
///
/// Cites BOTH steps, so the claim is evidence-backed the moment it is appended.
pub fn contradiction_claim(pair: &Pair, verdict: &str) -> (ClaimProposed, Vec<Cite>) {
    let shared = pair
        .shared
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    (
        ClaimProposed {
            claim: claim_id(pair),
            kind: "contradiction".to_string(),
            title: format!("two evidence steps disagree about {shared}"),
            body: verdict.trim().to_string(),
        },
        vec![
            Cite {
                r#ref: Ref::new(format!("step:{}", pair.older)),
                url: None,
            },
            Cite {
                r#ref: Ref::new(format!("step:{}", pair.newer)),
                url: None,
            },
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Seq, StepId, StepType, TrajId, WakeId};
    use chrono::TimeZone;
    use std::sync::Arc;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    fn step(id: &str, seq: u64, kind: &str, class: Class, refs: &[&str], days: i64) -> Step {
        Step {
            id: StepId::new(id),
            traj: TrajId::new("t1"),
            seq: Seq(seq),
            at: t0() + chrono::Duration::days(days),
            wake: WakeId::new("w"),
            kind: StepType::new(kind),
            class,
            body: Arc::new(serde_json::json!({})),
            cites: Arc::new(vec![]),
            refs: Arc::new(refs.iter().map(Ref::new).collect()),
            ignorable: false,
        }
    }

    fn cfg() -> ReconConfig {
        ReconConfig {
            batch_steps: 400,
            stale_after_days: 90,
            expirable_kinds: vec!["mail/delivered".into(), "tool/result".into()],
            max_contradiction_pairs: 24,
            max_calls_per_pass: 6,
            distill_max_tokens: 2048,
        }
    }

    #[test]
    fn pairs_are_evidence_steps_sharing_a_ref() {
        let steps = vec![
            step("a", 1, "tool/result", Class::Evidence, &["gh:o/r#1"], 0),
            step("b", 2, "tool/result", Class::Evidence, &["gh:o/r#1"], 1),
            // Evidence, but shares nothing.
            step("c", 3, "tool/result", Class::Evidence, &["gh:o/r#9"], 2),
            // Shares the ref, but is a THOUGHT: never a contradiction candidate.
            step("d", 4, "thought/text", Class::Thought, &["gh:o/r#1"], 3),
        ];
        let got = pairs(&steps, 24);
        assert_eq!(
            got.len(),
            1,
            "exactly one evidence pair shares a ref: {got:?}"
        );
        assert_eq!(got[0].older, StepId::new("a"));
        assert_eq!(got[0].newer, StepId::new("b"));
        assert_eq!(got[0].shared, vec![Ref::new("gh:o/r#1")]);
    }

    #[test]
    fn pairs_are_capped_and_deterministic() {
        let steps: Vec<Step> = (1..=8)
            .map(|i| {
                step(
                    &format!("s{i}"),
                    i,
                    "tool/result",
                    Class::Evidence,
                    &["gh:o/r#1"],
                    i as i64,
                )
            })
            .collect();
        // 8 steps all sharing one ref is 28 pairs; the cap is what a pass actually judges.
        let capped = pairs(&steps, 5);
        assert_eq!(capped.len(), 5);
        assert_eq!(
            capped,
            pairs(&steps, 5),
            "the same input pairs the same way"
        );

        let mut shuffled = steps.clone();
        shuffled.reverse();
        assert_eq!(
            pairs(&shuffled, 5),
            capped,
            "input order must not move the answer"
        );
        // The newest step is in the first pair: the cap keeps the freshest disagreements.
        assert_eq!(capped[0].newer, StepId::new("s8"));
    }

    #[test]
    fn stale_never_returns_a_pin_whatever_the_config_says() {
        let mut c = cfg();
        // A misconfiguration that `resolve::validate` refuses at boot; the runtime lock stands
        // anyway, so a config reaching here by any route still expires no pin (V7).
        c.expirable_kinds = vec!["pin/set".into(), "pin/retire".into(), "tool/result".into()];
        let steps = vec![
            step("p", 1, "pin/set", Class::Evidence, &[], 0),
            step("q", 2, "pin/retire", Class::Thought, &[], 0),
            step("r", 3, "tool/result", Class::Evidence, &[], 0),
        ];
        let got = stale(&steps, t0() + chrono::Duration::days(365), &c);
        assert_eq!(
            got.len(),
            1,
            "only the expirable kind is a candidate: {got:?}"
        );
        assert_eq!(got[0].step, StepId::new("r"));
    }

    #[test]
    fn stale_never_returns_a_claim() {
        let mut c = cfg();
        c.expirable_kinds = vec![
            "claim/proposed".into(),
            "claim/accepted".into(),
            "claim/rejected".into(),
        ];
        let steps = vec![
            step("a", 1, "claim/proposed", Class::Thought, &[], 0),
            step("b", 2, "claim/accepted", Class::Evidence, &[], 0),
            step("c", 3, "claim/rejected", Class::Thought, &[], 0),
        ];
        assert_eq!(
            stale(&steps, t0() + chrono::Duration::days(365), &c),
            vec![]
        );
    }

    #[test]
    fn age_is_measured_against_the_injected_now() {
        let c = cfg();
        let steps = vec![step("a", 1, "tool/result", Class::Evidence, &[], 0)];
        // A day short of the threshold: not stale, whatever the wall clock says.
        assert_eq!(stale(&steps, t0() + chrono::Duration::days(89), &c), vec![]);
        let got = stale(&steps, t0() + chrono::Duration::days(90), &c);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].age_days, 90);
        assert_eq!(got[0].why, StaleReason::Age);
        // And a `now` BEFORE the step is a negative age, never a candidate.
        assert_eq!(stale(&steps, t0() - chrono::Duration::days(1), &c), vec![]);
    }

    #[test]
    fn a_contradiction_claim_cites_both_steps() {
        let pair = Pair {
            older: StepId::new("a"),
            newer: StepId::new("b"),
            shared: vec![Ref::new("gh:o/r#1")],
        };
        let (claim, cites) = contradiction_claim(&pair, "  they disagree  ");
        assert_eq!(claim.kind, "contradiction");
        assert_eq!(claim.body, "they disagree");
        assert_eq!(
            cites
                .iter()
                .map(|c| c.r#ref.to_string())
                .collect::<Vec<_>>(),
            vec!["step:a".to_string(), "step:b".to_string()]
        );
    }
}
