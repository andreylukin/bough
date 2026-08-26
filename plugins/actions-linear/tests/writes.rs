//! §7 — `linear_write` is STATUS CHANGES AND COMMENTS, and nothing else. Ticket creation is
//! refused twice: the kind does not exist, and a creation-shaped payload is refused here too.

use std::sync::Arc;

use bough_plugin_actions::{
    idem_key, marker_for, ActionKind, ActionProvider, ActionRequest, ActionTarget, ExecuteRequest,
};
use bough_plugin_actions_linear::{
    LinearActionError, LinearActions, LinearApi, LinearWritePayload,
};
use bough_plugin_ledger::{ActionId, AgentName, StepId, WakeId};
use chrono::{TimeZone, Utc};
use parking_lot::Mutex;

/// A recording Linear: it answers reads from one issue and logs every operation, so a test can
/// see exactly which mutations ran.
#[derive(Default)]
struct FakeLinear {
    calls: Mutex<Vec<(String, serde_json::Value)>>,
    comments: Mutex<Vec<String>>,
}

const ISSUE: &str = r#"{"issue":{"id":"uuid-1","identifier":"TEAM-1","team":{"states":{"nodes":[
    {"id":"state-done","name":"Done"},{"id":"state-todo","name":"Todo"}]}}}}"#;

