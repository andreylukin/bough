//! V6, for an ORDINARY lane rather than for the leader: a lane's scoped persona shadows the
//! global one for that lane alone, `tools.restrict` narrows the lane's world and narrows it in
//! both places the model can feel it (the schema and the executor), and a worker spawned by that
//! lane inherits none of it.
//!
//! Everything here is offline: an in-memory ledger, a real assembler, a real tool registry.

use std::sync::Arc;

use bough_kernel::{Context, EntryId, KernelCore, Plugin};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, Agents, AgentsHandle, Attach,
    CancelCause, CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_projection::{
    AssembleRequest, Assembled, Projection, ProjectionHandle, Projector,
};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCallId, ToolCx, ToolFailure, ToolName,
    ToolOutcome, ToolScope, ToolSpec, Tools, ToolsHandle,
};

use bough_plugin_lane_scope::{
    restrict_of, LaneScopeConfig, LaneScopePlugin, LaneSpec, PERSONA_TITLE,
};

// ---- the fixture -----------------------------------------------------------------------------

struct Fixture {
    root: Context,
    row: Context,
    tools: ToolsHandle,
    projection: ProjectionHandle,
    agents: AgentsHandle,
    _dir: tempfile::TempDir,
    // Held so the provisions stay in the store for the life of the fixture.
    _slots: Vec<Box<dyn std::any::Any>>,
}

impl Fixture {
    async fn open() -> Fixture {
        let root = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(root.clone()) as Arc<_>);
        let dir = tempfile::tempdir().expect("a temp dir");
        let assembler = Assembler::new(
            Arc::new(AssemblerConfig {
                budget_tokens: 100_000,
                headroom: 0.6,
                tail_steps: 20,
                tail_floor_steps: 4,
                dialogue_steps: 0,
                mail_newest_n: 3,
                max_tiers: 3,
                file_view_dir: dir.path().to_path_buf(),
            }),
            ledger.clone(),
            root.clone(),
        );
        let projection = ProjectionHandle(assembler as Arc<dyn Projector>);
        let tools = ToolsHandle::with_limits(4, 5_000);
        let agents = AgentsHandle::new(root.clone(), ledger.clone());
        agents
            .set_factory(&root, Arc::new(StubFactory) as Arc<dyn AgentFactory>)
            .await
            .expect("the slot is free");

        let slots: Vec<Box<dyn std::any::Any>> = vec![
            Box::new(
                root.provide::<Projection>(projection.clone())
                    .await
                    .expect("projection provides"),
            ),
            Box::new(
                root.provide::<Tools>(tools.clone())
                    .await
                    .expect("tools provides"),
            ),
            Box::new(
                root.provide::<Agents>(agents.clone())
                    .await
                    .expect("agents provides"),
            ),
        ];

        // The ROW's context: its own fiber and the row's effective inject set, which is what
        // `apply` reads its dependencies out of.
        let row = root.for_row(
            root.core().new_fiber_uid(),
            EntryId::new("lane.scope"),
            LaneScopePlugin::NAME,
            LaneScopePlugin::inject(),
        );
        Fixture {
            root,
            row,
            tools,
            projection,
            agents,
            _dir: dir,
            _slots: slots,
        }
    }

    async fn create(&self, name: &str) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                TrajId::new(format!("t-{name}")),
                chrono::Utc::now(),
            ))
            .await
            .expect("the agent is created");
        // The disposer is what tears the agent down; the fixture keeps every agent alive.
        std::mem::forget(disposer);
        agent
    }

    async fn assemble(&self, agent: &str) -> Assembled {
        self.projection
            .0
            .assemble(&AssembleRequest {
                agent: AgentName::new(agent),
                wake: None,
                at: chrono::Utc::now(),
                budget: None,
                as_of: None,
            })
            .await
            .expect("an assembly with no rows still succeeds")
    }

    /// The persona band's body for `agent`, or `None` when no persona section survived.
    async fn persona(&self, agent: &str) -> Option<String> {
        self.assemble(agent)
            .await
            .sections
            .into_iter()
            .find(|s| s.title == PERSONA_TITLE)
            .map(|s| s.body)
    }
}

/// A factory that attaches a driver doing nothing: `create` needs one, and no test here wakes.
struct StubFactory;

#[async_trait::async_trait]
impl AgentFactory for StubFactory {
    fn driver(&self) -> &'static str {
        "stub"
    }
    async fn attach(
        &self,
        _cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        Ok(Arc::new(StubDriver) as Arc<dyn AgentDriver>)
    }
}

struct StubDriver;

#[async_trait::async_trait]
impl AgentDriver for StubDriver {
    fn driver(&self) -> &'static str {
        "stub"
    }
    async fn notify(&self, _receipt: &InboxReceipt, _msg: &Message) {}
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
}

/// A tool that succeeds, so a refusal in a test can only come from the registry.
struct Echo;

