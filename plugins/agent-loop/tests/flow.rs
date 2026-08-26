//! §5's wake flow, asserted against the DURABLE LEDGER: a rejected `agent/pre-step` still closes
//! a wake that spent no step, claimed messages a decision omits stay removed, `request/header` is
//! appended only when it changes, a `concludes_wake` tool result ends the wake at its step, a
//! `wake-stopping` listener that steers runs another step and listener ORDER does not change the
//! outcome, and a plugin failure ends the wake and not the loop.

mod support;

use std::sync::Arc;

use bough_kernel::ListenerOpts;
use bough_plugin_agents::{AgentPreStep, AgentWakeStopping, PreStepDecision, Status};
use bough_plugin_llm::{Chunk, StopReason, ToolCallId, ToolName};
use parking_lot::Mutex;
use support::*;

fn kinds_of(steps: &[bough_plugin_ledger::Step]) -> Vec<&str> {
    steps.iter().map(|s| s.kind.as_str()).collect()
}

/// §5: "a rejected or emptied first claim still closes a durable wake that spent no step."
#[tokio::test]
async fn a_rejected_pre_step_still_closes_a_durable_wake_that_spent_no_step() {
    let f = Fixture::mounted().await;
    f.ctx
        .on_waterfall::<AgentPreStep, _, _>(|mut pre, _next| async move {
            pre.decision = PreStepDecision::Reject {
                reason: "not now".into(),
            };
            pre
        })
        .await
        .expect("the listener registers");

    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("hello")).await.expect("mail lands");
    let steps = f.wait_for_wake_ends(1).await;

    let kinds = kinds_of(&steps);
    assert!(kinds.contains(&"wake/start"), "{kinds:?}");
    assert!(kinds.contains(&"wake/end"), "{kinds:?}");
    assert!(
        !kinds.contains(&"step/start"),
        "a rejected pre-step spends no step: {kinds:?}"
    );
    let end = steps
        .iter()
        .find(|s| s.kind.as_str() == "wake/end")
        .unwrap();
    assert_eq!(end.body["reason"], "completed");
    assert!(
        f.adapter.requests().is_empty(),
        "and no model call was made"
    );
}

/// §5: "claimed messages the decision omits STAY REMOVED."
#[tokio::test]
async fn claimed_messages_a_decision_omits_stay_removed() {
    let f = Fixture::mounted().await;
    f.ctx
        .on_waterfall::<AgentPreStep, _, _>(|mut pre, _next| async move {
            // Enter with NOTHING: the claim already happened and is durable.
            pre.decision = PreStepDecision::Enter { messages: vec![] };
            pre
        })
        .await
        .expect("the listener registers");

    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("hello")).await.expect("mail lands");
    f.wait_for_wake_ends(1).await;

    let steps = f.steps().await;
    let claims: Vec<_> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "inbox/spliced" && s.body["op"] == "claim")
        .collect();
    assert_eq!(claims.len(), 1, "the message was claimed durably");
    assert!(
        agent.inbox().is_empty(),
        "and it did NOT go back into the inbox"
    );
}

/// §5: `request/header` is durable "only when it changes" — where "it" is everything the header
/// records. Since this phase's review that includes the projection digest, because V4 anchors a
/// step's system prefix on the newest header at or before it: a step whose prefix moved with no
/// header for it is a prefix nothing in the ledger describes. So a header appears exactly when
/// the previous one no longer describes the request, and never twice with the same content.
#[tokio::test]
async fn a_request_header_is_appended_only_when_it_changes() {
    let f = Fixture::mounted().await;
    // Two rounds: a tool call, then plain text. Same prompt version, sections, tools and call
    // config, so exactly ONE header covers both steps.
    f.adapter.script(vec![
        vec![
            Chunk::ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("nope"),
                input: serde_json::json!({}),
            },
            Chunk::End {
                stop: StopReason::ToolUse,
            },
        ],
        says("done"),
    ]);
    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("go")).await.expect("mail lands");
    let steps = f.wait_for_wake_ends(1).await;

    let headers = steps
        .iter()
        .filter(|s| s.kind.as_str() == "request/header")
        .count();
    let starts = steps
        .iter()
        .filter(|s| s.kind.as_str() == "step/start")
        .count();
    assert!(starts >= 2, "two steps ran: {:?}", kinds_of(&steps));
    assert!(
        headers <= starts,
        "never more headers than steps: {headers} headers, {starts} steps"
    );
    // No two consecutive headers say the same thing: that is the whole of "only when it changes".
    let bodies: Vec<serde_json::Value> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "request/header")
        .map(|s| {
            let mut b = (*s.body).clone();
            // `as_of` and `budget` move with every append and are deliberately outside the
            // comparison; blank them so this asserts the compared fields only.
            if let Some(o) = b.as_object_mut() {
                o.remove("as_of");
                o.remove("budget");
            }
            b
        })
        .collect();
    for w in bodies.windows(2) {
        assert_ne!(w[0], w[1], "a header repeated an unchanged header");
    }
    // The four §5 names did NOT change across the two steps: same prompt version, sections,
    // tool schemas and call config. Only the projection digest moved.
    for w in bodies.windows(2) {
        for field in ["prompt_ver", "sections", "tools", "tools_digest", "call"] {
            assert_eq!(w[0][field], w[1][field], "`{field}` did not change");
        }
        assert_ne!(
            w[0]["projection_digest"], w[1]["projection_digest"],
            "the tool call and its result moved the projection"
        );
    }
}

