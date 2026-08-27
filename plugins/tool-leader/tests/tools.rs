//! The leader's five tools are scoped to ONE agent, and that agent comes from `ctx.leader`.
//! Everything here is a statement about the pair (registry, binding): the target sees them, no
//! other agent does — not in the schema it is shown and not at the executor it calls — and moving
//! the binding moves all five, which is the SWAP in miniature.

use std::sync::Arc;

use bough_kernel::{Context, EntryId, KernelCore, Plugin};
use bough_plugin_agents::{
    AgentCell, AgentDriver, AgentError, AgentFactory, Agents, AgentsHandle, Attach, CancelCause,
    CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_claims::{Claims, ClaimsConfig, ClaimsHandle};
use bough_plugin_graph_ops::{
    Graph, GraphError, GraphHandle, GraphOps, OpOutcome, OpPlan, OpRequest, UndoRequest,
};
use bough_plugin_leader::{Leader, LeaderConfig, LeaderHandle, LeaderPlugin};
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_mail_router::{Mail, MailConfig, MailHandle};
use bough_plugin_projection::{Projection, ProjectionHandle, Projector};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use bough_plugin_tool_leader::{ToolLeaderConfig, ToolLeaderPlugin, TOOL_NAMES};
use bough_plugin_tools::{FailureClass, ToolCall, ToolCallId, ToolName, Tools, ToolsHandle};

struct Fixture {
    root: Context,
    /// The `leader` row's context, and the `tool.leader` row's: two rows, two fibers.
    leader_row: Context,
    tool_row: Context,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    claims: ClaimsHandle,
    tools: ToolsHandle,
    _dir: tempfile::TempDir,
    _slots: Vec<Box<dyn std::any::Any>>,
}

impl Fixture {
    async fn open() -> Fixture {
        let root = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(root.clone()) as Arc<_>);
        let dir = tempfile::tempdir().expect("a temp dir");

        let agents = AgentsHandle::new(root.clone(), ledger.clone());
        agents
            .set_factory(&root, Arc::new(IdleFactory) as Arc<dyn AgentFactory>)
            .await
            .expect("the slot is free");
        ledger
            .declare_step_types(&root, bough_plugin_mail_router::vocabulary::step_types())
            .await
            .expect("the mail step types declare");
        let mail = MailHandle::new(
            root.clone(),
            ledger.clone(),
            agents.clone(),
            Arc::new(MailConfig {
                unsorted_traj: "t-unsorted".to_string(),
                unsorted_limit: 50,
                deliver_to_dormant: true,
            }),
        );
        let graph = GraphHandle(Arc::new(RefusingGraph) as Arc<dyn GraphOps>);
        let claims = ClaimsHandle::new(
            root.clone(),
            ledger.clone(),
            agents.clone(),
            graph.clone(),
            Arc::new(ClaimsConfig { open_limit: 50 }),
        );
        let assembler = Assembler::new(
            Arc::new(AssemblerConfig {
                budget_tokens: 100_000,
                headroom: 0.6,
                tail_steps: 20,
                tail_floor_steps: 4,
                mail_newest_n: 3,
                max_tiers: 3,
                file_view_dir: dir.path().to_path_buf(),
            }),
            ledger.clone(),
            root.clone(),
        );
        let projection = ProjectionHandle(assembler as Arc<dyn Projector>);
        let tools = ToolsHandle::with_limits(4, 5_000);

        let slots: Vec<Box<dyn std::any::Any>> = vec![
            Box::new(
                root.provide::<Ledger>(ledger.clone())
                    .await
                    .expect("ledger"),
            ),
            Box::new(
                root.provide::<Agents>(agents.clone())
                    .await
                    .expect("agents"),
            ),
            Box::new(root.provide::<Mail>(mail).await.expect("mail")),
            Box::new(
                root.provide::<Claims>(claims.clone())
                    .await
                    .expect("claims"),
            ),
            Box::new(root.provide::<Graph>(graph).await.expect("graph")),
            Box::new(
                root.provide::<Projection>(projection)
                    .await
                    .expect("projection"),
            ),
            Box::new(root.provide::<Tools>(tools.clone()).await.expect("tools")),
        ];

        let leader_row = row(&root, "leader", LeaderPlugin::NAME, LeaderPlugin::inject());
        let tool_row = row(
            &root,
            "tool.leader",
            ToolLeaderPlugin::NAME,
            ToolLeaderPlugin::inject(),
        );
        Fixture {
            root,
            leader_row,
            tool_row,
            ledger,
            agents,
            claims,
            tools,
            _dir: dir,
            _slots: slots,
        }
    }

    /// Apply both rows of the set for `target`, in the order the bundle group mounts them.
    async fn mount_set(
        &self,
        leader_row: &Context,
        tool_row: &Context,
        target: &str,
    ) -> LeaderHandle {
        LeaderPlugin::apply(
            leader_row.clone(),
            Arc::new(LeaderConfig {
                agent: target.to_string(),
                persona: format!("You are {target}, and you lead."),
                adopt_batch: 8,
                attribute_reconsolidation: true,
            }),
        )
        .await
        .expect("the leader row applies");
        ToolLeaderPlugin::apply(tool_row.clone(), Arc::new(ToolLeaderConfig {}))
            .await
            .expect("the tool row applies");
        (*leader_row.get::<Leader>().expect("ctx.leader is bound")).clone()
    }

    async fn lane(&self, name: &str) {
        let (_, disposer) = self
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                TrajId::new(format!("t-{name}")),
                chrono::Utc::now(),
            ))
            .await
            .expect("the agent is created");
        std::mem::forget(disposer);
    }

    fn visible(&self, agent: &str) -> Vec<String> {
        self.tools
            .visible(&AgentName::new(agent))
            .into_iter()
            .map(|n| n.as_str().to_string())
            .collect()
    }
}

