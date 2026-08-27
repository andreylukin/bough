//! §3's fan-out rule, end to end: ONE event reaches EVERY matching lane, each with its own
//! `mail/delivered` step, its own seq and its own consumption state — and the `mail/route`
//! waterfall is a real extension point over that decision, not decoration.

mod common;

use bough_plugin_ledger::AgentName;
use bough_plugin_mail_router::{MailRoute, RouteDecision};
use common::*;

#[tokio::test]
async fn one_event_reaches_every_matching_agent() {
    let f = fixture().await;
    f.lane("ci", &["repo:bough", "gh:bough/bough#12"]).await;
    f.lane("infra", &["repo:bough"]).await;
    f.lane("docs", &["repo:wiki"]).await;

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");

    // Two owners, not the "best" one, and `docs` is untouched.
    assert_eq!(report.matched, names(&["ci", "infra"]));
    assert_eq!(
        report
            .delivered
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>(),
        names(&["ci", "infra"])
    );
    assert!(report.unsorted.is_none());
    assert!(f.steps_on("t-docs", "mail/delivered").await.is_empty());
}

#[tokio::test]
async fn each_recipient_gets_its_own_mail_delivered_step_and_seq() {
    let f = fixture().await;
    f.lane("ci", &["repo:bough"]).await;
    f.lane("infra", &["repo:bough"]).await;

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");

    assert_eq!(f.steps_on("t-ci", "mail/delivered").await.len(), 1);
    assert_eq!(f.steps_on("t-infra", "mail/delivered").await.len(), 1);

    // Each recipient's receipt is ITS OWN (P3-D15): a distinct message, a distinct splice step,
    // and that step on the recipient's own trajectory. Seq is per trajectory, so two recipients
    // may well share a NUMBER — what must never be shared is the row it points at.
    let (ci, infra) = (&report.delivered[0].1, &report.delivered[1].1);
    assert_ne!(ci.message, infra.message);
    assert_ne!(ci.step, infra.step);
    for (receipt, traj) in [(ci, "t-ci"), (infra, "t-infra")] {
        let spliced = f
            .ledger
            .0
            .step(&receipt.step)
            .await
            .expect("a read")
            .expect("the splice step");
        assert_eq!(spliced.traj.as_str(), traj);
        assert_eq!(spliced.seq, receipt.seq);
    }
}

#[tokio::test]
async fn consumption_by_one_agent_leaves_the_others_unconsumed() {
    let f = fixture().await;
    f.lane("ci", &["repo:bough"]).await;
    f.lane("infra", &["repo:bough"]).await;

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");
    // The seq consumption is keyed by is the `mail/delivered` step's, not the splice receipt's
    // (§5: consumption is per (agent, MAIL seq)).
    let ci_seq = f.steps_on("t-ci", "mail/delivered").await[0].seq;
    let _ = &report;

    assert_eq!(f.unconsumed("t-ci").await.len(), 1);
    assert_eq!(f.unconsumed("t-infra").await.len(), 1);

    f.consume("t-ci", ci_seq).await;

    assert!(f.unconsumed("t-ci").await.is_empty());
    assert_eq!(
        f.unconsumed("t-infra").await.len(),
        1,
        "consumption is per (agent, seq); one lane reading its mail must not read another's"
    );
}

#[tokio::test]
async fn a_misroute_to_a_third_agent_does_not_strand_the_true_owner() {
    let f = fixture().await;
    f.lane("ci", &["repo:bough"]).await;
    f.lane("stranger", &[]).await;

    // A policy listener that ADDS a wrong recipient. The failure this test exists for is a
    // router that treats "someone got it" as "it was routed" and drops the true owner.
    let _l = f
        .ctx
        .on_waterfall::<MailRoute, _, _>(|mut d: RouteDecision, next| async move {
            d.to.push(AgentName::new("stranger"));
            next.run(d).await
        })
        .await
        .expect("a listener");

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");

    assert_eq!(report.matched, names(&["ci", "stranger"]));
    assert_eq!(f.steps_on("t-ci", "mail/delivered").await.len(), 1);
    assert_eq!(f.unconsumed("t-ci").await.len(), 1);
}

#[tokio::test]
async fn a_route_listener_may_add_a_recipient() {
    let f = fixture().await;
    f.lane("ci", &["repo:bough"]).await;
    f.lane("audit", &[]).await;

    let _l = f
        .ctx
        .on_waterfall::<MailRoute, _, _>(|mut d: RouteDecision, next| async move {
            // The shape §0.2 promises: policy attaches to the seam without importing the loop.
            d.to.push(AgentName::new("audit"));
            next.run(d).await
        })
        .await
        .expect("a listener");

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");
    assert_eq!(report.matched, names(&["ci", "audit"]));
    assert_eq!(f.steps_on("t-audit", "mail/delivered").await.len(), 1);
}

#[tokio::test]
async fn a_route_listener_that_skips_next_short_circuits() {
    let f = fixture().await;
    f.lane("ci", &["repo:bough"]).await;
    f.lane("audit", &[]).await;

    // Registered FIRST, so it runs first and never calls `next`.
    let _first = f
        .ctx
        .on_waterfall::<MailRoute, _, _>(|d: RouteDecision, _next| async move {
            // P5-D5: it short-circuits to a decision that ALREADY has the true owners in it,
            // because the matcher seeded it before dispatch rather than running as a listener.
            d
        })
        .await
        .expect("a listener");
    let _second = f
        .ctx
        .on_waterfall::<MailRoute, _, _>(|mut d: RouteDecision, next| async move {
            d.to.push(AgentName::new("audit"));
            next.run(d).await
        })
        .await
        .expect("a listener");

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");
    assert_eq!(report.matched, names(&["ci"]));
    assert!(f.steps_on("t-audit", "mail/delivered").await.is_empty());
}

/// §3: "Misroutes stay recoverable via refs." A row whose agent is not up is the one case the
/// router can observe on its own, and skipping it silently DROPPED the event: no `mail/delivered`,
/// no `mail/unrouted`, nothing in the ledger naming it, while the report still said it matched.
#[tokio::test]
async fn a_matched_lane_with_no_live_agent_is_recorded_not_dropped() {
    let f = fixture().await;
    f.ledger
        .0
        .put_agent(bough_plugin_ledger::AgentRow {
            name: AgentName::new("asleep"),
            traj: bough_plugin_ledger::TrajId::new("t-asleep"),
            routing_refs: set(&["repo:bough"]),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("a row with no live agent");

    let report = f
        .mail
        .route(envelope("CI is red", &["repo:bough"]))
        .await
        .expect("a route");

    assert_eq!(report.matched, names(&["asleep"]));
    assert!(report.delivered.is_empty());
    assert_eq!(
        report.undeliverable,
        names(&["asleep"]),
        "the caller is told who it could not reach"
    );
    // And the event is recoverable: it is on the unsorted trajectory, which is what the leader
    // reads.
    assert!(report.unsorted.is_some());
    let queued = f.mail.unsorted(50).await.expect("a read");
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].body.get("subject").and_then(|s| s.as_str()),
        Some("CI is red")
    );
}
