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
