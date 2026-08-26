//! §7, Phase 2: the four primitives exist as tools BEFORE the capability does. With no Provider
//! mounted every one of them refuses, and the refusal names the kind — that refusal is what the
//! model meets, and it must read as "the harness cannot do this", never as a broken tool.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_actions::{ActionKind, ActionsHandle};
use bough_plugin_ledger::{AgentName, LedgerHandle, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tool_actions::{spec, tool_name};
use bough_plugin_tools::{FailureClass, ToolCall, ToolCallId, ToolName, ToolsHandle};

/// A registry with the four primitives on it, over an actions seam that has NO Provider — Phase
/// 2's real composition.
async fn fixture() -> (Context, ToolsHandle, AgentName) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let actions = Arc::new(ActionsHandle::new(ledger));
    let tools = ToolsHandle::new();
    for kind in ActionKind::all() {
        tools
            .register(&ctx, spec(*kind, actions.clone()))
            .await
            .expect("the tool registers");
    }
    (ctx, tools, AgentName::new("sol"))
}

fn call(name: &str, target: &str, i: u32) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(format!("c{i}")),
        name: ToolName::new(name),
        args: serde_json::json!({ "target": target, "payload": {} }),
        agent: AgentName::new("sol"),
        wake: WakeId::new("w1"),
        step_index: i,
    }
}

/// A valid target per kind, so the refusal is about the missing Provider and nothing else.
fn target(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::OpenPr => "owner/repo",
        ActionKind::PushToPr | ActionKind::BotThreadOp => "owner/repo#12",
        ActionKind::LinearWrite => "TEAM-123",
    }
}

#[tokio::test]
async fn each_primitive_refuses_with_no_provider_mounted() {
    let (ctx, tools, _agent) = fixture().await;

    for (i, kind) in ActionKind::all().iter().enumerate() {
        let name = tool_name(*kind);
        let results = tools
            .execute(&ctx, vec![call(name, target(*kind), i as u32)])
            .await;
        assert_eq!(results.len(), 1, "one call, one result");
        let r = &results[0];
        assert!(!r.ok, "`{name}` must not report success with no provider");
        let failure = r.failure.as_ref().expect("a failed result carries why");
        assert_eq!(failure.kind, FailureClass::Denied);
        assert!(
            failure.message.contains(name),
            "the refusal must name the kind; got `{}`",
            failure.message
        );
        assert!(
            failure.message.contains("no provider"),
            "the refusal must say the harness lacks the capability; got `{}`",
            failure.message
        );
        // A refusal is VALUELESS: nothing happened, so there is nothing to hand back.
        assert!(r.value.is_none());
    }
}

#[tokio::test]
async fn a_fifth_spelling_is_not_a_tool_at_all() {
    let (ctx, tools, agent) = fixture().await;

    // Exactly four, and they are §7's four.
    let visible: Vec<String> = tools
        .visible(&agent)
        .iter()
        .map(|n| n.as_str().to_string())
        .collect();
    assert_eq!(
        visible,
        vec![
            "bot_thread_op".to_string(),
            "linear_write".into(),
            "open_pr".into(),
            "push_to_pr".into()
        ]
    );

    // `slack_send` is not a kind that could be spelled, so it is not a tool that can be called.
    let results = tools
        .execute(&ctx, vec![call("slack_send", "#general", 0)])
        .await;
    let failure = results[0].failure.as_ref().expect("an unknown tool fails");
    assert_eq!(failure.kind, FailureClass::NotFound);
}
