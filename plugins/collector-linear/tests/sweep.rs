//! Invariant under test (V1, P6-D7): a sweep against the local stub delivers cited mail, a second
//! sweep delivers nothing, and the API KEY appears in nothing but the `Authorization` header —
//! not in the report, not in an error, not in the `Debug` rendering of the config.

use crate::common;

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

/// The configured scope REACHES THE QUERY. `teams`/`projects` used to be config nothing read: the
/// query carried no filter, so the sweep collected every issue and comment in the workspace and
/// stamped `WakeClass::Assigned` on all of them.
#[tokio::test]
async fn the_configured_scope_is_in_the_query_the_stub_receives() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Ok).await;
    let _sol = fx.agent("sol").await;
    let mut cfg = fx.cfg(&stub.url);
    cfg.projects = vec!["Rebuild".to_string()];
    fx.collector(cfg).sweep_at(at()).await.expect("a sweep");

    // The stub records inside the connection task, which may finish just after the client's
    // response has been read; wait, bounded, for both queries rather than racing it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while stub.requests().len() < 2 && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let bodies: Vec<serde_json::Value> = stub
        .requests()
        .iter()
        .map(|(_, body)| serde_json::from_str(body).expect("the stub saw JSON"))
        .collect();
    assert!(!bodies.is_empty(), "the stub saw a request");
    let issues = bodies
        .iter()
        .find(|b| b["query"].as_str().unwrap_or("").contains("BoughIssues"))
        .expect("the issues query was sent");
    let and = issues["variables"]["filter"]["and"]
        .as_array()
        .expect("the issues query carries a filter");
    assert_eq!(and[0]["assignee"]["isMe"]["eq"], true, "{issues}");
    assert!(
        and.iter().any(|c| c["team"]["key"]["in"][0] == "TEAM"),
        "the configured team is in the filter: {issues}"
    );
    assert!(
        and.iter()
            .any(|c| c["project"]["name"]["in"][0] == "Rebuild"),
        "the configured project is in the filter: {issues}"
    );

    let comments = bodies
        .iter()
        .find(|b| b["query"].as_str().unwrap_or("").contains("BoughComments"))
        .expect("the comments query was sent");
    assert_eq!(
        comments["variables"]["filter"]["and"][0]["issue"]["team"]["key"]["in"][0], "TEAM",
        "a comment is scoped through its issue: {comments}"
    );
}

/// A row with neither `teams` nor `projects` — which is what `bundles/bough-base.yml` ships —
/// says so, every sweep, instead of quietly sweeping the whole workspace into `deliver_to`.
#[tokio::test]
async fn an_unscoped_row_reports_itself_off_and_sends_nothing() {
    let fx = Fx::new().await;
    let stub = Stub::start(Mode::Ok).await;
    let _sol = fx.agent("sol").await;
    let mut cfg = fx.cfg(&stub.url);
    cfg.teams = Vec::new();
    cfg.projects = Vec::new();
    let report = fx.collector(cfg).sweep_at(at()).await.expect("a sweep");

    assert!(
        report
            .disabled
            .iter()
            .any(|(what, why)| what == "scope" && why.contains("neither `teams` nor `projects`")),
        "{:?}",
        report.disabled
    );
    assert!(report.sources.is_empty(), "{:?}", report.sources);
    assert!(stub.requests().is_empty(), "no query was sent at all");
    assert!(fx.delivered("sol").await.is_empty());
}

/// A row's key leaves process memory with the row. `open` takes a reference on it for the
/// invariant to scan by; nothing used to give it back, so a disabled or reloaded row left the
/// secret behind.
#[test]
fn releasing_a_rows_key_takes_it_out_of_process_memory() {
    use bough_plugin_collector_linear::{active_keys, hold_key, release_key};
    // A key of its own, so a collector another test in this binary built cannot hold it.
    const MINE: &str = "lin_api_release_test";
    // Two rows holding one key: the first release must not blind the second's invariant.
    hold_key(MINE);
    hold_key(MINE);
    assert!(active_keys().iter().any(|k| k == MINE));
    release_key(MINE);
    assert!(
        active_keys().iter().any(|k| k == MINE),
        "the second row still holds it"
    );
    release_key(MINE);
    assert!(
        !active_keys().iter().any(|k| k == MINE),
        "the last release takes it out: {:?}",
        active_keys()
    );
}
