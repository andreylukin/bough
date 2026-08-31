//! Invariant under test: a sweep against the stub Slack MCP server delivers cited mail with
//! `slack:` refs and NO credential anywhere in this process; a second sweep delivers nothing
//! (watermark + ref guard); an empty `queries` map, a missing server row and a drifted rendering
//! are each a LOUD per-sweep report, never a silent empty sweep.

use crate::common;

use bough_plugin_ledger::Class;
use common::{at, Fx, Mode};

#[tokio::test]
async fn a_sweep_against_the_stub_delivers_cited_mail() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;
    let (collector, stub) = fx.collector(fx.cfg(), Mode::Ok).await;

    let report = collector.sweep_at(at()).await.expect("a sweep");

    assert!(report.disabled.is_empty(), "{:?}", report.disabled);
    let delivered: usize = report.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 2, "{:?}", report.sources);

    let steps = fx.delivered("sol").await;
    assert_eq!(steps.len(), 2);
    for step in &steps {
        assert_eq!(step.class, Class::Evidence);
        assert!(!step.cites.is_empty(), "cited by construction");
        assert!(step.refs.iter().any(|r| r.as_str().starts_with("slack:")));
        // Mention is a configured wake class, and every query's items are mentions.
        assert_eq!(step.body["class"].as_str(), Some("wake"));
    }

    // The call is ascending, detailed and bounded to the server's page cap.
    let calls = stub.calls.lock().clone();
    assert_eq!(calls.len(), 1);
    let (tool, args) = &calls[0];
    assert_eq!(tool, "slack_search_public_and_private");
    assert_eq!(args["query"], "to:me");
    assert_eq!(args["sort_dir"], "asc");
    assert_eq!(args["limit"], 20);
    assert!(args.get("after").is_none(), "no watermark on a first sweep");
}

#[tokio::test]
async fn a_second_sweep_delivers_nothing_and_carries_the_watermark() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;
    let (collector, stub) = fx.collector(fx.cfg(), Mode::Ok).await;

    collector.sweep_at(at()).await.expect("the first sweep");
    let second = collector.sweep_at(at()).await.expect("the second sweep");

    let delivered: usize = second.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 0, "{:?}", second.sources);
    assert_eq!(fx.delivered("sol").await.len(), 2);

    // The second call's `after` is the newest delivered ts in SECONDS.
    let calls = stub.calls.lock().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].1["after"], "1787946300", "{:?}", calls[1].1);
}

#[tokio::test]
async fn an_empty_queries_map_is_a_loud_disabled_report() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;
    let mut cfg = fx.cfg();
    cfg.queries.clear();
    let (collector, stub) = fx.collector(cfg, Mode::Ok).await;

    let report = collector.sweep_at(at()).await.expect("the sweep survives");
    assert!(report.sources.is_empty());
    assert_eq!(report.disabled.len(), 1, "{:?}", report.disabled);
    assert_eq!(report.disabled[0].0, "queries");
    assert!(stub.calls.lock().is_empty(), "no call was made");
}

#[tokio::test]
async fn a_missing_server_row_and_a_drifted_rendering_are_loud_failed_sources() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;

    let collector = fx.collector_without_server(fx.cfg());
    let report = collector.sweep_at(at()).await.expect("the sweep survives");
    assert_eq!(report.disabled.len(), 1, "{:?}", report.disabled);
    assert!(
        report.disabled[0].1.contains("slack"),
        "{:?}",
        report.disabled
    );

    let (collector, _stub) = fx.collector(fx.cfg(), Mode::Drifted).await;
    let report = collector.sweep_at(at()).await.expect("the sweep survives");
    assert_eq!(report.disabled.len(), 1, "{:?}", report.disabled);
    assert!(
        report.disabled[0].1.contains("drifted"),
        "{:?}",
        report.disabled
    );

    let (collector, _stub) = fx.collector(fx.cfg(), Mode::ToolError).await;
    let report = collector.sweep_at(at()).await.expect("the sweep survives");
    assert!(
        report.disabled[0].1.contains("answered an error"),
        "{:?}",
        report.disabled
    );
    assert!(fx.delivered("sol").await.is_empty());
}

#[tokio::test]
async fn a_lost_watermark_write_still_duplicates_nothing() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;

    let snapshot = fx.dir.path().join("state.before");
    let (collector, _stub) = fx.collector(fx.cfg(), Mode::Ok).await;
    drop(collector);
    std::fs::copy(&fx.state_db, &snapshot).expect("a snapshot");

    let (collector, _stub) = fx.collector(fx.cfg(), Mode::Ok).await;
    collector.sweep_at(at()).await.expect("a sweep");
    drop(collector);
    std::fs::copy(&snapshot, &fx.state_db).expect("the watermark write is lost");

    let (collector, _stub) = fx.collector(fx.cfg(), Mode::Ok).await;
    let again = collector.sweep_at(at()).await.expect("the re-sweep");

    let delivered: usize = again.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(
        delivered, 0,
        "the ref guard, not the watermark, is the argument"
    );
    assert_eq!(fx.delivered("sol").await.len(), 2);
}