/// §5: "a tool result carrying `concludes_wake` ends the wake at its step."
#[tokio::test]
async fn a_concludes_wake_tool_result_ends_the_wake_at_its_step() {
    let f = Fixture::mounted().await;
    f.tools
        .register(&f.ctx, support::concluding_tool())
        .await
        .expect("the tool registers");
    f.adapter.script(vec![
        vec![
            Chunk::ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("finish"),
                input: serde_json::json!({}),
            },
            Chunk::End {
                stop: StopReason::ToolUse,
            },
        ],
        says("should never run"),
    ]);
    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("go")).await.expect("mail lands");
    let steps = f.wait_for_wake_ends(1).await;

    let result = steps
        .iter()
        .find(|s| s.kind.as_str() == "tool/result")
        .expect("the tool ran");
    assert_eq!(result.body["concludes_wake"], true);
    assert_eq!(
        steps
            .iter()
            .filter(|s| s.kind.as_str() == "step/start")
            .count(),
        1,
        "the wake ended AT that step: {:?}",
        kinds_of(&steps)
    );
    assert_eq!(f.adapter.requests().len(), 1, "no second model call");
}

/// §5 + P2-D10: a `wake-stopping` listener that steers runs another step, and listener ORDER does
/// not change the outcome, because the DATA (the inbox) decides.
#[tokio::test]
async fn a_wake_stopping_listener_that_steers_runs_another_step() {
    for order in ["steer-first", "steer-last"] {
        let f = Fixture::mounted().await;
        let steered = Arc::new(Mutex::new(false));
        let quiet = Arc::new(Mutex::new(0usize));

        let s = steered.clone();
        let steering = move |w: bough_plugin_agents::WakeStopping| {
            let s = s.clone();
            async move {
                let first = {
                    let mut done = s.lock();
                    let first = !*done;
                    *done = true;
                    first
                };
                if first {
                    w.handle
                        .steer(ordinary("one more thing"))
                        .await
                        .expect("a steer lands");
                }
                None
            }
        };
        let q = quiet.clone();
        let counting = move |_w: bough_plugin_agents::WakeStopping| {
            let q = q.clone();
            async move {
                *q.lock() += 1;
                None
            }
        };

        if order == "steer-first" {
            f.ctx
                .on_serial::<AgentWakeStopping, _, _>(steering)
                .await
                .unwrap();
            f.ctx
                .on_serial::<AgentWakeStopping, _, _>(counting)
                .await
                .unwrap();
        } else {
            f.ctx
                .on_serial::<AgentWakeStopping, _, _>(counting)
                .await
                .unwrap();
            f.ctx
                .on_serial::<AgentWakeStopping, _, _>(steering)
                .await
                .unwrap();
        }

        let (agent, _d) = f.agent("sol").await;
        agent.followup(andrey("go")).await.expect("mail lands");
        let steps = f.wait_for_wake_ends(1).await;

        assert_eq!(
            steps
                .iter()
                .filter(|s| s.kind.as_str() == "step/start")
                .count(),
            2,
            "{order}: the steer ran a second step: {:?}",
            kinds_of(&steps)
        );
        assert_eq!(
            *quiet.lock(),
            2,
            "{order}: EVERY listener runs on every stopping dispatch (P2-D10)"
        );
    }
}

