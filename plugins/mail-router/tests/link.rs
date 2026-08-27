//! §5: "a late-added routing ref starts mail delivery from LINK TIME, with earlier history
//! reachable by query, never queued as backlog." All three tests here are about that one
//! sentence, from the three sides it can be got wrong from.

use crate::common;

use bough_plugin_ledger::AgentName;
use common::*;

#[tokio::test]
async fn a_late_linked_ref_queues_no_backlog() {
    let f = fixture().await;
    f.lane("ci", &[]).await;

    // Three events go unsorted while nobody routes on the ref.
    for i in 0..3 {
        f.mail
            .route(envelope(&format!("event {i}"), &["repo:bough"]))
            .await
            .expect("a route");
    }
    assert_eq!(f.mail.unsorted(50).await.expect("a read").len(), 3);

    let report = f
        .mail
        .link_ref(&AgentName::new("ci"), set(&["repo:bough"]), now())
        .await
        .expect("a link");

    assert_eq!(report.backfilled, 0);
    assert_eq!(report.added, set(&["repo:bough"]));
    assert!(
        f.steps_on("t-ci", "mail/delivered").await.is_empty(),
        "linking a ref must not replay history into an inbox"
    );
    // And the link itself is evidence, so a later delivery is explainable.
    let routing = f.steps_on("t-ci", "agent/routing").await;
    assert_eq!(routing.len(), 1);
    assert_eq!(
        routing[0].body.get("agent").and_then(|v| v.as_str()),
        Some("ci")
    );
}

#[tokio::test]
async fn a_late_linked_ref_exposes_history_through_connected() {
    let f = fixture().await;
    f.lane("ci", &[]).await;
    f.lane("infra", &["repo:bough"]).await;

    // `infra` has real history under the ref, on its own trajectory.
    f.mail
        .route(envelope("an earlier event", &["repo:bough"]))
        .await
        .expect("a route");
    assert_eq!(f.steps_on("t-infra", "mail/delivered").await.len(), 1);

    let report = f
        .mail
        .link_ref(&AgentName::new("ci"), set(&["repo:bough"]), now())
        .await
        .expect("a link");

    // Nothing was queued, and yet the history is REACHABLE: `connected()` is the query §5 means.
    assert_eq!(report.backfilled, 0);
    let connected = f
        .ledger
        .0
        .connected(&AgentName::new("ci"))
        .await
        .expect("membership");
    assert!(
        connected
            .ref_matches
            .iter()
            .any(|t| t.as_str() == "t-infra"),
        "the linked ref reaches `infra`'s trajectory: {:?}",
        connected.ref_matches
    );
    assert_eq!(report.now_connected, connected.ref_matches);
}

#[tokio::test]
async fn delivery_after_the_link_starts_at_link_time() {
    let f = fixture().await;
    f.lane("ci", &[]).await;

    f.mail
        .route(envelope("before the link", &["repo:bough"]))
        .await
        .expect("a route");
    f.mail
        .link_ref(&AgentName::new("ci"), set(&["repo:bough"]), now())
        .await
        .expect("a link");
    f.mail
        .route(envelope("after the link", &["repo:bough"]))
        .await
        .expect("a route");

    let delivered = f.steps_on("t-ci", "mail/delivered").await;
    assert_eq!(delivered.len(), 1, "exactly the one that came after");
    assert_eq!(
        delivered[0].body.get("subject").and_then(|v| v.as_str()),
        Some("after the link")
    );
    // The earlier one is still in the queue, where a leader can adopt it deliberately.
    let queued = f.mail.unsorted(50).await.expect("a read");
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].body.get("subject").and_then(|v| v.as_str()),
        Some("before the link")
    );
}
