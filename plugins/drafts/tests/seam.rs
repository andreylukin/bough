//! §7: `draft()` appends a step and NOTHING ELSE HAPPENS; `list` reads back what it wrote,
//! filtered by agent and kind; and both tools refuse a draft with no audience — "where would this
//! go" is the question a draft exists to answer.

use std::collections::BTreeSet;

use bough_kernel::{Context, KernelCore};
use bough_plugin_drafts::tool::{DraftTool, DRAFT_MESSAGE_TOOL, DRAFT_TICKET_TOOL};
use bough_plugin_drafts::{
    DraftKind, DraftQuery, DraftsHandle, NewDraft, DRAFT_MESSAGE, DRAFT_TICKET,
};
use bough_plugin_ledger::{
    AgentName, AgentRow, LedgerHandle, Order, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tools::{FailureClass, ToolCall, ToolCallId, ToolName, ToolsHandle};
use chrono::{TimeZone, Utc};

fn at() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 2, 3, 4, 5, 6).unwrap()
}

async fn fixture() -> (Context, LedgerHandle, DraftsHandle) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
    for def in bough_plugin_drafts::step_types() {
        drop(ledger.0.register_step_type(def).expect("a fresh step type"));
    }
    for (name, traj) in [("sol", "t-sol"), ("terra", "t-terra")] {
        ledger
            .0
            .put_agent(AgentRow {
                name: AgentName::new(name),
                traj: TrajId::new(traj),
                routing_refs: BTreeSet::new(),
                wake_classes: BTreeSet::new(),
                model_override: None,
                tick_floor: None,
                digest_rollup: None,
            })
            .await
            .expect("agents is mutable config");
    }
    let drafts = DraftsHandle::new(ledger.clone(), 50);
    (ctx, ledger, drafts)
}

fn new_draft(agent: &str, kind: DraftKind, subject: &str) -> NewDraft {
    NewDraft {
        kind,
        agent: AgentName::new(agent),
        wake: WakeId::new("w1"),
        audience: "slack:#eng".into(),
        subject: subject.into(),
        body: "the body".into(),
        refs: BTreeSet::new(),
        at: at(),
    }
}

/// The step type, the class and the id: a draft is a `draft/*` THOUGHT carrying the id it returns.
#[tokio::test]
async fn draft_appends_the_right_step_type_and_class_and_returns_its_id() {
    let (_ctx, ledger, drafts) = fixture().await;
    for (kind, expect) in [
        (DraftKind::Message, DRAFT_MESSAGE),
        (DraftKind::Ticket, DRAFT_TICKET),
    ] {
        let row = drafts
            .draft(new_draft("sol", kind, "ship it"))
            .await
            .expect("the draft lands");
        let step = ledger
            .0
            .step(&row.step)
            .await
            .expect("the query runs")
            .expect("the step is there");
        assert_eq!(step.kind.as_str(), expect);
        assert_eq!(step.class, bough_plugin_ledger::Class::Thought);
        assert_eq!(
            step.body.get("draft").and_then(|v| v.as_str()),
            Some(row.id.as_str()),
            "the body carries the id `draft` returned"
        );
    }
}

/// NOTHING ELSE HAPPENS: the only rows in the trajectory are the drafts themselves — no
/// `action/intent`, no anything.
#[tokio::test]
async fn a_draft_writes_no_other_row() {
    let (_ctx, ledger, drafts) = fixture().await;
    drafts
        .draft(new_draft("sol", DraftKind::Message, "ship it"))
        .await
        .expect("the draft lands");
    let all = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![TrajId::new("t-sol")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the query runs");
    let kinds: Vec<&str> = all.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, vec![DRAFT_MESSAGE]);
}

