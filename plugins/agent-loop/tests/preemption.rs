//! V2 — §5's checkpoint-and-answer. Andrey's message starts its answer wake IMMEDIATELY, the
//! interrupted wake gets exactly one grace step to jot, the next wake of any kind resumes from
//! that jot, a preempted wake refreshes no about-line, and a message that arrives during an
//! answer wake joins it before the first streamed token and queues after it.

mod support;

use std::sync::Arc;

use bough_plugin_agent_loop::preempt::{self, Preemption, Running};
use bough_plugin_agents::AgentWakeEnd;
use bough_plugin_ledger::WakeId;
use parking_lot::Mutex;
use support::*;

/// Hold the model round open so a wake is provably IN FLIGHT when the message arrives.
fn hold(f: &Fixture) -> Arc<tokio::sync::Notify> {
    let gate = Arc::new(tokio::sync::Notify::new());
    *f.adapter.hold.lock() = Some(gate.clone());
    gate
}

/// Let every held round through, now and in future: a test that has made its point stops holding.
fn release(f: &Fixture, gate: &Arc<tokio::sync::Notify>) {
    *f.adapter.hold.lock() = None;
    gate.notify_waiters();
}

async fn wake_starts(f: &Fixture) -> usize {
    f.steps()
        .await
        .iter()
        .filter(|s| s.kind.as_str() == "wake/start")
        .count()
}