#[async_trait::async_trait]
impl Tool for Echo {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        Ok(ToolOutcome {
            content: "ok".to_string(),
            ..ToolOutcome::default()
        })
    }
}

fn echo(name: &str) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        description: format!("the {name} tool"),
        input_schema: schemars::schema_for!(serde_json::Value),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool: Arc::new(Echo),
    }
}

fn call(name: &str, agent: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId::new("c1"),
        name: ToolName::new(name),
        args: serde_json::json!({}),
        agent: AgentName::new(agent),
        wake: WakeId::new("w1"),
        step_index: 0,
    }
}

fn lane(agent: &str, persona: Option<&str>, allow: Option<&[&str]>, deny: &[&str]) -> LaneSpec {
    LaneSpec {
        agent: agent.to_string(),
        persona: persona.map(|p| p.to_string()),
        allow: allow.map(|a| a.iter().map(|s| s.to_string()).collect()),
        deny: deny.iter().map(|s| s.to_string()).collect(),
    }
}

const GLOBAL: &str = "You are one lane of a working tree.";
const TERRA: &str = "You are terra. You hold the ground.";

// ---- the cases -------------------------------------------------------------------------------

#[tokio::test]
async fn a_scoped_persona_replaces_the_global_section_for_that_agent() {
    let f = Fixture::open().await;
    f.create("terra").await;
    LaneScopePlugin::apply(
        f.row.clone(),
        Arc::new(LaneScopeConfig {
            default_persona: Some(GLOBAL.to_string()),
            lanes: vec![lane("terra", Some(TERRA), None, &[])],
        }),
    )
    .await
    .expect("the row applies");

    let persona = f.persona("terra").await.expect("terra has a persona band");
    assert_eq!(
        persona, TERRA,
        "the lane's section shadows the global one under the same SectionId"
    );
    // One id, one band: shadowing REPLACES, it never appends a second copy.
    let personas = f
        .assemble("terra")
        .await
        .sections
        .into_iter()
        .filter(|s| s.title == PERSONA_TITLE)
        .count();
    assert_eq!(personas, 1, "most-specific-wins yields exactly one band");
}

#[tokio::test]
async fn another_agent_still_sees_the_global_section() {
    let f = Fixture::open().await;
    f.create("terra").await;
    f.create("sol").await;
    LaneScopePlugin::apply(
        f.row.clone(),
        Arc::new(LaneScopeConfig {
            default_persona: Some(GLOBAL.to_string()),
            lanes: vec![lane("terra", Some(TERRA), None, &[])],
        }),
    )
    .await
    .expect("the row applies");

    assert_eq!(f.persona("sol").await.as_deref(), Some(GLOBAL));
    assert_eq!(f.persona("terra").await.as_deref(), Some(TERRA));
}

#[tokio::test]
async fn restrict_is_an_intersection_of_two_restrictions() {
    let f = Fixture::open().await;
    f.create("terra").await;
    for name in ["bash", "read_file", "grep"] {
        f.tools
            .register(&f.root, echo(name))
            .await
            .expect("the tool registers");
    }

    // The row's own restriction: an allow list of two.
    LaneScopePlugin::apply(
        f.row.clone(),
        Arc::new(LaneScopeConfig {
            default_persona: None,
            lanes: vec![lane("terra", None, Some(&["bash", "read_file"]), &[])],
        }),
    )
    .await
    .expect("the row applies");
    assert_eq!(
        f.tools.visible(&AgentName::new("terra")),
        vec![ToolName::new("bash"), ToolName::new("read_file")]
    );

    // A SECOND restriction, from anywhere: composition may only narrow (§5).
    f.tools
        .restrict(
            &f.root,
            &AgentName::new("terra"),
            restrict_of(&lane("terra", None, Some(&["read_file", "grep"]), &[])),
        )
        .await
        .expect("a second restriction registers");
    assert_eq!(
        f.tools.visible(&AgentName::new("terra")),
        vec![ToolName::new("read_file")],
        "the intersection of {{bash, read_file}} and {{read_file, grep}}"
    );
}

#[tokio::test]
async fn a_filtered_tool_is_absent_from_the_schema() {
    let f = Fixture::open().await;
    f.create("terra").await;
    for name in ["bash", "read_file"] {
        f.tools
            .register(&f.root, echo(name))
            .await
            .expect("the tool registers");
    }
    LaneScopePlugin::apply(
        f.row.clone(),
        Arc::new(LaneScopeConfig {
            default_persona: None,
            lanes: vec![lane("terra", None, None, &["bash"])],
        }),
    )
    .await
    .expect("the row applies");

    let names: Vec<String> = f
        .tools
        .schemas(&AgentName::new("terra"))
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, vec!["read_file".to_string()]);
    // The filter is per-agent: an unrestricted agent still sees both.
    assert_eq!(f.tools.schemas(&AgentName::new("sol")).len(), 2);
}