/// §5: "a plugin failure ends the current wake, not the loop."
#[tokio::test]
async fn a_plugin_failure_ends_the_wake_and_not_the_loop() {
    let f = Fixture::mounted().await;
    let boom = Arc::new(Mutex::new(true));
    let b = boom.clone();
    f.ctx
        .on_waterfall_with::<AgentPreStep, _, _>(ListenerOpts::default(), move |pre, next| {
            let b = b.clone();
            async move {
                if *b.lock() {
                    *b.lock() = false;
                    panic!("a listener blew up");
                }
                next.run(pre).await
            }
        })
        .await
        .expect("the listener registers");

    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("first")).await.expect("mail lands");
    f.wait_for_wake_ends(1).await;

    // The loop is still alive: the next message gets a whole wake of its own.
    agent.followup(andrey("second")).await.expect("mail lands");
    let steps = f.wait_for_wake_ends(2).await;
    assert!(
        steps
            .iter()
            .filter(|s| s.kind.as_str() == "wake/start")
            .count()
            >= 2,
        "the loop survived the failing plugin: {:?}",
        kinds_of(&steps)
    );
    for _ in 0..200 {
        if agent.status() == Status::Idle {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(agent.status(), Status::Idle);
}

/// §9 + §5: the durable record of a batch is in the MODEL's call order, even when the calls
/// finish in the opposite order. The tools seam returns results in call order; this asserts the
/// LEDGER — what the model reads back — keeps that order too.
#[tokio::test]
async fn durable_tool_results_stay_model_ordered_in_the_ledger() {
    use bough_plugin_tools::{
        RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome, ToolScope, ToolSpec,
    };

    struct Timed {
        tag: &'static str,
        delay: std::time::Duration,
        log: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl Tool for Timed {
        fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
            true
        }
        async fn call(
            &self,
            _call: Arc<ToolCall>,
            _cx: ToolCx,
        ) -> Result<ToolOutcome, ToolFailure> {
            tokio::time::sleep(self.delay).await;
            self.log.lock().push(self.tag.to_string());
            Ok(ToolOutcome {
                content: self.tag.to_string(),
                value: None,
                cites: vec![],
                concludes_wake: false,
            })
        }
    }
    fn timed(name: &'static str, ms: u64, log: Arc<Mutex<Vec<String>>>) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(name),
            description: format!("the {name} tool"),
            input_schema: schemars::json_schema!({ "type": "object" }),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(Timed {
                tag: name,
                delay: std::time::Duration::from_millis(ms),
                log,
            }),
        }
    }

    let f = Fixture::mounted().await;
    let done = Arc::new(Mutex::new(Vec::new()));
    f.tools
        .register(&f.ctx, timed("slow", 120, done.clone()))
        .await
        .expect("slow registers");
    f.tools
        .register(&f.ctx, timed("fast", 0, done.clone()))
        .await
        .expect("fast registers");

    f.adapter.script(vec![
        vec![
            Chunk::ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("slow"),
                input: serde_json::json!({}),
            },
            Chunk::ToolCall {
                id: ToolCallId::new("c2"),
                name: ToolName::new("fast"),
                input: serde_json::json!({}),
            },
            Chunk::End {
                stop: StopReason::ToolUse,
            },
        ],
        says("done"),
    ]);
    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("go")).await.expect("mail lands");
    let steps = f.wait_for_wake_ends(1).await;

    assert_eq!(
        done.lock().clone(),
        vec!["fast".to_string(), "slow".to_string()],
        "the calls really did complete in the REVERSE of call order"
    );
    let results: Vec<String> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "tool/result")
        .map(|s| s.body["name"].as_str().unwrap_or("?").to_string())
        .collect();
    assert_eq!(
        results,
        vec!["slow".to_string(), "fast".to_string()],
        "the durable steps are in the model's call order: {:?}",
        kinds_of(&steps)
    );
}

