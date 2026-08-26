//! Invariant under test (V1): a sweep is ref-guarded THEN watermarked, so a second sweep — and a
//! sweep whose watermark write was LOST — delivers nothing twice; every delivery is EVIDENCE
//! carrying its `gh:` ref; a `deliver_to` naming no live agent is reported every sweep; and one
//! source's unparseable payload leaves the others sweeping.

mod common;

use bough_plugin_ledger::Class;
use common::{at, Fx};

const PR_ARGS: [&str; 8] = [
    "pr",
    "list",
    "--repo",
    "o/r",
    "--json",
    "number,title,url,updatedAt,author,state,isDraft",
    "--limit",
    "50",
];

fn review_args() -> Vec<String> {
    vec![
        "api".to_string(),
        "search/issues".to_string(),
        "-f".to_string(),
        "q=is:open is:pr review-requested:@me repo:o/r".to_string(),
    ]
}

const PRS: &str = r#"[
  {"number":12,"title":"a PR","url":"https://example.invalid/12","updatedAt":"2026-08-01T00:00:00Z",
   "author":{"login":"andrey"},"state":"OPEN","isDraft":false}
]"#;

const REVIEWS: &str = r#"{"items":[
  {"number":4,"title":"please review","updated_at":"2026-08-01T01:00:00Z",
   "html_url":"https://example.invalid/4","user":{"login":"teammate"},"body":"a look?"}
]}"#;

/// The standard two-source fixture set.
fn standard(fx: &Fx) {
    fx.gh_fixture(&PR_ARGS, "json", PRS);
    let args = review_args();
    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    fx.gh_fixture(&argv, "json", REVIEWS);
}

#[tokio::test]
async fn a_sweep_against_the_shim_delivers_cited_mail_to_every_deliver_to_agent() {
    let fx = Fx::new().await;
    standard(&fx);
    let _sol = fx.agent("sol").await;
    let _terra = fx.agent("terra").await;
    let mut cfg = fx.cfg();
    cfg.deliver_to = vec!["sol".to_string(), "terra".to_string()];

    let report = fx.collector(cfg).sweep_at(at()).await.expect("a sweep");

    assert!(report.disabled.is_empty(), "{:?}", report.disabled);
    // Two sources, one item each, two agents.
    let delivered: usize = report.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 4, "{:?}", report.sources);

    for who in ["sol", "terra"] {
        let steps = fx.delivered(who).await;
        assert_eq!(steps.len(), 2, "{who}");
        let classes: Vec<&str> = steps
            .iter()
            .map(|s| s.body["class"].as_str().expect("a class"))
            .collect();
        // A review request is a configured wake class; a PR update never is (§5).
        assert!(classes.contains(&"wake"), "{classes:?}");
        assert!(classes.contains(&"ordinary"), "{classes:?}");
    }
}

#[tokio::test]
async fn every_delivered_step_is_evidence_and_carries_its_gh_ref() {
    let fx = Fx::new().await;
    standard(&fx);
    let _sol = fx.agent("sol").await;

    fx.collector(fx.cfg())
        .sweep_at(at())
        .await
        .expect("a sweep");

    let steps = fx.delivered("sol").await;
    assert_eq!(steps.len(), 2);
    for step in &steps {
        assert_eq!(step.class, Class::Evidence);
        assert!(!step.cites.is_empty(), "cited by construction");
        assert!(
            step.refs.iter().any(|r| r.as_str().starts_with("gh:")),
            "{:?}",
            step.refs
        );
    }
    assert!(steps
        .iter()
        .any(|s| s.refs.iter().any(|r| r.as_str() == "gh:o/r#12")));
    assert!(steps
        .iter()
        .any(|s| s.refs.iter().any(|r| r.as_str() == "gh:o/r#4")));
}

#[tokio::test]
async fn a_second_sweep_over_the_same_fixtures_delivers_nothing() {
    let fx = Fx::new().await;
    standard(&fx);
    let _sol = fx.agent("sol").await;

    let collector = fx.collector(fx.cfg());
    collector.sweep_at(at()).await.expect("the first sweep");
    let second = collector.sweep_at(at()).await.expect("the second sweep");

    let delivered: usize = second.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 0, "{:?}", second.sources);
    assert_eq!(fx.delivered("sol").await.len(), 2);
}

#[tokio::test]
async fn a_lost_watermark_write_still_duplicates_nothing_because_the_ref_guard_runs_first() {
    let fx = Fx::new().await;
    standard(&fx);
    let _sol = fx.agent("sol").await;

    // A snapshot of the watermark file taken BEFORE the sweep, restored after it: exactly what a
    // crash between the delivery and the watermark write leaves behind.
    let snapshot = fx.dir.path().join("state.before");
    fx.collector(fx.cfg()); // create the file
    std::fs::copy(&fx.state_db, &snapshot).expect("a snapshot");

    fx.collector(fx.cfg())
        .sweep_at(at())
        .await
        .expect("a sweep");
    assert_eq!(fx.delivered("sol").await.len(), 2);

    std::fs::copy(&snapshot, &fx.state_db).expect("the watermark write is lost");
    let again = fx
        .collector(fx.cfg())
        .sweep_at(at())
        .await
        .expect("the re-sweep");

    let delivered: usize = again.sources.iter().map(|(_, d, _, _)| d).sum();
    let skipped: usize = again.sources.iter().map(|(_, _, s, _)| s).sum();
    assert_eq!(
        delivered, 0,
        "the ref guard, not the watermark, is the argument"
    );
    assert_eq!(skipped, 2);
    assert_eq!(fx.delivered("sol").await.len(), 2);
}