fn row(root: &Context, entry: &str, plugin: &'static str, inject: bough_kernel::Inject) -> Context {
    root.for_row(
        root.core().new_fiber_uid(),
        EntryId::new(entry),
        plugin,
        inject,
    )
}

fn call(name: &str, agent: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: ToolCallId::new("c1"),
        name: ToolName::new(name),
        args,
        agent: AgentName::new(agent),
        wake: WakeId::new("w1"),
        step_index: 0,
    }
}

fn lane_claim() -> serde_json::Value {
    serde_json::json!({
        "kind": "lane",
        "title": "infra deserves a lane",
        "body": "three weeks of infra mail landed on terra",
        "detail": { "name": "infra", "routing_refs": ["repo:bough"] }
    })
}

// ---- the cases -------------------------------------------------------------------------------

#[tokio::test]
async fn the_five_tools_are_in_the_targets_schema() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

    let mut expected: Vec<String> = TOOL_NAMES.iter().map(|n| n.to_string()).collect();
    expected.sort();
    let names: Vec<String> = f
        .tools
        .schemas(&AgentName::new("sol"))
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, expected, "exactly the five, in the schema itself");
}

#[tokio::test]
async fn they_are_absent_from_every_other_agents_schema() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    f.lane("terra").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

    // The global `propose_claim` is the only claim tool an ordinary lane is shown, and it is
    // registered by `claims`, not here — so with it unmounted terra sees nothing at all.
    assert!(
        f.visible("terra").is_empty(),
        "a lane agent is shown none of the leader's tools; got {:?}",
        f.visible("terra")
    );
}

#[tokio::test]
async fn the_executor_refuses_them_for_another_agent() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    f.lane("terra").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

    for name in TOOL_NAMES {
        let results = f
            .tools
            .execute(&f.root, vec![call(name, "terra", serde_json::json!({}))])
            .await;
        let failure = results[0]
            .failure
            .as_ref()
            .unwrap_or_else(|| panic!("`{name}` must not run for terra"));
        assert_eq!(
            failure.kind,
            FailureClass::NotFound,
            "a tool outside the agent's scope is indistinguishable from one that never existed \
             (§9); `{name}` said {failure:?}"
        );
    }
}

#[tokio::test]
async fn the_scoped_propose_claim_accepts_a_structural_kind() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

    let results = f
        .tools
        .execute(&f.root, vec![call("propose_claim", "sol", lane_claim())])
        .await;
    assert!(
        results[0].ok,
        "the leader may propose structure: {:?}",
        results[0].failure
    );

    // It is a CLAIM and nothing else: an open one, of the structural kind, and no agents row was
    // born by proposing it.
    let open = f
        .claims
        .open(&bough_plugin_claims::ClaimQuery::default())
        .await
        .expect("the open list reads");
    assert_eq!(open.len(), 1);
    assert!(open[0].kind.is_structural());
    assert_eq!(open[0].kind.as_str(), "lane");
    assert!(
        f.ledger
            .0
            .agent(&AgentName::new("infra"))
            .await
            .expect("the read runs")
            .is_none(),
        "proposing a lane does not bear one: acceptance is Andrey's act"
    );
}

