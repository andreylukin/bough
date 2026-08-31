//! Invariant under test: the MCP transport is the SAME collector with a different wire. A sweep
//! through a stub `linear-server` delivers the same cited-mail shape the GraphQL transport
//! delivers, with NO API KEY anywhere in the process's config; the viewer pin (`assignee: "me"`)
//! and the watermark ride the call arguments; a missing server row is a loud per-sweep report,
//! never a silent empty sweep.

use crate::common;

use bough_plugin_ledger::Class;
use common::{at, Fx};

#[tokio::test]
async fn a_sweep_through_the_mcp_server_delivers_cited_mail_and_needs_no_key() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;
    let (collector, stub) = fx.mcp_collector(fx.mcp_cfg()).await;

    let report = collector.sweep_at(at()).await.expect("a sweep");

    assert!(report.disabled.is_empty(), "{:?}", report.disabled);
    let delivered: usize = report.sources.iter().map(|(_, d, _, _)| d).sum();
    // Two issues and one comment (on TEAM-123 only).
    assert_eq!(delivered, 3, "{:?}", report.sources);

    let steps = fx.delivered("sol").await;
    assert_eq!(steps.len(), 3);
    for step in &steps {
        assert_eq!(step.class, Class::Evidence);
        assert!(!step.cites.is_empty(), "cited by construction");
        assert!(step.refs.iter().any(|r| r.as_str().starts_with("linear:")));
    }
    // The issue is a configured wake class; a comment is not (§5): same rule as GraphQL.
    let classes: Vec<&str> = steps
        .iter()
        .map(|s| s.body["class"].as_str().expect("a class"))
        .collect();
    assert!(classes.contains(&"wake"), "{classes:?}");
    assert!(classes.contains(&"ordinary"), "{classes:?}");

    // The viewer pin is IN THE CALL, because `WakeClass::Assigned` is stamped on what it returns.
    for (tool, args) in stub.calls.lock().iter() {
        if tool == "list_issues" {
            assert_eq!(args["assignee"], "me", "{args}");
            assert_eq!(args["team"], "TEAM", "{args}");
        }
    }
}

#[tokio::test]
async fn a_second_sweep_delivers_nothing_and_carries_the_watermark_in_the_call() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;
    let (collector, stub) = fx.mcp_collector(fx.mcp_cfg()).await;

    collector.sweep_at(at()).await.expect("the first sweep");
    let second = collector.sweep_at(at()).await.expect("the second sweep");

    let delivered: usize = second.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 0, "{:?}", second.sources);
    assert_eq!(fx.delivered("sol").await.len(), 3);

    // The second round of `list_issues` calls carries an `updatedAt` bound read from the
    // watermark; the first carried none.
    let calls = stub.calls.lock().clone();
    let issue_calls: Vec<&serde_json::Value> = calls
        .iter()
        .filter(|(t, _)| t == "list_issues")
        .map(|(_, a)| a)
        .collect();
    assert!(issue_calls[0].get("updatedAt").is_none(), "{issue_calls:?}");
    assert!(
        issue_calls.last().unwrap().get("updatedAt").is_some(),
        "{issue_calls:?}"
    );
}

#[tokio::test]
async fn a_missing_server_row_is_a_loud_per_sweep_report_not_a_silent_empty_sweep() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;
    // An mcp seam with NO server registered: the row activated, the server row did not.
    let mcp = bough_plugin_mcp::McpHandle::new();
    let collector = fx
        .collector(fx.mcp_cfg())
        .with_mcp(mcp, bough_plugin_mcp::ServerName::new("linear-server"));

    let report = collector.sweep_at(at()).await.expect("the sweep survives");
    assert!(report.sources.is_empty(), "{:?}", report.sources);
    assert_eq!(report.disabled.len(), 2, "{:?}", report.disabled);
    for (_, why) in &report.disabled {
        assert!(why.contains("linear-server"), "{why}");
    }
    assert!(fx.delivered("sol").await.is_empty());
}

#[tokio::test]
async fn an_empty_api_key_disables_nothing_when_the_transport_is_mcp() {
    let fx = Fx::new().await;
    let _sol = fx.agent("sol").await;
    let cfg = fx.mcp_cfg();
    assert!(cfg.api_key.is_empty(), "the premise of the test");
    let (collector, _stub) = fx.mcp_collector(cfg).await;

    let report = collector.sweep_at(at()).await.expect("a sweep");
    assert!(
        !report.disabled.iter().any(|(s, _)| s == "api_key"),
        "{:?}",
        report.disabled
    );
}
