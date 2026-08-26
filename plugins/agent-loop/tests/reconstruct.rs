//! V4 — model-visible ⟺ ledgered (§0.2). Every request of a multi-step wake rebuilds from the
//! ledger byte for byte, and a PLANTED side channel (an `llm/stream` listener that appends to
//! `messages`) makes the invariant report.

mod support;

use bough_plugin_agent_loop::invariant;
use bough_plugin_llm::{
    Chunk, LlmContentBlock, LlmMessage, LlmRole, StopReason, ToolCallId, ToolName,
};
use support::*;

/// A two-step wake: a tool call, then an answer. BOTH requests must rebuild.
#[tokio::test]
async fn every_request_of_a_wake_reconstructs_byte_for_byte() {
    let f = Fixture::mounted().await;
    f.tools
        .register(&f.ctx, support::echo_tool())
        .await
        .expect("the tool registers");
    f.adapter.script(vec![
        vec![
            Chunk::TextDelta {
                text: "let me look".into(),
            },
            Chunk::ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("echo"),
                input: serde_json::json!({ "text": "hi" }),
            },
            Chunk::End {
                stop: StopReason::ToolUse,
            },
        ],
        says("all done"),
    ]);
    let (agent, _d) = f.agent("sol").await;
    agent
        .followup(andrey("look at it"))
        .await
        .expect("mail lands");
    let steps = f.wait_for_wake_ends(1).await;

    assert_eq!(f.adapter.requests().len(), 2, "two model rounds ran");
    let sent = recorded_for(&steps);
    assert_eq!(sent.len(), 2, "and both were recorded");
    invariant::evaluate_reconstruction(&sent, &steps)
        .expect("every request rebuilds from the ledger, byte for byte");
    // The second request really does carry the first round's work: this is not a vacuous pass.
    let second = &f.adapter.requests()[1];
    let text = format!("{:?}", second.messages);
    assert!(text.contains("let me look"), "{text}");
    assert!(
        text.contains("hi"),
        "the tool result is in the transcript: {text}"
    );
}

/// The planted side channel V4 asks for: a listener that adds a message the ledger never saw.
#[tokio::test]
async fn a_side_channel_message_makes_the_invariant_report() {
    let f = Fixture::mounted().await;
    // The model sees a message the ledger never recorded — the exact shape of a side channel,
    // however it got there (a stream wrapper, an adapter, a loop that trusted its listeners).
    *f.adapter.inject.lock() = Some(LlmMessage {
        role: LlmRole::User,
        content: vec![LlmContentBlock::Text {
            text: "a message that never was a step".into(),
        }],
    });

    let (agent, _d) = f.agent("sol").await;
    agent.followup(andrey("hello")).await.expect("mail lands");
    let steps = f.wait_for_wake_ends(1).await;

    // What the ADAPTER saw is the side-channelled request; record that, exactly as a loop that
    // trusted its listeners would have.
    let sent: Vec<invariant::SentRequest> = recorded_for(&steps)
        .into_iter()
        .zip(f.adapter.requests())
        .map(|(mut s, actually_sent)| {
            s.request = actually_sent;
            s
        })
        .collect();
    let err = invariant::evaluate_reconstruction(&sent, &steps)
        .expect_err("a message that reached the model without a step must be reported");
    assert!(err.contains("does not reconstruct"), "{err}");
}