#[tokio::test]
async fn list_filters_by_agent_and_by_kind() {
    let (_ctx, _l, drafts) = fixture().await;
    drafts
        .draft(new_draft("sol", DraftKind::Message, "sol msg"))
        .await
        .unwrap();
    drafts
        .draft(new_draft("sol", DraftKind::Ticket, "sol ticket"))
        .await
        .unwrap();
    drafts
        .draft(new_draft("terra", DraftKind::Message, "terra msg"))
        .await
        .unwrap();

    let all = drafts.list(&DraftQuery::default()).await.unwrap();
    assert_eq!(all.len(), 3);

    let sol = drafts
        .list(&DraftQuery {
            agents: vec![AgentName::new("sol")],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(sol.len(), 2);
    assert!(sol.iter().all(|d| d.agent.as_str() == "sol"));

    let tickets = drafts
        .list(&DraftQuery {
            kind: Some(DraftKind::Ticket),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0].subject, "sol ticket");

    let both = drafts
        .list(&DraftQuery {
            agents: vec![AgentName::new("sol")],
            kind: Some(DraftKind::Message),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].subject, "sol msg");
}

/// An agent with no `agents` row has no chain to file a draft on, and the refusal names it
/// instead of inventing a trajectory.
#[tokio::test]
async fn a_draft_for_an_unknown_agent_is_refused() {
    let (_ctx, _l, drafts) = fixture().await;
    let err = drafts
        .draft(new_draft("nobody", DraftKind::Message, "x"))
        .await
        .expect_err("there is no such agent");
    assert!(matches!(
        err,
        bough_plugin_drafts::DraftError::UnknownAgent(_)
    ));
}

async fn call_tool(
    ctx: &Context,
    tools: &ToolsHandle,
    name: &str,
    args: serde_json::Value,
) -> bough_plugin_tools::ToolResult {
    tools
        .execute(
            ctx,
            vec![ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new(name),
                args,
                agent: AgentName::new("sol"),
                wake: WakeId::new("w1"),
                step_index: 0,
            }],
        )
        .await
        .pop()
        .expect("one call, one result")
}

/// Both tools refuse an EMPTY AUDIENCE: a draft nobody can place is not a draft.
#[tokio::test]
async fn both_tools_refuse_an_empty_audience() {
    let (ctx, _l, drafts) = fixture().await;
    let tools = ToolsHandle::with_limits(4, 5_000);
    for kind in [DraftKind::Message, DraftKind::Ticket] {
        tools
            .register(&ctx, DraftTool::spec(kind, drafts.clone()))
            .await
            .expect("it registers");
    }
    for (name, args) in [
        (
            DRAFT_MESSAGE_TOOL,
            serde_json::json!({ "audience": "   ", "subject": "s", "body": "b" }),
        ),
        (
            DRAFT_TICKET_TOOL,
            serde_json::json!({ "audience": "", "title": "t", "body": "b" }),
        ),
    ] {
        let r = call_tool(&ctx, &tools, name, args).await;
        assert!(!r.ok, "`{name}` must refuse an empty audience");
        assert_eq!(
            r.failure.as_ref().map(|f| f.kind),
            Some(FailureClass::Denied),
            "`{name}`: the model is being told what a draft needs, not that the harness broke"
        );
    }
    assert!(
        drafts
            .list(&DraftQuery::default())
            .await
            .unwrap()
            .is_empty(),
        "a refused draft writes no row"
    );
}

/// The happy path through the TOOL: one draft step, and the model is told it was not sent.
#[tokio::test]
async fn the_message_tool_writes_a_draft_and_says_it_did_not_send() {
    let (ctx, ledger, drafts) = fixture().await;
    let tools = ToolsHandle::with_limits(4, 5_000);
    tools
        .register(&ctx, DraftTool::spec(DraftKind::Message, drafts.clone()))
        .await
        .unwrap();
    let r = call_tool(
        &ctx,
        &tools,
        DRAFT_MESSAGE_TOOL,
        serde_json::json!({ "audience": "slack:#eng", "subject": "s", "body": "b" }),
    )
    .await;
    assert!(r.ok, "{:?}", r.failure);
    assert!(r.content.contains("NOT sent"), "{}", r.content);
    let rows = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new(DRAFT_MESSAGE)],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

/// The two tools are the only thing this crate registers, and neither name spells a send.
#[tokio::test]
async fn the_crate_registers_exactly_two_tools_and_neither_sends() {
    let (ctx, _l, drafts) = fixture().await;
    let tools = ToolsHandle::with_limits(4, 5_000);
    for kind in [DraftKind::Message, DraftKind::Ticket] {
        tools
            .register(&ctx, DraftTool::spec(kind, drafts.clone()))
            .await
            .unwrap();
    }
    let names: Vec<String> = [
        "slack_send",
        "send_message",
        "post_message",
        "create_ticket",
    ]
    .iter()
    .map(|n| n.to_string())
    .collect();
    for n in names {
        let r = call_tool(&ctx, &tools, &n, serde_json::json!({})).await;
        assert_eq!(
            r.failure.as_ref().map(|f| f.kind),
            Some(FailureClass::NotFound),
            "`{n}` must not exist"
        );
    }
}