#[tokio::test]
async fn a_deliver_to_naming_no_live_agent_is_disabled_every_sweep_and_delivers_nothing() {
    let fx = Fx::new().await;
    standard(&fx);
    let mut cfg = fx.cfg();
    cfg.deliver_to = vec!["nobody".to_string()];

    let collector = fx.collector(cfg);
    for _ in 0..2 {
        let report = collector.sweep_at(at()).await.expect("a sweep");
        assert!(report.sources.is_empty());
        assert_eq!(report.disabled.len(), 1, "{:?}", report.disabled);
        assert_eq!(report.disabled[0].0, "deliver_to");
        assert!(report.disabled[0].1.contains("nobody"));
    }
    // Nowhere to deliver ⇒ not one `gh` call was spent.
    assert!(fx.gh_log().is_empty(), "{:?}", fx.gh_log());
}

#[tokio::test]
async fn an_unparseable_payload_fails_that_source_only_and_leaves_the_others_sweeping() {
    let fx = Fx::new().await;
    fx.gh_fixture(&PR_ARGS, "json", "{not json at all");
    let args = review_args();
    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    fx.gh_fixture(&argv, "json", REVIEWS);
    let _sol = fx.agent("sol").await;

    let report = fx
        .collector(fx.cfg())
        .sweep_at(at())
        .await
        .expect("a sweep");

    assert_eq!(report.disabled.len(), 1, "{:?}", report.disabled);
    assert_eq!(report.disabled[0].0, "prs:o/r");
    assert_eq!(report.sources.len(), 1, "{:?}", report.sources);
    assert_eq!(report.sources[0].0, "review_requests:o/r");
    assert_eq!(report.sources[0].1, 1);
    let steps = fx.delivered("sol").await;
    assert_eq!(steps.len(), 1);
    assert!(steps[0].refs.iter().any(|r| r.as_str() == "gh:o/r#4"));
}

#[tokio::test]
async fn an_unplanned_gh_call_is_a_failed_source_not_a_network_request() {
    let fx = Fx::new().await;
    // No fixtures at all: the shim answers every argv with exit 42.
    let _sol = fx.agent("sol").await;
    let report = fx
        .collector(fx.cfg())
        .sweep_at(at())
        .await
        .expect("a sweep");
    assert_eq!(report.sources.len(), 0);
    assert_eq!(report.disabled.len(), 2, "{:?}", report.disabled);
    assert!(report
        .disabled
        .iter()
        .all(|(_, why)| why.contains("no fixture for")));
}

#[tokio::test]
async fn the_status_of_the_last_sweep_is_what_the_sweep_returned() {
    let fx = Fx::new().await;
    standard(&fx);
    let _sol = fx.agent("sol").await;
    let collector = fx.collector(fx.cfg());
    let report = collector.sweep_at(at()).await.expect("a sweep");
    assert_eq!(collector.status(), report);
}

#[tokio::test]
async fn a_prs_ref_carried_for_the_router_is_not_a_delivery_of_that_pr() {
    // A failing check's mail carries `gh:o/r#12` for Phase 5's router. The guard keys on what a
    // step CITES, so the PR itself is still delivered.
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;
    let checks_args = [
        "pr",
        "list",
        "--repo",
        "o/r",
        "--json",
        "number,title,url,statusCheckRollup",
        "--limit",
        "50",
    ];
    fx.gh_fixture(
        &checks_args,
        "json",
        r#"[{"number":12,"title":"a PR","url":"https://example.invalid/12","updatedAt":"2026-07-31T00:00:00Z",
             "statusCheckRollup":[{"name":"test","conclusion":"FAILURE","completedAt":"2026-07-31T00:00:00Z"}]}]"#,
    );
    fx.gh_fixture(&PR_ARGS, "json", PRS);

    let mut cfg = fx.cfg();
    cfg.checks = true;
    cfg.review_requests = false;
    let report = fx.collector(cfg).sweep_at(at()).await.expect("a sweep");

    assert!(report.disabled.is_empty(), "{:?}", report.disabled);
    let delivered: usize = report.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 2, "{:?}", report.sources);
    let cited: Vec<String> = fx
        .delivered("sol")
        .await
        .iter()
        .map(|s| s.cites[0].r#ref.as_str().to_string())
        .collect();
    assert!(cited.contains(&"gh:o/r#12".to_string()), "{cited:?}");
    assert!(
        cited.contains(&"gh:o/r#12:check:test".to_string()),
        "{cited:?}"
    );
}