#[tokio::test]
async fn a_filtered_tool_is_refused_by_the_executor() {
    let f = Fixture::open().await;
    f.create("terra").await;
    f.tools
        .register(&f.root, echo("bash"))
        .await
        .expect("the tool registers");
    LaneScopePlugin::apply(
        f.row.clone(),
        Arc::new(LaneScopeConfig {
            default_persona: None,
            lanes: vec![lane("terra", None, None, &["bash"])],
        }),
    )
    .await
    .expect("the row applies");

    let results = f.tools.execute(&f.root, vec![call("bash", "terra")]).await;
    let failure = results[0]
        .failure
        .as_ref()
        .expect("a filtered tool cannot succeed");
    assert_eq!(
        failure.kind,
        FailureClass::NotFound,
        "a filtered tool is indistinguishable from one that never existed (§9)"
    );
    // The set the model is SHOWN and the set it can CALL are the same set: an unrestricted agent
    // calls the same tool and succeeds.
    let ok = f.tools.execute(&f.root, vec![call("bash", "sol")]).await;
    assert!(ok[0].ok, "sol is not restricted");
}

#[tokio::test]
async fn a_workers_scope_inherits_nothing_from_its_spawner() {
    let f = Fixture::open().await;
    f.create("terra").await;
    f.tools
        .register(&f.root, echo("bash"))
        .await
        .expect("the tool registers");
    LaneScopePlugin::apply(
        f.row.clone(),
        Arc::new(LaneScopeConfig {
            default_persona: Some(GLOBAL.to_string()),
            lanes: vec![lane("terra", Some(TERRA), None, &["bash"])],
        }),
    )
    .await
    .expect("the row applies");

    // A worker of terra's is its own agent with its own name, and every registration here is
    // scoped BY NAME — so nothing of the lane's world reaches it.
    // The name a REAL worker of terra's gets, from the workers registry itself: if that naming
    // ever collapsed a worker onto its spawner, this test would go red rather than pass by luck.
    let worker = bough_plugin_workers::WorkersHandle::worker_agent_name(
        &AgentName::new("terra"),
        &bough_plugin_workers::WorkerId::new("1"),
    )
    .to_string();
    let worker = worker.as_str();
    assert_eq!(worker, "terra/worker-1");
    f.create(worker).await;
    assert_eq!(
        f.persona(worker).await.as_deref(),
        Some(GLOBAL),
        "the worker gets the global persona, not its spawner's"
    );
    assert_eq!(
        f.tools.visible(&AgentName::new(worker)),
        vec![ToolName::new("bash")],
        "the spawner's deny list does not follow the worker"
    );
}

#[tokio::test]
async fn a_lane_named_by_config_that_does_not_exist_yet_is_a_warning_then_a_retry() {
    let f = Fixture::open().await;
    // `luna` is tomorrow's lane: named by config, with no live agent.
    let cfg = Arc::new(LaneScopeConfig {
        default_persona: Some(GLOBAL.to_string()),
        lanes: vec![lane("luna", Some("You are luna."), None, &["bash"])],
    });
    f.tools
        .register(&f.root, echo("bash"))
        .await
        .expect("the tool registers");

    // The pure half of the rule: a missing lane is PENDING, never a boot failure.
    let mounted = bough_plugin_lane_scope::mount(
        &f.row,
        &f.projection,
        &f.tools,
        &f.agents,
        &LaneScopeConfig {
            default_persona: None,
            lanes: cfg.lanes.clone(),
        },
    )
    .await
    .expect("a config naming an unborn lane still applies");
    assert_eq!(mounted.mounted, Vec::<AgentName>::new());
    assert_eq!(mounted.pending, vec![AgentName::new("luna")]);

    // The whole rule, through the row: apply, then birth the lane, then watch the retry land.
    let f = Fixture::open().await;
    f.tools
        .register(&f.root, echo("bash"))
        .await
        .expect("the tool registers");
    LaneScopePlugin::apply(f.row.clone(), cfg)
        .await
        .expect("the row applies with the lane still unborn");
    assert_eq!(
        f.persona("luna").await.as_deref(),
        Some(GLOBAL),
        "before the retry, luna has only the global persona"
    );

    f.create("luna").await;
    // `agent/created` is an EMIT: dispatch is spawned, so the retry lands soon rather than
    // synchronously. Poll rather than sleep, so a fast machine finishes fast.
    let mut persona = None;
    for _ in 0..200 {
        persona = f.persona("luna").await;
        if persona.as_deref() == Some("You are luna.") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        persona.as_deref(),
        Some("You are luna."),
        "the retry mounts the lane's section on agent/created"
    );
    assert!(
        f.tools.visible(&AgentName::new("luna")).is_empty(),
        "and its restriction with it"
    );
}
