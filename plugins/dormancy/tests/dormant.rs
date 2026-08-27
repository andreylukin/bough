//! §1 at the seam: a dormant agent gets NO ticks and NO drain wakes, keeps its keep and its
//! routing, and its ordinary mail is delivered and left queued on purpose.

mod common;

use bough_plugin_agents::{WakeCause, WakeKind, WakeRequest};
use common::*;

/// The whole of §1's first sentence, durably: mail arrives, and no wake exists.
#[tokio::test]
async fn a_dormant_agent_opens_no_wake_for_ordinary_mail() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    t.sleep().await;

    agent
        .deliver(ordinary("a PR moved", &["repo:bough"]))
        .await
        .expect("delivery is not suppressed");
    // The drain the standing invariant would arm for a live lane, asked for explicitly: without
    // it the assertion would be vacuous, because ordinary mail is not a wake reason by itself.
    let req = agent
        .request_wake(
            WakeKind::Drain,
            WakeCause::Mail {
                class: bough_plugin_agents::MailClass::Ordinary,
            },
        )
        .await;
    settle(&agent).await;

    assert_eq!(req, WakeRequest::Nothing);
    assert!(
        t.steps_of("wake/start").await.is_empty(),
        "a dormant lane opens no wake at all: {:?}",
        t.steps_of("wake/start").await
    );

    // THE CONTROL: the same tree, the same mail, awake — and the drain does open a wake.
    let live = tree(&["repo:bough"], &["class:ask"]).await;
    let other = live.sol().await;
    other
        .deliver(ordinary("a PR moved", &["repo:bough"]))
        .await
        .expect("delivery");
    let req = other
        .request_wake(
            WakeKind::Drain,
            WakeCause::Mail {
                class: bough_plugin_agents::MailClass::Ordinary,
            },
        )
        .await;
    settle(&other).await;
    assert!(matches!(req, WakeRequest::Started(_)));
    assert_eq!(
        live.steps_of("wake/start").await.len(),
        1,
        "the suppression is dormancy's, not the fixture's"
    );
}

/// Dormancy suppresses WAKES, never DELIVERIES (§5): the backlog has to survive the sleep.
#[tokio::test]
async fn ordinary_mail_is_delivered_and_stays_unconsumed() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    t.sleep().await;

    agent
        .deliver(ordinary("one", &["repo:bough"]))
        .await
        .expect("delivery");
    agent
        .deliver(ordinary("two", &["repo:bough"]))
        .await
        .expect("delivery");
    settle(&agent).await;

    let steps = t.steps().await;
    assert_eq!(
        steps
            .iter()
            .filter(|s| s.kind.as_str() == "mail/delivered")
            .count(),
        2,
        "both deliveries landed"
    );
    let consumed = bough_plugin_agent_loop::mail::consumed_union(&steps);
    let unconsumed = bough_plugin_agent_loop::mail::unconsumed(&steps, &consumed);
    assert_eq!(
        unconsumed.len(),
        2,
        "and neither was consumed: nothing woke to consume them"
    );
}

/// P5-D1's reason for the extra argument: a sleeping lane with a backlog and no drain scheduled
/// is the CORRECT state, not a permanent violation.
#[tokio::test]
async fn the_standing_invariant_holds_over_a_dormant_agent_with_a_backlog() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    t.sleep().await;
    agent
        .deliver(ordinary("backlog", &["repo:bough"]))
        .await
        .expect("delivery");
    settle(&agent).await;

    let steps = t.steps().await;
    bough_plugin_agent_loop::invariant::evaluate_mail(&steps, false)
        .expect("a dormant lane's backlog satisfies the standing invariant");

    // And the same stream is a violation the moment the lane is awake again.
    let awake: Vec<_> = steps
        .into_iter()
        .filter(|s| s.kind.as_str() != bough_plugin_dormancy::STEP_TYPE)
        .collect();
    assert!(
        bough_plugin_agent_loop::invariant::evaluate_mail(&awake, false).is_err(),
        "without the dormancy steps the very same backlog is a violation"
    );
}

/// §1: a dormant agent "keeps its keep and routing". Sleeping edits no row.
#[tokio::test]
async fn a_dormant_agent_keeps_its_routing_refs_and_wake_classes() {
    let t = tree(&["repo:bough", "gh:o/r"], &["class:ask", "class:review"]).await;
    let before = t
        .ledger
        .0
        .agent(&name("sol"))
        .await
        .expect("a read")
        .expect("the row");
    t.sleep().await;
    let after = t
        .ledger
        .0
        .agent(&name("sol"))
        .await
        .expect("a read")
        .expect("the row");

    assert_eq!(before.routing_refs, after.routing_refs);
    assert_eq!(before.wake_classes, after.wake_classes);
    assert_eq!(before.traj, after.traj);
    assert!(t.dormancy.is_dormant(&name("sol")));
    assert_eq!(t.dormancy.dormant(), vec![name("sol")]);
}

/// The catch-up entry point (P3-D16) goes through the same admission point: no wake, no ledger
/// row, `Nothing` back.
#[tokio::test]
async fn request_wake_returns_nothing_for_a_dormant_agent() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    agent
        .deliver(ordinary("something to catch up on", &["repo:bough"]))
        .await
        .expect("delivery");
    settle(&agent).await;
    t.sleep().await;
    let before = t.steps().await.len();

    let req = agent
        .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
        .await;

    assert_eq!(
        req,
        WakeRequest::Nothing,
        "a dormant lane has nothing to catch up on: the wake never opens"
    );
    assert_eq!(
        t.steps().await.len(),
        before,
        "and nothing was appended by the attempt"
    );
}

/// §1's "no ticks" literally: a `Scheduled` wake — the tick a schedule fires — is deferred at the
/// same admission point. Both lanes are given work first, so the tick has something to process and
/// the control shows the identical tick opening a wake on the awake lane.
#[tokio::test]
async fn a_scheduled_tick_opens_no_wake_for_a_dormant_agent() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    t.sleep().await;
    agent
        .deliver(ordinary("work for the tick", &["repo:bough"]))
        .await
        .expect("delivery");
    settle(&agent).await;

    let req = agent
        .request_wake(WakeKind::Scheduled, WakeCause::Schedule("nightly"))
        .await;
    settle(&agent).await;

    assert_eq!(req, WakeRequest::Nothing, "a dormant lane gets no ticks");
    assert!(
        t.steps_of("wake/start").await.is_empty(),
        "and the tick appended nothing: {:?}",
        t.steps_of("wake/start").await
    );

    // THE CONTROL: the identical tick, the identical work, awake — and it does open a wake.
    let live = tree(&["repo:bough"], &["class:ask"]).await;
    let other = live.sol().await;
    other
        .deliver(ordinary("work for the tick", &["repo:bough"]))
        .await
        .expect("delivery");
    settle(&other).await;
    let req = other
        .request_wake(WakeKind::Scheduled, WakeCause::Schedule("nightly"))
        .await;
    settle(&other).await;
    assert!(
        matches!(req, WakeRequest::Started(_)),
        "the suppression is dormancy's, not the fixture's: {req:?}"
    );
    assert_eq!(live.steps_of("wake/start").await.len(), 1);
}