/// §5: "Andrey's message starts its answer wake IMMEDIATELY" — it does not wait for the running
/// wake, and it does not wait for the jot.
#[tokio::test]
async fn an_andrey_message_starts_its_answer_wake_immediately() {
    let f = Fixture::mounted().await;
    let gate = hold(&f);
    let (agent, _d) = f.agent("sol").await;

    // A drain wake is running and stuck inside its model round.
    agent
        .followup(ordinary("a push"))
        .await
        .expect("mail lands");
    for _ in 0..200 {
        if wake_starts(&f).await >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(wake_starts(&f).await, 1, "the drain wake is in flight");
    assert!(
        !f.steps()
            .await
            .iter()
            .any(|s| s.kind.as_str() == "wake/end"),
        "and it has not finished"
    );

    agent
        .followup(andrey("stop, look at this"))
        .await
        .expect("mail lands");
    for _ in 0..200 {
        if wake_starts(&f).await >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        wake_starts(&f).await >= 2,
        "his answer wake opened while the other was still running"
    );
    // The precise claim, read off the ledger's own order: his wake OPENED before the wake it
    // interrupted CLOSED. That is what "immediately, in parallel" means (§5).
    let steps = f.steps().await;
    let first_wake = steps
        .iter()
        .find(|s| s.kind.as_str() == "wake/start")
        .expect("the drain wake")
        .wake
        .clone();
    let answer_start = steps
        .iter()
        .find(|s| s.kind.as_str() == "wake/start" && s.wake != first_wake)
        .expect("his answer wake");
    let interrupted_end = steps
        .iter()
        .find(|s| s.kind.as_str() == "wake/end" && s.wake == first_wake);
    if let Some(end) = interrupted_end {
        assert!(
            answer_start.seq < end.seq,
            "his wake opened before the interrupted one closed"
        );
    }
    release(&f, &gate);
}

/// §5 + P2-D14: the interrupted wake gets exactly ONE grace step to jot, and a jot ALWAYS exists.
#[tokio::test]
async fn the_interrupted_wake_gets_exactly_one_grace_step_to_jot() {
    let f = Fixture::mounted().await;
    let gate = hold(&f);
    let (agent, _d) = f.agent("sol").await;
    agent
        .followup(ordinary("a push"))
        .await
        .expect("mail lands");
    for _ in 0..200 {
        if wake_starts(&f).await >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    agent.followup(andrey("stop")).await.expect("mail lands");

    let jot = f.wait_for_kind("wake/jot").await;
    assert_eq!(
        f.steps()
            .await
            .iter()
            .filter(|s| s.kind.as_str() == "wake/jot")
            .count(),
        1,
        "exactly one grace step, not a stream of them"
    );
    assert!(
        jot.body["state"].as_str().is_some(),
        "and it says where the work stood: {}",
        jot.body
    );
    release(&f, &gate);
}

/// P2-D14: "a jot ALWAYS exists" — a grace step that cannot produce one still leaves a synthetic
/// jot, built deterministically from the wake's last thought steps.
#[tokio::test]
async fn a_failed_grace_step_still_leaves_a_synthetic_jot() {
    let mut cfg = config();
    // No time at all for the grace round: the model cannot answer in it.
    cfg.grace_deadline_ms = 1;
    let f = Fixture::with_config(cfg).await;
    let gate = hold(&f);
    let (agent, _d) = f.agent("sol").await;
    agent
        .followup(ordinary("a push"))
        .await
        .expect("mail lands");
    for _ in 0..200 {
        if wake_starts(&f).await >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    agent.followup(andrey("stop")).await.expect("mail lands");

    let jot = f.wait_for_kind("wake/jot").await;
    assert_eq!(
        jot.body["synthetic"], true,
        "the loop wrote the jot itself: {}",
        jot.body
    );
    assert!(!jot.body["state"].as_str().unwrap().is_empty());
    release(&f, &gate);
}

/// §5: "the jot lets the next wake of ANY kind resume", and a preempted wake refreshes no
/// about-line (`agent/wake-end` is dispatched for COMPLETED wakes only).
#[tokio::test]
async fn the_next_wake_of_any_kind_resumes_from_the_jot() {
    let f = Fixture::mounted().await;
    let refreshes = Arc::new(Mutex::new(Vec::<String>::new()));
    let r = refreshes.clone();
    f.ctx
        .on_parallel::<AgentWakeEnd, _, _>(move |e| {
            let r = r.clone();
            async move {
                r.lock().push(format!("{:?}", e.reason));
            }
        })
        .await
        .expect("the listener registers");

    let gate = hold(&f);
    let (agent, _d) = f.agent("sol").await;
    agent
        .followup(ordinary("a push"))
        .await
        .expect("mail lands");
    for _ in 0..200 {
        if wake_starts(&f).await >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    agent.followup(andrey("stop")).await.expect("mail lands");
    f.wait_for_kind("wake/jot").await;
    release(&f, &gate);

    // The next wake of ANY kind opens with `wake/resumed`.
    agent
        .followup(ordinary("another push"))
        .await
        .expect("mail lands");
    let resumed = f.wait_for_kind("wake/resumed").await;
    assert!(resumed.body["from_jot"].as_str().is_some());

    // A preempted wake refreshes nothing: no `completed` about-line moment for it.
    let seen = refreshes.lock().clone();
    assert!(
        seen.iter().all(|r| r == "Completed"),
        "wake-end fires for COMPLETED wakes only, saw {seen:?}"
    );
    let ends = f.steps().await;
    assert!(
        ends.iter()
            .any(|s| s.kind.as_str() == "wake/end" && s.body["reason"] == "interrupted"),
        "and the interrupted wake did close, durably"
    );
}

/// §5's cutoff, as the DATA that decides it: "started responding" means the first reply token has
/// streamed. Before it a message joins the running answer wake; after it, it queues as the next
/// wake's first mail.
///
/// DEVIATION, stated plainly: this loop implements JOIN by leaving the message for the answer
/// wake's next step boundary rather than by cancelling and restarting the step
/// (`step/end { outcome: restarted }`, P2-D15). The decision is the one §5 draws; the restart is
/// not built. See the WP-4 report.
#[test]
fn a_message_before_the_first_token_joins_and_after_it_queues() {
    let running = WakeId::new("w1");
    let msg = bough_plugin_agent_loop::testing::andrey("m", "hi");
    assert_eq!(
        preempt::decide(
            &msg,
            Some(Running {
                wake: &running,
                is_answer: true,
                streamed: false,
            }),
            WakeId::new("w2"),
        ),
        Some(Preemption::Join {
            wake: running.clone()
        })
    );
    assert_eq!(
        preempt::decide(
            &msg,
            Some(Running {
                wake: &running,
                is_answer: true,
                streamed: true,
            }),
            WakeId::new("w2"),
        ),
        Some(Preemption::Queue),
    );
}

/// The other half of the same rule, live: a second message never opens a SECOND answer wake while
/// one is running — it is answered by the wake that is already open, or by the next one.
#[tokio::test]
async fn a_message_during_an_answer_wake_does_not_open_a_second_one() {
    let f = Fixture::mounted().await;
    let gate = hold(&f);
    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("first")).await.expect("mail lands");
    for _ in 0..200 {
        if wake_starts(&f).await >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    agent.followup(andrey("second")).await.expect("mail lands");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(wake_starts(&f).await, 1, "one answer wake at a time (§5)");

    release(&f, &gate);
    // And the second message is not lost: it gets its wake once the first is done.
    let steps = f.wait_for_wake_ends(2).await;
    assert!(
        steps
            .iter()
            .filter(|s| s.kind.as_str() == "wake/start")
            .count()
            >= 2,
        "the queued message got its own wake"
    );
}
