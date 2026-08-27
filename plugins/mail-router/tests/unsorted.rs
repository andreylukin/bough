//! P5-D4: the unsorted queue is a REAL trajectory and the leader is a SINK on it, not its owner.
//! A tree may boot with no leader — headless, or in the moment before the `leader` row activates —
//! and mail must be neither dropped nor refused then.

mod common;

use std::sync::Arc;

use bough_plugin_ledger::AgentName;
use bough_plugin_mail_router::{Question, UnsortedSink, ASK_CLASS_REF};
use common::*;

#[tokio::test]
async fn zero_matches_lands_in_the_unsorted_queue() {
    let f = fixture().await;
    f.lane("docs", &["repo:wiki"]).await;

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");

    assert!(report.matched.is_empty());
    assert!(!report.adopted);
    let queued = f.mail.unsorted(50).await.expect("a read");
    assert_eq!(queued.len(), 1);
    assert_eq!(Some(queued[0].id.clone()), report.unsorted);
    // Nothing was invented for `docs`: a zero-match envelope reaches nobody.
    assert!(f.steps_on("t-docs", "mail/delivered").await.is_empty());
}

#[tokio::test]
async fn the_leader_sink_receives_it_as_ordinary_mail() {
    let f = fixture().await;
    f.lane("leader", &[]).await;
    let _sink = f
        .mail
        .unsorted_sink(
            &f.ctx,
            Arc::new(NamedSink(AgentName::new("leader"))) as Arc<dyn UnsortedSink>,
        )
        .await
        .expect("the sink mounts");

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");

    assert!(report.unsorted.is_some(), "it is still queued durably");
    assert!(report.adopted);
    let delivered = f.steps_on("t-leader", "mail/delivered").await;
    assert_eq!(delivered.len(), 1);
    // ORDINARY, never wake: an unsorted item is the leader's inbox work, not an interruption.
    assert_eq!(
        delivered[0].body.get("class").and_then(|c| c.as_str()),
        Some("ordinary")
    );
}

#[tokio::test]
async fn with_no_sink_the_queue_keeps_it_and_a_later_sink_adopts_it() {
    let f = fixture().await;

    // The leaderless moment. Nothing is dropped and nothing errors.
    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route with no leader in the tree");
    assert!(!report.adopted);
    let queued = f.mail.unsorted(50).await.expect("a read");
    assert_eq!(queued.len(), 1);

    // The leader arrives afterwards and takes the backlog.
    f.lane("leader", &[]).await;
    let _sink = f
        .mail
        .unsorted_sink(
            &f.ctx,
            Arc::new(NamedSink(AgentName::new("leader"))) as Arc<dyn UnsortedSink>,
        )
        .await
        .expect("the sink mounts");
    f.lane("ci", &[]).await;
    let receipts = f
        .mail
        .adopt(&AgentName::new("ci"), &[queued[0].id.clone()], now())
        .await
        .expect("an adoption");

    assert_eq!(receipts.len(), 1);
    assert_eq!(f.steps_on("unsorted", "mail/adopted").await.len(), 1);
    assert_eq!(f.steps_on("t-ci", "mail/delivered").await.len(), 1);
}

#[tokio::test]
async fn ask_leader_is_wake_class_and_carries_class_ask() {
    let f = fixture().await;
    // A leader that reactivates on `class:ask` is exactly a lane routing on that ref.
    f.lane("leader", &[ASK_CLASS_REF]).await;

    let step = f
        .mail
        .ask_leader(Question {
            asked_by: "graph-ops",
            about: "which lane owns `repo:bough` after the split?".into(),
            options: vec!["ci".into(), "infra".into()],
            cites: vec![],
            refs: set(&["repo:bough"]),
            at: now(),
        })
        .await
        .expect("a question");

    // The question itself is a Thought on the unsorted trajectory: a question is not truth (§16).
    let questions = f.steps_on("unsorted", "leader/question").await;
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].id, step);
    assert_eq!(
        questions[0].body.get("asked_by").and_then(|v| v.as_str()),
        Some("graph-ops")
    );

    let delivered = f.steps_on("t-leader", "mail/delivered").await;
    assert_eq!(delivered.len(), 1, "the leader was asked, exactly once");
    assert_eq!(
        delivered[0].body.get("class").and_then(|c| c.as_str()),
        Some("wake"),
        "wake class is what may reactivate a dormant leader"
    );
    assert!(
        delivered[0]
            .body
            .get("refs")
            .and_then(|r| r.as_array())
            .expect("refs")
            .iter()
            .any(|r| r.as_str() == Some(ASK_CLASS_REF)),
        "the `class:ask` ref is the gate a dormant leader's wake_classes is checked against"
    );
}
