//! §5's mail rules, as V6 names them: consumption is a UNION (so concurrent wakes cannot regress
//! it), unconsumed ordinary mail implies a scheduled drain wake, one drain wake is in flight per
//! agent, an Andrey message always gets a fresh answer wake whatever queue it arrived through,
//! and a drain wake never answers him.

use crate::support;

use bough_plugin_agent_loop::mail;
use bough_plugin_agent_loop::testing::{delivered, ordinary as ordinary_msg, wake_end, wake_of};
use bough_plugin_agent_loop::LoopConfig;
use bough_plugin_ledger::{Seq, SeqRange};
use bough_plugin_llm::WakeKind;
use support::*;

fn range(from: u64, to: u64) -> SeqRange {
    SeqRange {
        from: Seq(from),
        to: Seq(to),
    }
}

/// §5: "consumed = the UNION of all wake_end sets."
#[test]
fn consumed_is_the_union_of_wake_end_sets() {
    let w = wake_of("w1");
    let ends = vec![
        wake_end(10, &w, "completed", &[(1, 2)]),
        wake_end(11, &w, "completed", &[(5, 5)]),
        wake_end(12, &w, "completed", &[(3, 4)]),
    ];
    assert_eq!(mail::consumed_union(&ends), vec![range(1, 5)]);
}

/// §5: "Union is order-independent, so concurrent wakes can never regress consumption."
#[test]
fn concurrent_wakes_over_disjoint_seqs_never_regress_consumption() {
    let w = wake_of("w1");
    let answer = wake_end(20, &w, "completed", &[(7, 7)]);
    let drain = wake_end(21, &w, "completed", &[(1, 3)]);
    let one_order = mail::consumed_union(&[answer.clone(), drain.clone()]);
    let other_order = mail::consumed_union(&[drain, answer]);
    assert_eq!(one_order, other_order);
    assert_eq!(one_order, vec![range(1, 3), range(7, 7)]);
    // And neither order un-consumes what the other consumed.
    for r in [range(1, 3), range(7, 7)] {
        assert!(one_order.contains(&r));
    }
}

/// §5's standing invariant, live and at the moment it BITES: ordinary mail arrives, and while it
/// is still unconsumed a drain wake is already scheduled — then the drain runs on its own,
/// consumes it, and the invariant holds again for the opposite reason.
#[tokio::test]
async fn unconsumed_ordinary_mail_implies_a_scheduled_drain_wake() {
    // A long debounce so the "scheduled but not yet run" window is observable rather than raced.
    let f = Fixture::with_config(LoopConfig {
        drain_debounce_ms: 400,
        ..config()
    })
    .await;
    let (agent, _d) = f.agent("sol").await;
    agent
        .followup(ordinary("a push landed"))
        .await
        .expect("mail lands");

    // Phase 1: the message is durably in the inbox and NOT yet consumed by any wake — so a
    // drain must already be scheduled. (`mail/delivered` is written BY the wake, so the only
    // durable trace of unconsumed mail at this instant is `inbox/spliced`.)
    let spliced = f.wait_for_kind("inbox/spliced").await;
    assert_eq!(spliced.body["op"], "insert");
    assert!(
        spliced.body.to_string().contains("a push landed"),
        "the durable envelope carries the message: {}",
        spliced.body
    );
    let mid = f.steps().await;
    assert!(
        mid.iter().all(|s| s.kind.as_str() != "wake/end"),
        "the debounce window has not closed yet: {:?}",
        f.kinds().await
    );
    assert!(
        mail::consumed_union(&mid).is_empty(),
        "nothing has been consumed yet"
    );
    assert!(
        bough_plugin_agent_loop::driver::any_drain_scheduled(f.ctx.fiber_uid()),
        "unconsumed ordinary mail with no drain scheduled"
    );

    // Phase 2: the drain runs by itself, is COALESCED (never an answer), and consumes that seq.
    let steps = f.wait_for_wake_ends(1).await;
    let start = steps
        .iter()
        .find(|s| s.kind.as_str() == "wake/start")
        .expect("a wake ran");
    assert_eq!(
        start.body["urgency"], "coalesced",
        "ordinary mail drains, it does not answer"
    );
    let delivered_step = steps
        .iter()
        .find(|s| s.kind.as_str() == "mail/delivered")
        .expect("the drain delivered the mail");
    let consumed = mail::consumed_union(&steps);
    assert!(
        consumed.iter().any(|r| r.contains(delivered_step.seq)),
        "the drain consumed the delivered seq: consumed={consumed:?} seq={:?}",
        delivered_step.seq
    );
    let left = mail::unconsumed(&steps, &consumed)
        .into_iter()
        .filter(mail::is_ordinary)
        .count();
    assert_eq!(
        left, 0,
        "nothing ordinary is left unconsumed after the drain"
    );
}

/// §5: "One drain wake in flight per agent."
#[tokio::test]
async fn only_one_drain_wake_is_in_flight_per_agent() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f.agent("sol").await;
    for i in 0..3 {
        agent
            .followup(ordinary(&format!("push {i}")))
            .await
            .expect("mail lands");
    }
    let steps = f.wait_for_wake_ends(1).await;
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let steps2 = f.steps().await;
    let drains = steps2
        .iter()
        .filter(|s| s.kind.as_str() == "wake/start" && s.body["urgency"] == "coalesced")
        .count();
    assert_eq!(
        drains,
        1,
        "three ordinary messages coalesce into ONE drain wake: {:?}",
        steps.len()
    );
}

/// §5: "An Andrey message ALWAYS gets a fresh sol answer wake, whatever queue it arrived through."
#[tokio::test]
async fn an_andrey_message_gets_a_fresh_answer_wake_from_either_queue() {
    for (queue, send) in [("next-wake", true), ("next-step", false)] {
        let f = Fixture::mounted().await;
        let (agent, _d) = f.agent("sol").await;
        if send {
            agent.followup(andrey("hello")).await.expect("mail lands");
        } else {
            agent.steer(andrey("hello")).await.expect("mail lands");
        }
        let steps = f.wait_for_wake_ends(1).await;
        let start = steps
            .iter()
            .find(|s| s.kind.as_str() == "wake/start")
            .expect("a wake ran");
        assert_eq!(
            start.body["urgency"], "immediate",
            "{queue}: his message never waits"
        );
        assert!(
            steps.iter().any(|s| s.kind.as_str() == "step/start"),
            "{queue}: and it is actually answered"
        );
    }
}

/// §5: "drain and tick wakes never answer him" — a drain claims ORDINARY seqs only.
#[test]
fn a_drain_wake_never_answers_andrey() {
    let sel = mail::selector_for(WakeKind::Drain, bough_plugin_agents::Target::NextWake);
    assert_eq!(
        sel.classes,
        Some(vec![bough_plugin_agents::MailClass::Ordinary])
    );
    assert!(!mail::admits(
        WakeKind::Drain,
        &bough_plugin_agent_loop::testing::andrey("m", "hi")
    ));
    assert!(mail::admits(WakeKind::Drain, &ordinary_msg("m", None)));
    // An answer wake, by contrast, claims its TRIGGER and nothing else.
    let answer = mail::only_the_trigger(
        mail::selector_for(WakeKind::Answer, bough_plugin_agents::Target::NextWake),
        &bough_plugin_agents::MessageId::new("m7"),
    );
    assert_eq!(
        answer.only.map(|v| v.len()),
        Some(1),
        "an answer wake reads only the message that triggered it"
    );
    let _ = delivered(1, &wake_of("w"), "ordinary", "x");
}