#[tokio::test]
async fn the_global_one_refuses_it_for_a_lane_agent() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    f.lane("terra").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;
    // The GLOBAL `propose_claim`, registered by `claims` exactly as its row does it.
    bough_plugin_claims::tool::register(&f.root, &f.claims)
        .await
        .expect("the global tool registers");

    // terra now sees one tool, and it is the global twin.
    assert_eq!(f.visible("terra"), vec!["propose_claim".to_string()]);
    let refused = f
        .tools
        .execute(&f.root, vec![call("propose_claim", "terra", lane_claim())])
        .await;
    let failure = refused[0]
        .failure
        .as_ref()
        .expect("a lane agent may not propose structure");
    assert_eq!(failure.kind, FailureClass::Denied);
    assert!(
        failure
            .message
            .contains("only the leader proposes structure"),
        "{}",
        failure.message
    );

    // The same call, from the leader, succeeds: the scoped tool SHADOWS the global one for sol
    // alone, and the difference between them is behaviour rather than presentation.
    let allowed = f
        .tools
        .execute(&f.root, vec![call("propose_claim", "sol", lane_claim())])
        .await;
    assert!(allowed[0].ok, "{:?}", allowed[0].failure);
}

#[tokio::test]
async fn the_row_reloads_when_the_leader_binding_changes() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    f.lane("terra").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    let leader = f.mount_set(&leader_row, &tool_row, "sol").await;
    assert_eq!(leader.target(), &AgentName::new("sol"));
    assert_eq!(f.visible("sol").len(), 5);

    // Editing `leader.config.agent` reloads the `leader` row; `tool-leader` injects `leader`, so
    // it unloads with the binding and reloads against the new one. Both fibers unwind, both rows
    // apply again — and nothing was recompiled.
    f.root.core().unwind_fiber(f.tool_row.fiber_uid()).await;
    f.root.core().unwind_fiber(f.leader_row.fiber_uid()).await;
    assert!(
        f.visible("sol").is_empty(),
        "unloading the row takes the tools out of the old target's scope"
    );

    let next_leader = row(
        &f.root,
        "leader",
        LeaderPlugin::NAME,
        LeaderPlugin::inject(),
    );
    let next_tool = row(
        &f.root,
        "tool.leader",
        ToolLeaderPlugin::NAME,
        ToolLeaderPlugin::inject(),
    );
    let moved = f.mount_set(&next_leader, &next_tool, "terra").await;

    assert_eq!(moved.target(), &AgentName::new("terra"));
    assert_eq!(
        f.visible("terra").len(),
        5,
        "all five moved together: the row read its target from the binding, never from config"
    );
    assert!(f.visible("sol").is_empty());
}

// ---- the stubs -------------------------------------------------------------------------------

struct RefusingGraph;

#[async_trait::async_trait]
impl GraphOps for RefusingGraph {
    fn provider(&self) -> &'static str {
        "refusing-graph"
    }
    async fn plan(&self, _req: &OpRequest) -> Result<OpPlan, GraphError> {
        Err(GraphError::Other(anyhow::anyhow!("no graph provider")))
    }
    async fn apply(&self, _req: &OpRequest) -> Result<OpOutcome, GraphError> {
        Err(GraphError::Other(anyhow::anyhow!("no graph provider")))
    }
    async fn undo(&self, _req: &UndoRequest) -> Result<OpOutcome, GraphError> {
        Err(GraphError::Other(anyhow::anyhow!("no graph provider")))
    }
}

struct IdleFactory;

#[async_trait::async_trait]
impl AgentFactory for IdleFactory {
    fn driver(&self) -> &'static str {
        "idle"
    }
    async fn attach(
        &self,
        _cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        Ok(Arc::new(Idle) as Arc<dyn AgentDriver>)
    }
}

struct Idle;

#[async_trait::async_trait]
impl AgentDriver for Idle {
    fn driver(&self) -> &'static str {
        "idle"
    }
    async fn notify(&self, _receipt: &InboxReceipt, _msg: &Message) {}
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
}
