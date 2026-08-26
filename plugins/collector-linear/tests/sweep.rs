//! Invariant under test (V1, P6-D7): a sweep against the local stub delivers cited mail, a second
//! sweep delivers nothing, and the API KEY appears in nothing but the `Authorization` header —
//! not in the report, not in an error, not in the `Debug` rendering of the config.

mod common;

use bough_plugin_ledger::Class;
use common::{at, Fx, Mode, Stub, KEY};

#[tokio::test]
async fn a_sweep_against_the_stub_delivers_cited_mail() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Ok).await;
    let _sol = fx.agent("sol").await;

    let report = fx
        .collector(fx.cfg(&stub.url))
        .sweep_at(at())
        .await
        .expect("a sweep");

    assert!(report.disabled.is_empty(), "{:?}", report.disabled);
    let delivered: usize = report.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 2, "{:?}", report.sources);

    let steps = fx.delivered("sol").await;
    assert_eq!(steps.len(), 2);
    for step in &steps {
        assert_eq!(step.class, Class::Evidence);
        assert!(!step.cites.is_empty(), "cited by construction");
        assert!(step.refs.iter().any(|r| r.as_str().starts_with("linear:")));
    }
    // The issue is a configured wake class; a comment is not (§5).
    let classes: Vec<&str> = steps
        .iter()
        .map(|s| s.body["class"].as_str().expect("a class"))
        .collect();
    assert!(classes.contains(&"wake"), "{classes:?}");
    assert!(classes.contains(&"ordinary"), "{classes:?}");
}

#[tokio::test]
async fn a_second_sweep_over_the_same_stub_state_delivers_nothing() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Ok).await;
    let _sol = fx.agent("sol").await;

    let collector = fx.collector(fx.cfg(&stub.url));
    collector.sweep_at(at()).await.expect("the first sweep");
    let second = collector.sweep_at(at()).await.expect("the second sweep");

    let delivered: usize = second.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 0, "{:?}", second.sources);
    assert_eq!(fx.delivered("sol").await.len(), 2);
}

#[tokio::test]
async fn a_lost_watermark_write_still_duplicates_nothing() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Ok).await;
    let _sol = fx.agent("sol").await;

    let snapshot = fx.dir.path().join("state.before");
    fx.collector(fx.cfg(&stub.url));
    std::fs::copy(&fx.state_db, &snapshot).expect("a snapshot");

    fx.collector(fx.cfg(&stub.url))
        .sweep_at(at())
        .await
        .expect("a sweep");
    std::fs::copy(&snapshot, &fx.state_db).expect("the watermark write is lost");

    let again = fx
        .collector(fx.cfg(&stub.url))
        .sweep_at(at())
        .await
        .expect("the re-sweep");

    let delivered: usize = again.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(
        delivered, 0,
        "the ref guard, not the watermark, is the argument"
    );
    assert_eq!(fx.delivered("sol").await.len(), 2);
}

#[tokio::test]
async fn the_key_travels_in_the_header_and_nowhere_else() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Ok).await;
    let _sol = fx.agent("sol").await;
    let cfg = fx.cfg(&stub.url);

    let report = fx
        .collector(cfg.clone())
        .sweep_at(at())
        .await
        .expect("a sweep");

    let requests = stub.requests();
    assert!(!requests.is_empty());
    for (auth, body) in &requests {
        assert_eq!(auth, KEY, "the key rides the Authorization header");
        assert!(!body.contains(KEY), "and never the request body");
    }
    assert!(!format!("{report:?}").contains(KEY), "not in the report");
    assert!(
        !format!("{cfg:?}").contains(KEY),
        "not in the debug rendering"
    );
    assert!(format!("{cfg:?}").contains("<redacted>"));
    for step in fx.delivered("sol").await {
        assert!(!step.body.to_string().contains(KEY), "not in a step");
    }
}

#[tokio::test]
async fn an_absent_key_disables_the_row_loudly_every_sweep_and_never_fails_the_boot() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Ok).await;
    let _sol = fx.agent("sol").await;
    let mut cfg = fx.cfg(&stub.url);
    cfg.api_key = String::new();

    let collector = fx.collector(cfg);
    for _ in 0..2 {
        let report = collector.sweep_at(at()).await.expect("a sweep");
        assert!(report.sources.is_empty());
        assert_eq!(report.disabled.len(), 1, "{:?}", report.disabled);
        assert_eq!(report.disabled[0].0, "api_key");
    }
    assert!(stub.requests().is_empty(), "no key, no request");
    assert!(fx.delivered("sol").await.is_empty());
}

#[tokio::test]
async fn a_deliver_to_naming_no_live_agent_is_disabled_every_sweep_and_delivers_nothing() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Ok).await;
    let mut cfg = fx.cfg(&stub.url);
    cfg.deliver_to = vec!["nobody".to_string()];

    let collector = fx.collector(cfg);
    for _ in 0..2 {
        let report = collector.sweep_at(at()).await.expect("a sweep");
        assert!(report.sources.is_empty());
        assert_eq!(report.disabled[0].0, "deliver_to");
        assert!(report.disabled[0].1.contains("nobody"));
    }
    assert!(
        stub.requests().is_empty(),
        "nowhere to deliver, so nothing is fetched"
    );
}

#[tokio::test]
async fn an_unauthorized_endpoint_fails_the_sources_without_quoting_the_key() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Unauthorized).await;
    let _sol = fx.agent("sol").await;

    let report = fx
        .collector(fx.cfg(&stub.url))
        .sweep_at(at())
        .await
        .expect("a sweep that reports rather than throws");

    assert_eq!(report.disabled.len(), 2, "{:?}", report.disabled);
    assert!(report.disabled.iter().all(|(_, why)| why.contains("401")));
    assert!(!format!("{report:?}").contains(KEY));
}

#[tokio::test]
async fn an_unparseable_payload_fails_the_source_rather_than_the_sweep() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Garbage).await;
    let _sol = fx.agent("sol").await;

    let report = fx
        .collector(fx.cfg(&stub.url))
        .sweep_at(at())
        .await
        .expect("a sweep");
    assert_eq!(report.sources.len(), 0);
    assert_eq!(report.disabled.len(), 2);
    assert!(report
        .disabled
        .iter()
        .all(|(_, why)| why.contains("unparseable payload")));
    assert!(fx.delivered("sol").await.is_empty());
}

#[tokio::test]
async fn graphql_errors_are_reported_as_a_failed_source() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::GraphqlErrors).await;
    let _sol = fx.agent("sol").await;

    let report = fx
        .collector(fx.cfg(&stub.url))
        .sweep_at(at())
        .await
        .expect("a sweep");
    assert!(report
        .disabled
        .iter()
        .all(|(_, why)| why.contains("returned errors")));
}