/// A section that appears MID-WAKE (a `projection/assemble` listener that starts contributing on
/// the second step) changes the system prefix under the model's feet. §5 appends a fresh
/// `request/header` when the section set changes, and the plan's third V4 case is that this must
/// not retroactively break step 0: each step is checked against the header that belongs to IT.
#[tokio::test]
async fn a_contributed_section_added_mid_wake_does_not_break_a_past_reconstruction() {
    use bough_plugin_projection::section::{Place, Position, Slot};
    use bough_plugin_projection::{ProjectionAssemble, RenderedSection, SectionId};

    let f = Fixture::mounted().await;
    f.tools
        .register(&f.ctx, support::echo_tool())
        .await
        .expect("the tool registers");

    // Contribute nothing to the first assemble, a section to every one after it.
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = seen.clone();
    f.ctx
        .on_waterfall::<ProjectionAssemble, _, _>(move |mut draft, next| {
            let counter = counter.clone();
            async move {
                if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                    draft.sections.push(RenderedSection {
                        id: SectionId::new("late-arrival"),
                        position: Position {
                            slot: Slot::Identity,
                            place: Place::Band,
                        },
                        title: "late arrival".into(),
                        body: "a section that did not exist at step 0".into(),
                        cites: Default::default(),
                        tokens: 8,
                        degraded: None,
                    });
                }
                next.run(draft).await
            }
        })
        .await
        .expect("the listener registers");

    f.adapter.script(vec![
        vec![
            Chunk::ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("echo"),
                input: serde_json::json!({ "text": "hi" }),
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

    // The section really did arrive mid-wake: the second request's system prefix carries it and
    // the first one's does not.
    let reqs = f.adapter.requests();
    assert_eq!(reqs.len(), 2, "two model rounds ran");
    assert!(
        !reqs[0]
            .system
            .as_deref()
            .unwrap_or("")
            .contains("late arrival"),
        "step 0 must predate the section"
    );
    assert!(
        reqs[1]
            .system
            .as_deref()
            .unwrap_or("")
            .contains("late arrival"),
        "step 1 must carry it: {:?}",
        reqs[1].system
    );
    // And §5 wrote a second header for it, so each step has its own anchor.
    let headers = steps
        .iter()
        .filter(|s| s.kind.as_str() == "request/header")
        .count();
    assert_eq!(headers, 2, "the changed section set gets a fresh header");

    let sent = recorded_for(&steps);
    assert_eq!(sent.len(), 2);
    invariant::evaluate_reconstruction(&sent, &steps)
        .expect("step 0 still reconstructs against ITS header, not the later one");
}

/// V4 over the GRACE step. It used to build its own request and call the adapter directly: a
/// model-visible input on a side channel, invisible to this check and with an EMPTY model, so
/// `model-policy` never saw it. It is now a real step of the interrupted wake — its instruction
/// is durable, it runs the `agent/request` waterfall, and its request reconstructs like any other.
#[tokio::test]
async fn the_grace_step_is_ledgered_and_runs_the_agent_request_waterfall() {
    use bough_plugin_llm::AgentRequest;

    let f = Fixture::mounted().await;
    // A listener in `model-policy`'s position: whatever it decides is what must reach the adapter.
    f.ctx
        .on_waterfall::<AgentRequest, _, _>(
            |mut value: bough_plugin_llm::RequestCall, next| async move {
                value.call.model = "decided-by-policy".to_string();
                next.run(value).await
            },
        )
        .await
        .expect("the listener registers");
    // A tool nobody releases holds the first wake open, so Andrey's message provably preempts a
    // wake that is IN FLIGHT — without holding the adapter, which the grace round also needs.
    let never = std::sync::Arc::new(tokio::sync::Notify::new());
    f.tools
        .register(&f.ctx, support::gated_tool(never))
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
        says("where I stand: halfway"),
    ]);

    let (agent, _d) = f.agent("sol").await;
    agent
        .followup(ordinary("a push"))
        .await
        .expect("mail lands");
    f.wait_for_kind("tool/call").await;
    agent.followup(andrey("stop")).await.expect("mail lands");

    let prompt = f.wait_for_kind("wake/grace-prompt").await;
    assert!(
        prompt.body["text"]
            .as_str()
            .unwrap_or_default()
            .contains("interrupted"),
        "the instruction the model was given is DURABLE: {}",
        prompt.body
    );
    let jot = f.wait_for_kind("wake/jot").await;
    assert_eq!(
        jot.body["synthetic"], false,
        "the MODEL wrote this jot, which is only possible if the grace round resolved a model: {}",
        jot.body
    );

    // Every request the adapter was handed carries the decided model — the grace one included.
    let requests = f.adapter.requests();
    assert!(
        requests.iter().all(|r| r.call.model == "decided-by-policy"),
        "the grace step went through `agent/request` too: {:?}",
        requests
            .iter()
            .map(|r| r.call.model.clone())
            .collect::<Vec<_>>()
    );
    let grace = requests
        .iter()
        .find(|r| r.call.tool_choice_none)
        .expect("the grace round forbids tools");
    assert!(
        format!("{:?}", grace.messages).contains("interrupted"),
        "and it is the round that carries the instruction"
    );

    let steps = f.steps().await;
    let sent = recorded_for(&steps);
    invariant::evaluate_reconstruction(&sent, &steps)
        .expect("the grace request rebuilds from the ledger like any other");
}
