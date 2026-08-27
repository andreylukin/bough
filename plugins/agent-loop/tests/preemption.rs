//! V2 — §5's checkpoint-and-answer. Andrey's message starts its answer wake IMMEDIATELY, the
//! interrupted wake gets exactly one grace step to jot, the next wake of any kind resumes from
//! that jot, a preempted wake refreshes no about-line, and a message that arrives during an
//! answer wake joins it before the first streamed token and queues after it.

use crate::support;

use std::sync::Arc;

use bough_plugin_agent_loop::preempt::{self, Preemption, Running};
use bough_plugin_agents::AgentWakeEnd;
use bough_plugin_ledger::WakeId;
use bough_plugin_llm::{Chunk, StopReason, ToolCallId, ToolName};
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

/// §5's cutoff as the pure DECISION: "started responding" means the first reply token has
/// streamed. Before it a message joins the running answer wake; after it, it queues.
/// The two tests below drive the same rule through the real loop.
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

/// §5, live: a message that arrives BEFORE the first streamed token joins the running answer
/// wake. The evidence is durable and cannot be faked by a flag — the inbox splice that CLAIMED
/// the second message carries the FIRST wake's id, and no second wake ever opened.
///
/// DEVIATION, stated plainly: the join happens at the running wake's next STEP boundary. P2-D15's
/// stronger form (cancel the in-flight request and append `step/end { outcome: restarted }`) is
/// not built; `StepOutcome::Restarted` is still unused.
#[tokio::test]
async fn a_message_before_the_first_token_joins_the_answer_wake() {
    let f = Fixture::mounted().await;
    let gate = hold(&f);
    let (agent, _d) = f.agent("sol").await;

    let first = agent.followup(andrey("first")).await.expect("mail lands");
    for _ in 0..200 {
        if wake_starts(&f).await >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // The round is held open BEFORE its first chunk, so not one token has streamed.
    let second = agent.followup(andrey("second")).await.expect("mail lands");
    release(&f, &gate);

    let w1 = f.wait_for_claim(&first.message).await;
    let w2 = f.wait_for_claim(&second.message).await;
    assert_eq!(
        w1, w2,
        "the second message was claimed by the wake that was already answering"
    );
    f.wait_for_wake_ends(1).await;
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert_eq!(
        wake_starts(&f).await,
        1,
        "and no second answer wake opened: {:?}",
        f.kinds().await
    );
    // The join is a real second STEP of that wake, not a relabelling.
    let steps = f.steps().await;
    assert!(
        steps
            .iter()
            .filter(|s| s.kind.as_str() == "step/start")
            .count()
            >= 2,
        "the joined message got its own step: {:?}",
        f.kinds().await
    );
}

/// §5, live: a message that arrives AFTER the first token has streamed does NOT join — it queues
/// as next-wake mail and is claimed by a LATER wake. The wake is held open past its first token
/// by a blocking tool, so "still running" is a fact and not a race.
#[tokio::test]
async fn a_message_after_the_first_token_queues_as_next_wake_mail() {
    let f = Fixture::mounted().await;
    let tool_gate = Arc::new(tokio::sync::Notify::new());
    f.tools
        .register(&f.ctx, gated_tool(tool_gate.clone()))
        .await
        .expect("the tool registers");
    f.adapter.script(vec![
        vec![
            Chunk::TextDelta {
                text: "on it".into(),
            },
            Chunk::ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("gate"),
                input: serde_json::json!({}),
            },
            Chunk::End {
                stop: StopReason::ToolUse,
            },
        ],
        says("done"),
    ]);

    let (agent, _d) = f.agent("sol").await;
    let first = agent.followup(andrey("first")).await.expect("mail lands");
    // Wait for the token to be durable: `thought/text` IS the streamed first token.
    f.wait_for_kind("thought/text").await;
    let second = agent.followup(andrey("second")).await.expect("mail lands");
    tool_gate.notify_waiters();

    let w1 = f.wait_for_claim(&first.message).await;
    let w2 = f.wait_for_claim(&second.message).await;
    assert_ne!(
        w1, w2,
        "the late message was NOT taken by the wake that had already started responding"
    );
    let steps = f.wait_for_wake_ends(2).await;
    assert!(
        steps
            .iter()
            .any(|s| s.kind.as_str() == "wake/start" && s.wake.to_string() == w2),
        "it got a wake of its own"
    );
}

