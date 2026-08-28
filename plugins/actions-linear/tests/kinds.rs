//! §7 (V2) — ticket creation is not something this harness can be asked to do. `create_ticket` is
//! not a kind, so it is not a tool; and a `linear_write` whose payload NAMES a title is refused by
//! the Provider before any mutation runs. Both halves are exercised through the real seams: the
//! tools registry the model actually calls, and the Provider's real `execute`.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_actions::{ActionKind, ActionProvider, ActionsHandle};
use bough_plugin_actions_linear::{LinearActionError, LinearActions, LinearApi};
use bough_plugin_ledger::{AgentName, AgentRow, LedgerHandle, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tool_actions::spec;
use bough_plugin_tools::{FailureClass, ToolCall, ToolCallId, ToolName, ToolsHandle};
use parking_lot::Mutex;

/// A Linear that answers the issue read and records every mutation it is asked to run.
#[derive(Default)]
struct RecordingLinear {
    mutations: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl LinearApi for RecordingLinear {
    async fn graphql(
        &self,
        query: &str,
        _v: serde_json::Value,
    ) -> Result<serde_json::Value, LinearActionError> {
        if query.starts_with("mutation") {
            self.mutations.lock().push(query.to_string());
        }
        Ok(
            serde_json::json!({ "issue": { "id": "uuid-1", "identifier": "TEAM-1",
            "team": { "states": { "nodes": [{ "id": "s", "name": "Done" }] } } } }),
        )
    }
}

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: ToolCallId::new("c1"),
        name: ToolName::new(name),
        args,
        agent: AgentName::new("sol"),
        wake: WakeId::new("w1"),
        step_index: 0,
    }
}

#[tokio::test]
async fn create_ticket_is_refused_as_an_unknown_kind_and_a_linear_write_naming_a_title_is_bad_payload(
) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new("sol"),
            traj: TrajId::new("t1"),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the agent row goes in");
    let actions = Arc::new(ActionsHandle::new(ledger));
    let api = Arc::new(RecordingLinear::default());
    actions
        .provider(
            &ctx,
            LinearActions::with_api(api.clone()) as Arc<dyn ActionProvider>,
        )
        .await
        .expect("the linear provider mounts");

    let tools = ToolsHandle::with_limits(8, 5_000);
    for kind in ActionKind::all() {
        tools
            .register(&ctx, spec(*kind, actions.clone()))
            .await
            .expect("the tool registers");
    }

    // Half one: `create_ticket` is not a tool, because it is not a kind. Ditto `slack_send`.
    for absent in ["create_ticket", "slack_send"] {
        let results = tools
            .execute(
                &ctx,
                vec![call(
                    absent,
                    serde_json::json!({ "target": "TEAM-1", "payload": {} }),
                )],
            )
            .await;
        let failure = results[0]
            .failure
            .as_ref()
            .unwrap_or_else(|| panic!("`{absent}` must not succeed"));
        assert_eq!(
            failure.kind,
            FailureClass::NotFound,
            "`{absent}` must be refused as an unknown tool, got `{}`",
            failure.message
        );
    }

    // Half two: creation smuggled into the one kind that does exist is refused by the Provider,
    // and the refusal happens BEFORE any mutation reaches Linear.
    let results = tools
        .execute(
            &ctx,
            vec![call(
                "linear_write",
                serde_json::json!({ "target": "TEAM-1",
                    "payload": { "title": "a new ticket", "comment": "hi" } }),
            )],
        )
        .await;
    let failure = results[0]
        .failure
        .as_ref()
        .expect("a creation-shaped payload must not succeed");
    assert!(
        failure.message.contains("creating tickets"),
        "the refusal must say why; got `{}`",
        failure.message
    );
    assert!(
        api.mutations.lock().is_empty(),
        "nothing was written to Linear: {:?}",
        api.mutations.lock()
    );
}