#[async_trait::async_trait]
impl LinearApi for FakeLinear {
    async fn graphql(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, LinearActionError> {
        self.calls
            .lock()
            .push((query.to_string(), variables.clone()));
        if query.contains("comments{nodes") {
            let nodes: Vec<serde_json::Value> = self
                .comments
                .lock()
                .iter()
                .map(|b| serde_json::json!({ "id": "c1", "url": "https://linear.app/c/1", "body": b }))
                .collect();
            return Ok(serde_json::json!({ "issue": { "comments": { "nodes": nodes } } }));
        }
        if query.starts_with("query") {
            return Ok(serde_json::from_str(ISSUE).unwrap());
        }
        if query.contains("commentCreate") {
            self.comments.lock().push(
                variables
                    .get("body")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
            return Ok(serde_json::json!({
                "commentCreate": { "comment": { "id": "c1", "url": "https://linear.app/c/1" } }
            }));
        }
        Ok(serde_json::json!({ "issueUpdate": { "success": true } }))
    }
}

impl FakeLinear {
    fn mutations(&self) -> Vec<String> {
        self.calls
            .lock()
            .iter()
            .filter(|(q, _)| q.starts_with("mutation"))
            .map(|(q, _)| q.clone())
            .collect()
    }
}

fn exec(payload: serde_json::Value) -> ExecuteRequest {
    let req = ActionRequest {
        kind: ActionKind::LinearWrite,
        target: ActionTarget::new("TEAM-1"),
        payload,
        agent: AgentName::new("sol"),
        wake: WakeId::new("w1"),
        step: StepId::new("s1"),
        at: Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap(),
    };
    let canonical = req
        .target
        .canonical(ActionKind::LinearWrite)
        .expect("the identifier canonicalises");
    let idem = idem_key(ActionKind::LinearWrite, &canonical, &req.step);
    ExecuteRequest {
        marker: marker_for(&idem),
        idem_key: idem,
        action: ActionId::new("a1"),
        canonical_target: canonical,
        request: Arc::new(req),
    }
}

fn refusal(e: bough_plugin_actions::ActionError) -> String {
    match e {
        bough_plugin_actions::ActionError::Provider { source, .. } => source.to_string(),
        other => panic!("expected a provider refusal, got {other}"),
    }
}

#[test]
fn a_payload_that_is_not_exactly_one_of_the_two_is_bad() {
    let both = LinearWritePayload {
        status: Some("Done".into()),
        comment: Some("hi".into()),
    };
    assert!(matches!(
        both.check(),
        Err(LinearActionError::BadPayload { .. })
    ));
    let neither = LinearWritePayload {
        status: None,
        comment: None,
    };
    assert!(matches!(
        neither.check(),
        Err(LinearActionError::BadPayload { .. })
    ));
    let blank = LinearWritePayload {
        status: None,
        comment: Some("   ".into()),
    };
    assert!(matches!(
        blank.check(),
        Err(LinearActionError::BadPayload { .. })
    ));
    assert!(LinearWritePayload {
        status: Some("Done".into()),
        comment: None
    }
    .check()
    .is_ok());
}

#[tokio::test]
async fn linear_write_changes_status_and_comments_and_refuses_creation() {
    // A status change: the state is looked up by name and a marked comment records the move.
    let api = Arc::new(FakeLinear::default());
    let p = LinearActions::with_api(api.clone());
    let req = exec(serde_json::json!({ "status": "Done", "comment": null }));
    let artifact = p.execute(&req).await.expect("the status moves");
    assert_eq!(artifact.locator, "https://linear.app/c/1");
    let muts = api.mutations();
    assert_eq!(
        muts.len(),
        2,
        "one issueUpdate, one commentCreate: {muts:?}"
    );
    assert!(muts[0].contains("issueUpdate"));
    assert!(muts[1].contains("commentCreate"));
    assert!(
        !muts.iter().any(|m| m.contains("issueCreate")),
        "no ticket was created"
    );
    let comment = api.comments.lock()[0].clone();
    assert!(comment.starts_with("Status → Done."));
    assert!(comment.ends_with(&format!("<!-- {} -->", req.marker)));

    // A comment: one mutation, and the body keeps its text with the marker suffixed.
    let api = Arc::new(FakeLinear::default());
    let p = LinearActions::with_api(api.clone());
    let req = exec(serde_json::json!({ "status": null, "comment": "looking at it" }));
    p.execute(&req).await.expect("the comment lands");
    assert_eq!(api.mutations().len(), 1);
    let comment = api.comments.lock()[0].clone();
    assert!(comment.starts_with("looking at it"));
    assert!(comment.ends_with(&format!("<!-- {} -->", req.marker)));

    // Both, neither, and a creation-shaped payload are refused with nothing written.
    for (payload, expect) in [
        (
            serde_json::json!({ "status": "Done", "comment": "hi" }),
            "exactly one",
        ),
        (serde_json::json!({}), "exactly one"),
        (
            serde_json::json!({ "title": "a new ticket", "comment": "hi" }),
            "creating tickets is Andrey's",
        ),
    ] {
        let api = Arc::new(FakeLinear::default());
        let p = LinearActions::with_api(api.clone());
        let e = p
            .execute(&exec(payload.clone()))
            .await
            .expect_err("refused");
        let text = refusal(e);
        assert!(text.contains(expect), "for {payload}: {text}");
        assert!(
            api.mutations().is_empty(),
            "the refusal came before any mutation: {:?}",
            api.mutations()
        );
    }
}

/// A `team`/`teamId`/`description` field is creation-shaped too.
#[tokio::test]
async fn every_creation_shaped_field_is_refused_as_creation() {
    for field in ["team", "teamId", "description"] {
        let api = Arc::new(FakeLinear::default());
        let p = LinearActions::with_api(api.clone());
        let payload = serde_json::json!({ field: "x", "comment": "hi" });
        let text = refusal(p.execute(&exec(payload)).await.expect_err("refused"));
        assert!(text.contains("creating tickets"), "for {field}: {text}");
        assert!(api.mutations().is_empty());
    }
}

/// The config's Debug never prints the key.
#[test]
fn the_api_key_is_redacted_from_the_config_rendering() {
    let cfg = bough_plugin_actions_linear::LinearActionsConfig {
        endpoint: "https://api.linear.app/graphql".into(),
        api_key: "lin_api_SECRETVALUE".into(),
        timeout_ms: 1000,
    };
    let rendered = format!("{cfg:?}");
    assert!(!rendered.contains("SECRETVALUE"), "{rendered}");
    assert!(rendered.contains("<redacted>"));
    assert!(rendered.contains("api.linear.app"));
}

/// Reconciliation's read half: the marker is found on a comment, and looking is not writing.
#[tokio::test]
async fn find_marker_reads_the_issues_comments_and_writes_nothing() {
    use bough_plugin_actions_reconcile::ArtifactLookup;
    let api = Arc::new(FakeLinear::default());
    let p = LinearActions::with_api(api.clone());
    let req = exec(serde_json::json!({ "status": null, "comment": "done" }));
    p.execute(&req).await.expect("the comment lands");

    let before = api.mutations().len();
    let found = p
        .find_marker(ActionKind::LinearWrite, "TEAM-1", &req.marker)
        .await
        .expect("the lookup runs")
        .expect("the marker is in the world");
    assert_eq!(found.marker, req.marker);
    assert_eq!(api.mutations().len(), before, "a lookup writes nothing");

    let absent = p
        .find_marker(
            ActionKind::LinearWrite,
            "TEAM-1",
            "bough-action:0000000000000000",
        )
        .await
        .expect("the lookup runs");
    assert!(absent.is_none());
}