/// §5 + P2-D11: a preempted wake refreshes NO about-line. The real `about-line` refresh is wired
/// to the same `agent/wake-end` moment the plugin registers it on, so the absence below is the
/// plugin's own behaviour and not a stubbed-out branch — the positive control in the same test
/// proves the wiring writes a line when a wake DOES complete.
#[tokio::test]
async fn a_preempted_wake_skips_its_about_line_refresh() {
    let f = Fixture::mounted().await;
    for def in bough_plugin_about_line::step_types() {
        f.ledger.0.register_step_type(def).expect("a fresh type");
    }
    let cfg = Arc::new(bough_plugin_about_line::AboutConfig {
        max_state_chars: 400,
        max_intent_chars: 200,
    });
    let l = f.ledger.clone();
    f.ctx
        .on_parallel::<AgentWakeEnd, _, _>(move |ended| {
            let (l, cfg) = (l.clone(), cfg.clone());
            async move {
                let _ = bough_plugin_about_line::refresh(
                    &l,
                    &cfg,
                    &ended.wake,
                    ended.reason,
                    &ended.end_step,
                )
                .await;
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
    let interrupted = f
        .steps()
        .await
        .iter()
        .find(|s| s.kind.as_str() == "wake/start")
        .expect("the drain wake")
        .wake
        .clone();
    agent.followup(andrey("stop")).await.expect("mail lands");
    f.wait_for_kind("wake/jot").await;
    release(&f, &gate);

    // The positive control: SOME wake completes and writes a line, so an empty ledger cannot pass.
    let line = f.wait_for_kind("about/line").await;
    assert_ne!(
        line.wake, interrupted,
        "the line belongs to a completed wake"
    );
    let interrupted_end = f
        .steps()
        .await
        .into_iter()
        .find(|s| s.kind.as_str() == "wake/end" && s.wake == interrupted)
        .expect("the interrupted wake closed");
    assert_eq!(interrupted_end.body["reason"], "interrupted");
    assert!(
        !f.steps()
            .await
            .iter()
            .any(|s| s.kind.as_str() == "about/line" && s.wake == interrupted),
        "and it refreshed no about-line: {:?}",
        f.kinds().await
    );
}

/// The other half of the same rule, live: a second message never opens a SECOND answer wake
/// while one is running — it is answered by the wake that is already open (a join, before the
/// first token) or by the next one (a queue, after it). Either way it is never lost.
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
    let second = agent.followup(andrey("second")).await.expect("mail lands");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(wake_starts(&f).await, 1, "one answer wake at a time (§5)");

    release(&f, &gate);
    // And the second message is not lost: some wake claims it.
    f.wait_for_claim(&second.message).await;
}

/// §5: "an interrupt stops the wake producing" — and that has to reach a tool that is ALREADY
/// running. The tool below never returns on its own; only the cancellation ends it, so a green
/// here is the signal arriving rather than a deadline expiring (the fixture's deadline is far
/// longer than this test's patience).
#[tokio::test]
async fn an_interrupt_reaches_a_tool_that_is_already_running() {
    let f = Fixture::mounted().await;
    // A gate nobody ever notifies: the tool returns only if it is cancelled.
    let never = Arc::new(tokio::sync::Notify::new());
    f.tools
        .register(&f.ctx, gated_tool(never))
        .await
        .expect("the tool registers");
    f.adapter.script(vec![
        vec![
            Chunk::ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("gate"),
                input: serde_json::json!({}),
            },
            Chunk::End {
                stop: StopReason::ToolUse,
            },
        ],
        says("answered"),
    ]);

    let (agent, _d) = f.agent("sol").await;
    agent
        .followup(ordinary("a push"))
        .await
        .expect("mail lands");
    // The tool is running: its call is durable and no result has been written.
    f.wait_for_kind("tool/call").await;
    agent.followup(andrey("stop")).await.expect("mail lands");

    let result = f.wait_for_kind("tool/result").await;
    assert_eq!(
        result.body["outcome"], "error",
        "the cancelled tool answered with a failure rather than hanging: {}",
        result.body
    );
    assert!(
        result.body["content"]
            .as_str()
            .unwrap_or_default()
            .contains("cancelled"),
        "and the failure says why: {}",
        result.body
    );
}

/// §2: `status` is the DRIVER-WIDE drain interval, and `when_idle()` means every wake is over.
/// Checkpoint-and-answer deliberately runs two wakes at once, and per-wake status transitions
/// made the FIRST finisher publish `Idle` over a wake that was still open — `bough exec` awaits
/// `when_idle()` and would have printed and torn down a half-finished agent.
#[tokio::test]
async fn when_idle_does_not_return_while_a_second_wake_is_still_open() {
    let f = Fixture::mounted().await;
    // A tool nobody releases keeps the FIRST wake open past the answer wake's whole life.
    let never = Arc::new(tokio::sync::Notify::new());
    f.tools
        .register(&f.ctx, gated_tool(never.clone()))
        .await
        .expect("the tool registers");
    f.adapter.script(vec![
        vec![
            Chunk::ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("gate"),
                input: serde_json::json!({}),
            },
            Chunk::End {
                stop: StopReason::ToolUse,
            },
        ],
        says("answered"),
    ]);

    let (agent, _d) = f.agent("sol").await;
    agent
        .followup(ordinary("a push"))
        .await
        .expect("mail lands");
    f.wait_for_kind("tool/call").await;
    assert_eq!(
        agent.status(),
        bough_plugin_agents::Status::Running,
        "a wake is open"
    );

    // Two wakes are now in flight; the answer wake will finish first.
    agent.followup(andrey("stop")).await.expect("mail lands");
    let idle = {
        let a = agent.clone();
        tokio::spawn(async move { a.when_idle().await })
    };
    f.wait_for_kind("wake/jot").await;

    // Both wakes are over before `when_idle()` may return.
    tokio::time::timeout(std::time::Duration::from_secs(5), idle)
        .await
        .expect("the agent goes idle once both wakes are over")
        .expect("the task joins");
    let steps = f.steps().await;
    assert_eq!(
        steps
            .iter()
            .filter(|s| s.kind.as_str() == "wake/end")
            .count(),
        2,
        "both wakes closed durably before idle: {:?}",
        f.kinds().await
    );
}
