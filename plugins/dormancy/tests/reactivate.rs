//! §1's second sentence at the seam: a dormant lane is reactivated ONLY by an Andrey message or a
//! wake-class item per `agents.wake_classes`, the reactivation is durable, and the backlog drains
//! through §5's standing invariant rather than through a special path.

use crate::common;

use bough_plugin_dormancy::{ReactivateCause, WakeUpRequest};
use common::*;

/// Andrey always gets through, and what he gets is a fresh answer wake (§5).
#[tokio::test]
async fn an_andrey_message_reactivates_and_gets_a_sol_answer_wake() {
    // The LIVE loop Provider: this case is about the answer wake §5 owes him, so the driver that
    // actually runs a model step is the one that has to prove it.
    let t = live_tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    t.sleep().await;
    assert!(t.dormancy.is_dormant(&name("sol")));

    agent
        .followup(from_andrey("are you awake?"))
        .await
        .expect("his message is delivered");
    settle(&agent).await;

    assert!(
        !t.dormancy.is_dormant(&name("sol")),
        "an Andrey message reactivates whatever the wake classes say"
    );
    let starts = t.steps_of("wake/start").await;
    assert_eq!(starts.len(), 1, "exactly one wake: {starts:?}");
    assert_eq!(
        starts[0].body.get("urgency").and_then(|v| v.as_str()),
        Some("immediate"),
        "his wake is IMMEDIATE, never a coalesced drain: {:?}",
        starts[0].body
    );
    assert!(
        !t.steps_of("thought/text").await.is_empty(),
        "and it actually answered him"
    );
}

/// A wake-class item the row asked for reactivates; one it did not ask for does not.
#[tokio::test]
async fn a_wake_class_item_reactivates() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    t.sleep().await;

    // Not asked for: the lane stays asleep and the item queues.
    agent
        .deliver(wake_class("a review landed", &["class:review"]))
        .await
        .expect("delivery");
    settle(&agent).await;
    assert!(
        t.dormancy.is_dormant(&name("sol")),
        "`class:review` is not in this row's wake_classes"
    );
    assert!(t.steps_of("wake/start").await.is_empty());

    // Asked for: it reactivates.
    agent
        .deliver(wake_class("someone asked you something", &["class:ask"]))
        .await
        .expect("delivery");
    settle(&agent).await;
    assert!(!t.dormancy.is_dormant(&name("sol")));
    assert_eq!(t.steps_of("wake/start").await.len(), 1);
}

/// §5: reactivation arms ONE drain wake, and only when something is queued.
#[tokio::test]
async fn reactivation_arms_one_drain_wake() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    t.sleep().await;
    agent
        .deliver(ordinary("backlog", &["repo:bough"]))
        .await
        .expect("delivery");
    settle(&agent).await;

    let change = t
        .dormancy
        .wake_up(WakeUpRequest {
            agent: name("sol"),
            cause: ReactivateCause::Command,
            cites: Vec::new(),
            at: now(),
        })
        .await
        .expect("the reactivation commits");

    assert!(!change.dormant);
    assert!(
        change.drain.is_some(),
        "a backlog at reactivation arms the drain the standing invariant demands"
    );
    settle(&agent).await;
    assert_eq!(
        t.steps_of("wake/start").await.len(),
        1,
        "ONE drain wake, not one per queued item"
    );

    // And with nothing queued there is no drain at all — §5's "and none when nothing is queued".
    let quiet = tree(&["repo:bough"], &["class:ask"]).await;
    let other = quiet.sol().await;
    quiet.sleep().await;
    let change = quiet
        .dormancy
        .wake_up(WakeUpRequest {
            agent: name("sol"),
            cause: ReactivateCause::Command,
            cites: Vec::new(),
            at: now(),
        })
        .await
        .expect("the reactivation commits");
    settle(&other).await;
    assert_eq!(change.drain, None);
    assert!(quiet.steps_of("wake/start").await.is_empty());
}

/// The backlog is not replayed by dormancy: the drain wake claims it, and the invariant that
/// demanded the drain is then satisfied by the ledger.
#[tokio::test]
async fn the_backlog_drains_by_the_standing_invariant() {
    // The LIVE loop: consumption is what this case is about, and the scripted Provider re-writes
    // its own `mail/delivered` steps rather than consuming the producer's.
    let t = live_tree(&["repo:bough"], &["class:ask"]).await;
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

    t.dormancy
        .wake_up(WakeUpRequest {
            agent: name("sol"),
            cause: ReactivateCause::Command,
            cites: Vec::new(),
            at: now(),
        })
        .await
        .expect("the reactivation commits");
    settle(&agent).await;

    let steps = t.steps().await;
    let consumed = bough_plugin_agent_loop::mail::consumed_union(&steps);
    let left = bough_plugin_agent_loop::mail::unconsumed(&steps, &consumed);
    assert!(
        left.is_empty(),
        "the one drain wake consumed the whole backlog: {left:?}"
    );
    bough_plugin_agent_loop::invariant::evaluate_mail(&steps, false)
        .expect("nothing unconsumed is nothing to schedule");
}

/// P5-D2: the reactivation is DURABLE, exactly one step, and it says what woke the lane.
#[tokio::test]
async fn reactivation_appends_one_dormancy_step_citing_the_trigger() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let agent = t.sol().await;
    t.sleep().await;
    agent
        .deliver(wake_class("someone asked", &["class:ask"]))
        .await
        .expect("delivery");
    settle(&agent).await;

    let steps = t.steps_of(bough_plugin_dormancy::STEP_TYPE).await;
    assert_eq!(steps.len(), 2, "one sleep, one reactivation: {steps:?}");
    let last = steps.last().expect("the reactivation");
    assert_eq!(
        last.body.get("dormant").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        last.body.get("cause").and_then(|v| v.as_str()),
        Some("wake_class"),
        "the step names what reactivated the lane: {:?}",
        last.body
    );
    assert!(
        last.cites
            .iter()
            .any(|c| c.r#ref.as_str().starts_with("msg:")),
        "and cites the trigger it woke for: {:?}",
        last.cites
    );
}

/// Idempotence in both directions: a second `/sleep` writes nothing new.
#[tokio::test]
async fn sleeping_twice_is_idempotent() {
    let t = tree(&["repo:bough"], &["class:ask"]).await;
    let _agent = t.sol().await;
    let first = t.sleep().await;
    let second = t.sleep().await;

    assert_eq!(first, second, "the second sleep is the first one's answer");
    assert_eq!(
        t.steps_of(bough_plugin_dormancy::STEP_TYPE).await.len(),
        1,
        "and appended no second step"
    );

    // The same on the way up.
    let req = || WakeUpRequest {
        agent: name("sol"),
        cause: ReactivateCause::Command,
        cites: Vec::new(),
        at: now(),
    };
    let up = t.dormancy.wake_up(req()).await.expect("reactivation");
    let again = t.dormancy.wake_up(req()).await.expect("reactivation");
    assert_eq!(up.step, again.step);
    assert_eq!(t.steps_of(bough_plugin_dormancy::STEP_TYPE).await.len(), 2);
}