/// §2 / §2.5: the ambient INITIATOR is set for the whole wake — every waterfall the wake
/// dispatches inline sees it, the `llm/stream` tee among them. Without it a listener watching a
/// stream has no way to name the agent whose work it is watching (which is exactly what the focus
/// pane needs).
#[tokio::test]
async fn the_initiator_is_set_for_the_whole_wake_including_the_llm_stream_waterfall() {
    let f = Fixture::mounted().await;
    let seen_at_stream: Arc<Mutex<Vec<Option<bough_plugin_agents::AgentId>>>> = Default::default();
    let seen_at_pre_step: Arc<Mutex<Vec<Option<bough_plugin_agents::AgentId>>>> =
        Default::default();

    {
        let seen = seen_at_stream.clone();
        f.ctx
            .on_waterfall::<bough_plugin_llm::LlmStreamEvent, _, _>(move |call, next| {
                let seen = seen.clone();
                async move {
                    // Read it BEFORE delegating: the tee runs inline in the dispatching task, so
                    // the task-local is visible here or nowhere.
                    seen.lock().push(bough_plugin_agents::initiator::current());
                    next.run(call).await
                }
            })
            .await
            .expect("the tee registers");
    }
    {
        let seen = seen_at_pre_step.clone();
        f.ctx
            .on_waterfall::<AgentPreStep, _, _>(move |pre, next| {
                let seen = seen.clone();
                async move {
                    seen.lock().push(bough_plugin_agents::initiator::current());
                    next.run(pre).await
                }
            })
            .await
            .expect("the listener registers");
    }

    assert_eq!(
        bough_plugin_agents::initiator::current(),
        None,
        "outside a wake there is no initiator"
    );

    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("hello")).await.expect("mail lands");
    f.wait_for_wake_ends(1).await;

    let want = Some(agent.id().clone());
    assert_eq!(
        *seen_at_pre_step.lock(),
        vec![want.clone()],
        "the wake's first waterfall already runs under the initiator"
    );
    let streams = seen_at_stream.lock().clone();
    assert!(!streams.is_empty(), "the wake made a model call");
    assert!(
        streams.iter().all(|s| *s == want),
        "the llm/stream waterfall runs under the same initiator: {streams:?}"
    );
    assert_eq!(
        bough_plugin_agents::initiator::current(),
        None,
        "and it does not leak out of the wake"
    );
}

/// §2.5 / P3-D16 on the LIVE driver: nothing queued is nothing to do.
#[tokio::test]
async fn request_wake_with_nothing_queued_starts_no_wake() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f.agent("sol").await;
    let before = f.kinds().await;

    let req = agent
        .request_wake(
            bough_plugin_llm::WakeKind::Catchup,
            bough_plugin_agents::WakeCause::CatchUp,
        )
        .await;

    assert_eq!(req, bough_plugin_agents::WakeRequest::Nothing);
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert_eq!(
        f.kinds().await,
        before,
        "no wake/start, and no synthetic message"
    );
    assert_eq!(agent.status(), Status::Idle);
}

/// §2.5 / P3-D16 on the LIVE driver: queued mail gets exactly ONE catch-up wake, whose durable
/// urgency says what it was.
#[tokio::test]
async fn request_wake_with_queued_mail_starts_exactly_one() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f.agent("sol").await;
    // The catch-up SHAPE: ordinary mail is delivered and unconsumed in the ledger, and no
    // notification ever reached this driver — which is exactly the state a restart leaves behind.
    // Seeding the step directly is what lets the test own that boundary (P3-D14).
    f.ledger
        .0
        .append(bough_plugin_ledger::Append {
            traj: agent.traj().clone(),
            wake: bough_plugin_ledger::WakeId::new("wake:outside"),
            kind: bough_plugin_ledger::StepType::new("mail/delivered"),
            class: bough_plugin_ledger::Class::Evidence,
            body: serde_json::json!({
                "class": "ordinary",
                "from": "collector:github",
                "subject": "CI is red",
                "summary": "the delegate test failed again",
            }),
            cites: vec![bough_plugin_ledger::Cite {
                r#ref: bough_plugin_ledger::Ref::new("gh:bough/bough#12"),
                url: None,
            }],
            at: now(),
            id: None,
        })
        .await
        .expect("the seed appends");
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(
        !f.kinds().await.iter().any(|k| k == "wake/start"),
        "mail that never reached the driver starts nothing by itself"
    );

    let req = agent
        .request_wake(
            bough_plugin_llm::WakeKind::Catchup,
            bough_plugin_agents::WakeCause::CatchUp,
        )
        .await;
    let wake = match req {
        bough_plugin_agents::WakeRequest::Started(w) => w,
        bough_plugin_agents::WakeRequest::Nothing => panic!("queued mail is something to do"),
    };

    let steps = f.wait_for_wake_ends(1).await;
    let starts: Vec<_> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "wake/start")
        .collect();
    assert_eq!(starts.len(), 1, "exactly one wake, not two");
    assert_eq!(starts[0].wake, wake, "and it is the wake that was reported");
    assert_eq!(
        starts[0].body["urgency"], "catchup",
        "the durable urgency says it was a catch-up (§5)"
    );
}
