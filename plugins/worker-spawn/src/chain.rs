//! Invariant (§10): what a worker says becomes the SPAWNER's record, split by citation. The
//! report itself is one `worker/report` carrying the union of the report's EXTERNAL cites; every
//! claim whose only citation is the worker's own report becomes one `worker/claim` THOUGHT, so
//! the spawner can never cite a worker's say-so as evidence about the world.
//!
//! Pure: appends in, no store, `now` passed in. The roundtrip test drives it against a real
//! ledger, and this module can be reasoned about without one.

use bough_plugin_ledger::{Append, Class, StepType, TrajId, WakeId};
use bough_plugin_workers::{Report, WorkerClaim, WorkerId, WorkerReport};
use chrono::{DateTime, Utc};

/// The union of every claim's external cites, in first-seen order and without duplicates.
pub use bough_plugin_workers::external_cites_of as external_cites;

/// The steps one finished report contributes to the spawner's chain: the report, then one thought
/// per uncited claim, in the report's own order.
pub fn report_appends(
    worker: &WorkerId,
    traj: &TrajId,
    wake: &WakeId,
    at: DateTime<Utc>,
    report: &Report,
    steps: u32,
) -> Vec<Append> {
    let cites = external_cites(worker, report);
    // A report with no external cite is not evidence ABOUT THE WORLD, and §10 refuses to let one
    // read as such. The ledger would refuse `Evidence` without cites anyway; choosing the class
    // here means the refusal is a decision with a reason rather than an append error.
    let class = if cites.is_empty() {
        Class::Thought
    } else {
        Class::Evidence
    };
    let mut out = vec![Append {
        traj: traj.clone(),
        wake: wake.clone(),
        kind: StepType::new("worker/report"),
        class,
        body: serde_json::to_value(WorkerReport {
            worker: worker.clone(),
            summary: report.summary.clone(),
            claims: report.claims.clone(),
            steps,
        })
        .expect("WorkerReport serialises"),
        cites,
        at,
        id: None,
    }];
    for claim in &report.claims {
        if claim.is_externally_cited(worker) {
            continue;
        }
        out.push(Append {
            traj: traj.clone(),
            wake: wake.clone(),
            kind: StepType::new("worker/claim"),
            class: Class::Thought,
            body: serde_json::to_value(WorkerClaim {
                worker: worker.clone(),
                text: claim.text.clone(),
            })
            .expect("WorkerClaim serialises"),
            cites: Vec::new(),
            at,
            id: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Cite, Ref};
    use bough_plugin_workers::ReportClaim;

    fn cite(r: &str) -> Cite {
        Cite {
            r#ref: Ref::new(r),
            url: None,
        }
    }

    fn report() -> Report {
        Report {
            summary: "edited the file".into(),
            claims: vec![
                ReportClaim {
                    text: "line 3 now reads `x`".into(),
                    cites: vec![cite("step:s1")],
                },
                ReportClaim {
                    text: "it is probably fine".into(),
                    cites: vec![cite("worker:w1")],
                },
                ReportClaim {
                    text: "and also this".into(),
                    cites: vec![],
                },
            ],
        }
    }

    #[test]
    fn the_report_is_evidence_carrying_only_external_cites() {
        let w = WorkerId::new("w1");
        let a = report_appends(
            &w,
            &TrajId::new("t"),
            &WakeId::new("wk"),
            Utc::now(),
            &report(),
            4,
        );
        assert_eq!(a[0].kind.as_str(), "worker/report");
        assert_eq!(a[0].class, Class::Evidence);
        assert_eq!(a[0].cites, vec![cite("step:s1")]);
    }

    /// The two uncited claims become thoughts, in the report's order; the cited one does not.
    #[test]
    fn each_uncited_claim_becomes_one_thought_and_no_more() {
        let w = WorkerId::new("w1");
        let a = report_appends(
            &w,
            &TrajId::new("t"),
            &WakeId::new("wk"),
            Utc::now(),
            &report(),
            4,
        );
        let claims: Vec<String> = a[1..]
            .iter()
            .map(|s| {
                assert_eq!(s.kind.as_str(), "worker/claim");
                assert_eq!(s.class, Class::Thought);
                s.body["text"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(
            claims,
            vec![
                "it is probably fine".to_string(),
                "and also this".to_string()
            ]
        );
    }

    /// A report backed by nothing outside itself is a THOUGHT, not evidence with no cites.
    #[test]
    fn a_report_with_no_external_cite_is_not_evidence() {
        let w = WorkerId::new("w1");
        let r = Report {
            summary: "trust me".into(),
            claims: vec![ReportClaim {
                text: "done".into(),
                cites: vec![cite("worker:w1")],
            }],
        };
        let a = report_appends(&w, &TrajId::new("t"), &WakeId::new("wk"), Utc::now(), &r, 1);
        assert_eq!(a[0].class, Class::Thought);
        assert!(a[0].cites.is_empty());
    }
}
