//! The leader's tools are scoped to ONE agent, and that agent comes from `ctx.leader`.
//! Everything here is a statement about the pair (registry, binding): the target sees them, no
//! other agent does — not in the schema it is shown and not at the executor it calls — and moving
//! the binding moves all of them, which is the SWAP in miniature.
//!
//! THE CLAIMS DEMOLITION: `propose_claim` is gone with the claims seam; `create_lane` and
//! `merge_lanes` apply through `ctx.graph` DIRECTLY, so the graph double here RECORDS what it is
//! asked and writes the row a real bud would write — what these tests pin is what the tools ask
//! of the seam, attributed to whom, and what they do with the answer.

use std::sync::Arc;

use bough_kernel::{Context, EntryId, KernelCore, Plugin};
use bough_plugin_agents::{
    AgentCell, AgentDriver, AgentError, AgentFactory, Agents, AgentsHandle, Attach, CancelCause,
    CreateAgent, InboxReceipt, MailClass, Message, Sender, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_graph_ops::{
    Graph, GraphError, GraphHandle, GraphOps, OpKind, OpOutcome, OpPlan, OpRequest, UndoRequest,
};
use bough_plugin_leader::{Leader, LeaderConfig, LeaderHandle, LeaderPlugin};
use bough_plugin_ledger::query::StepQuery;
use bough_plugin_ledger::{
    AgentName, AgentRow, Ledger, LedgerHandle, Ref, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_mail_router::{Envelope, Mail, MailConfig, MailHandle};
use bough_plugin_projection::{Projection, ProjectionHandle, Projector};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use bough_plugin_rollups::Attribution;
use bough_plugin_tool_leader::{ToolLeaderConfig, ToolLeaderPlugin, TOOL_NAMES};
use bough_plugin_tools::{FailureClass, ToolCall, ToolCallId, ToolName, Tools, ToolsHandle};
use parking_lot::Mutex;

struct Fixture {
    root: Context,
    /// The `leader` row's context, and the `tool.leader` row's: two rows, two fibers.
    leader_row: Context,
    tool_row: Context,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    mail: MailHandle,
    graph: Arc<RecordingGraph>,
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
                tolerate_absent_lane: true,
            }),
        );
        let recording = Arc::new(RecordingGraph {
            ledger: ledger.clone(),
            seen: Mutex::new(Vec::new()),
            undone: Mutex::new(Vec::new()),
        });
        let graph = GraphHandle(recording.clone() as Arc<dyn GraphOps>);
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
            Box::new(root.provide::<Mail>(mail.clone()).await.expect("mail")),
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
            mail,
            graph: recording,
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

    /// One envelope matching NOBODY, so it lands on the unsorted queue. Call it BEFORE mounting
    /// the set: the leader row installs a SINK that would take it to the leader's own lane.
    async fn unsorted(&self, tag: &str) -> bough_plugin_ledger::StepId {
        let mut refs = std::collections::BTreeSet::new();
        refs.insert(Ref::new(format!("nobody:{tag}")));
        self.mail
            .route(Envelope {
                from: Sender::System("tool-leader-test"),
                class: MailClass::Ordinary,
                subject: format!("unsorted {tag}"),
                summary: format!("unsorted {tag}"),
                text: "UNSORTED".to_string(),
                cites: Vec::new(),
                refs,
                // MERGE (track B): the at-least-once guard. This envelope is a one-off, so it
                // carries no dedupe ref.
                dedupe_on: None,
                at: chrono::Utc::now(),
            })
            .await
            .expect("the envelope routes")
            .unsorted
            .expect("a zero-match envelope lands on the queue")
    }

    /// How many `timeline/entry` steps the ledger holds.
    async fn timeline_entries(&self) -> usize {
        self.ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new(
                    bough_plugin_leader::vocabulary::TIMELINE_ENTRY,
                )],
                ..Default::default()
            })
            .await
            .expect("the query answers")
            .len()
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

/// A `curate` call that asks only for the timeline half.
fn timeline_only() -> serde_json::Value {
    serde_json::json!({
        "timeline": [{
            "title": "terra and sol both touched the router",
            "at": "2026-01-01T00:00:00Z",
            "agents": ["sol", "terra"],
            "cites": ["gh:bough/rebuild#1"]
        }]
    })
}

fn lane_args() -> serde_json::Value {
    serde_json::json!({
        "name": "infra",
        "reason": "three weeks of infra mail landed on terra",
        "routing_refs": ["repo:bough"]
    })
}

// ---- the cases -------------------------------------------------------------------------------

#[tokio::test]
async fn the_set_is_create_lane_merge_lanes_and_curate() {
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
    assert_eq!(names, expected, "exactly the three, in the schema itself");
    assert_eq!(TOOL_NAMES, ["create_lane", "merge_lanes", "curate"]);
}

#[tokio::test]
async fn they_are_absent_from_every_other_agents_schema() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    f.lane("terra").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

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
async fn create_lane_bears_a_live_lane_attributed_to_the_leader() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

    let results = f
        .tools
        .execute(&f.root, vec![call("create_lane", "sol", lane_args())])
        .await;
    assert!(results[0].ok, "{:?}", results[0].failure);

    // The seam was asked for a BUD, by the leader, cited.
    let seen = f.graph.seen.lock().clone();
    let bud = match seen.as_slice() {
        [OpRequest::Bud(b)] => b.clone(),
        other => panic!("one bud, got {other:?}"),
    };
    assert_eq!(bud.parent, AgentName::new("sol"));
    assert!(
        matches!(&bud.by, Attribution::Agent { name } if *name == AgentName::new("sol")),
        "the op is the leader's act, attributed as such: {:?}",
        bud.by
    );
    assert!(
        !bud.cites.is_empty(),
        "a direct op still cites — at least the call itself"
    );

    // …and the lane is REAL: a row with the asked-for routing.
    let row = f
        .ledger
        .0
        .agent(&AgentName::new("infra"))
        .await
        .expect("the read runs")
        .expect("the lane was born");
    assert!(row.routing_refs.contains(&Ref::new("repo:bough")));

    // A second creation of the same name is refused, not re-budded.
    let again = f
        .tools
        .execute(&f.root, vec![call("create_lane", "sol", lane_args())])
        .await;
    let failure = again[0].failure.as_ref().expect("a duplicate is refused");
    assert!(
        failure.message.contains("already exists"),
        "{}",
        failure.message
    );
}

#[tokio::test]
async fn merge_lanes_asks_the_seam_and_refuses_self_absorption() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    f.lane("terra").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

    let results = f
        .tools
        .execute(
            &f.root,
            vec![call(
                "merge_lanes",
                "sol",
                serde_json::json!({
                    "survivor": "sol", "absorbed": "terra", "reason": "terra went quiet"
                }),
            )],
        )
        .await;
    assert!(results[0].ok, "{:?}", results[0].failure);
    let seen = f.graph.seen.lock().clone();
    let merge = match seen.as_slice() {
        [OpRequest::Merge(m)] => m.clone(),
        other => panic!("one merge, got {other:?}"),
    };
    assert_eq!(merge.survivor, AgentName::new("sol"));
    assert_eq!(merge.absorbed, AgentName::new("terra"));
    assert!(
        matches!(&merge.by, Attribution::Agent { name } if *name == AgentName::new("sol")),
        "{:?}",
        merge.by
    );

    // The leader folding its own lane away would take the leader set down with it.
    let refused = f
        .tools
        .execute(
            &f.root,
            vec![call(
                "merge_lanes",
                "sol",
                serde_json::json!({
                    "survivor": "terra", "absorbed": "sol", "reason": "no"
                }),
            )],
        )
        .await;
    let failure = refused[0]
        .failure
        .as_ref()
        .expect("self-absorption is refused");
    assert!(
        failure.message.contains("your own lane"),
        "{}",
        failure.message
    );
    assert_eq!(
        f.graph.seen.lock().len(),
        1,
        "the refusal never reached the seam"
    );
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
    assert_eq!(f.visible("sol").len(), 3);

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
        3,
        "all moved together: the row read its target from the binding, never from config"
    );
    assert!(f.visible("sol").is_empty());
}

/// `curate` with only placements: exactly what `adopt_unsorted` wrote.
#[tokio::test]
async fn curate_with_only_placements_adopts() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    f.lane("terra").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    let step = f.unsorted("one").await;
    f.mount_set(&leader_row, &tool_row, "sol").await;

    let results = f
        .tools
        .execute(
            &f.root,
            vec![call(
                "curate",
                "sol",
                serde_json::json!({
                    "placements": [{ "step": step.as_str(), "agent": "terra" }]
                }),
            )],
        )
        .await;
    assert!(results[0].ok, "{:?}", results[0].failure);
    let value = results[0].value.clone().expect("a value");
    assert_eq!(
        value["adopted"],
        serde_json::json!([{ "step": step.as_str(), "agent": "terra" }])
    );
    assert!(
        value["timeline"].as_array().expect("an array").is_empty(),
        "no timeline half was asked for, so none was written"
    );
    assert_eq!(f.timeline_entries().await, 0);
}

/// `curate` with only a timeline: exactly what `note_timeline` wrote.
#[tokio::test]
async fn curate_with_only_a_timeline_notes() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

    let results = f
        .tools
        .execute(&f.root, vec![call("curate", "sol", timeline_only())])
        .await;
    assert!(results[0].ok, "{:?}", results[0].failure);
    assert_eq!(f.timeline_entries().await, 1);
    let value = results[0].value.clone().expect("a value");
    assert_eq!(value["timeline"].as_array().expect("an array").len(), 1);
    assert!(
        value["adopted"].as_array().expect("an array").is_empty(),
        "no adopt half was asked for"
    );
}

/// Both halves, ONE call, one journalled pass.
#[tokio::test]
async fn curate_absorbs_adopt_unsorted_and_note_timeline() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    f.lane("terra").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    let step = f.unsorted("two").await;
    f.mount_set(&leader_row, &tool_row, "sol").await;

    let mut args = timeline_only();
    args["placements"] = serde_json::json!([{ "step": step.as_str(), "agent": "terra" }]);
    let results = f
        .tools
        .execute(&f.root, vec![call("curate", "sol", args)])
        .await;
    assert!(results[0].ok, "{:?}", results[0].failure);
    let value = results[0].value.clone().expect("a value");
    assert_eq!(value["adopted"].as_array().expect("an array").len(), 1);
    assert_eq!(value["timeline"].as_array().expect("an array").len(), 1);
    assert_eq!(f.timeline_entries().await, 1);
}

#[tokio::test]
async fn an_empty_curate_is_refused_rather_than_a_silent_no_op() {
    let f = Fixture::open().await;
    f.lane("sol").await;
    let leader_row = f.leader_row.clone();
    let tool_row = f.tool_row.clone();
    f.mount_set(&leader_row, &tool_row, "sol").await;

    let results = f
        .tools
        .execute(&f.root, vec![call("curate", "sol", serde_json::json!({}))])
        .await;
    let failure = results[0]
        .failure
        .as_ref()
        .expect("a call that asks for nothing is refused");
    assert!(
        failure.message.contains("something to do"),
        "{}",
        failure.message
    );
    assert_eq!(f.timeline_entries().await, 0);
}

// ---- the stubs -------------------------------------------------------------------------------

/// A graph seam that RECORDS the request and writes the row a real bud would write (the old
/// `claims/tests/lane.rs` double, moved here with the behaviour it pinned).
struct RecordingGraph {
    ledger: LedgerHandle,
    seen: Mutex<Vec<OpRequest>>,
    undone: Mutex<Vec<bough_plugin_ledger::StepId>>,
}

#[async_trait::async_trait]
impl GraphOps for RecordingGraph {
    fn provider(&self) -> &'static str {
        "recording-graph"
    }
    async fn plan(&self, _req: &OpRequest) -> Result<OpPlan, GraphError> {
        unreachable!("the leader's tools apply, they do not plan")
    }
    async fn apply(&self, req: &OpRequest) -> Result<OpOutcome, GraphError> {
        self.seen.lock().push(req.clone());
        match req {
            OpRequest::Bud(bud) => {
                let child = &bud.child;
                let name = child.agent.clone().expect("a lane bud names its agent");
                self.ledger
                    .0
                    .put_agent(AgentRow {
                        name: name.clone(),
                        traj: child.traj.clone(),
                        routing_refs: child.routing_refs.clone(),
                        wake_classes: child.wake_classes.clone(),
                        model_override: None,
                        tick_floor: None,
                        digest_rollup: None,
                    })
                    .await?;
                Ok(OpOutcome {
                    kind: OpKind::Bud,
                    step: bough_plugin_ledger::StepId::new("graph-bud-step"),
                    trajs: vec![child.traj.clone()],
                    edges: 1,
                    digests: Vec::new(),
                    rows_written: vec![name],
                    rows_deleted: Vec::new(),
                    undo_shape: None,
                })
            }
            OpRequest::Merge(m) => Ok(OpOutcome {
                kind: OpKind::Merge,
                step: bough_plugin_ledger::StepId::new("graph-merge-step"),
                trajs: Vec::new(),
                edges: 1,
                digests: Vec::new(),
                rows_written: vec![m.survivor.clone()],
                rows_deleted: vec![m.absorbed.clone()],
                undo_shape: None,
            }),
            other => panic!("the leader's tools ask for buds and merges only, got {other:?}"),
        }
    }
    async fn undo(&self, req: &UndoRequest) -> Result<OpOutcome, GraphError> {
        self.undone.lock().push(req.of.clone());
        let mut deleted = Vec::new();
        for row in self.ledger.0.agents().await? {
            if row.name.as_str() == "infra" {
                self.ledger.0.delete_agent(&row.name).await?;
                deleted.push(row.name);
            }
        }
        Ok(OpOutcome {
            kind: OpKind::Undo,
            step: bough_plugin_ledger::StepId::new("graph-undo-step"),
            trajs: Vec::new(),
            edges: 0,
            digests: Vec::new(),
            rows_written: Vec::new(),
            rows_deleted: deleted,
            undo_shape: Some(bough_plugin_graph_ops::UndoShape::Pointers),
        })
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

// ---- V7: the collapse holds under the CODE-MODE consumer too ---------------------------------
//
// The four bullets above measure the typed consumer: `tools.schemas`/`visible` is what reaches
// the model there. Under `tools-codemode` the model is shown ONE tool and the leader's set
// arrives as pre-injected FUNCTIONS, so "the five old spellings are gone from both consumers" is
// a second, different question with a second, different answer path (`conceal::visible_specs` →
// `bind::bindings` → the sandbox globals). These cases ask it of REAL QuickJS running a REAL
// program against the REAL specs this crate registers — not of `TOOL_NAMES`.
mod codemode {
    use super::*;

    use bough_plugin_js::{Caps, JsHandle};
    use bough_plugin_js_quickjs::{QuickJsConfig, QuickJsEngine};
    use bough_plugin_tools::{Tool, ToolCx, ToolOutcome};
    use bough_plugin_tools_codemode::conceal::Concealment;
    use bough_plugin_tools_codemode::{CodemodeConfig, ConcealMode};

    const LEADER: &str = "sol";

    /// The spellings WP-6 and the claims demolition retired. `propose_structure` and
    /// `draft_requirement` folded into `propose_claim`, which then fell with the claims seam;
    /// `adopt_unsorted` and `note_timeline` folded into `curate`.
    const RETIRED: [&str; 5] = [
        "adopt_unsorted",
        "draft_requirement",
        "propose_structure",
        "note_timeline",
        "propose_claim",
    ];

    /// The fixture with the leader set mounted on `sol`, `sol` alive, and the code-mode consumer
    /// installed over it: `run` registered and the typed schemas concealed for `sol`.
    ///
    /// Aliases are EMPTY on purpose: V7 is about which TOOLS survived the collapse, so the
    /// injected names here are the registered names.
    async fn open() -> (Fixture, Arc<bough_plugin_tools_codemode::run::Run>) {
        let f = Fixture::open().await;
        f.lane(LEADER).await;
        let _leader = f
            .mount_set(&f.leader_row.clone(), &f.tool_row.clone(), LEADER)
            .await;

        for def in bough_plugin_tools::vocabulary::step_types() {
            let _ = f.ledger.0.register_step_type(def);
        }
        for def in bough_plugin_tools_codemode::vocabulary::step_types() {
            let _ = f.ledger.0.register_step_type(def);
        }

        let js = JsHandle::with_caps(Caps {
            ops: 5_000_000,
            memory_bytes: 32 << 20,
            stack_bytes: 1 << 20,
            wall_ms: 20_000,
            console_bytes: 16_384,
        });
        js.set_engine(
            &f.root,
            Arc::new(QuickJsEngine::new(Arc::new(QuickJsConfig {
                interrupt_check_ops: 10_000,
                max_concurrent_programs: 4,
            }))),
        )
        .await
        .expect("the engine slot is free");

        let cfg = Arc::new(CodemodeConfig {
            caps: None,
            conceal: ConcealMode::Mirror,
            aliases: Default::default(),
            namespaces: Default::default(),
            hide: Default::default(),
            shell_tools: ["bash".to_string()].into_iter().collect(),
            shell_content_result: ["bash".to_string()].into_iter().collect(),
            tags_min: 3,
            tags_max: 5,
            inner_deadline_ms: None,
            max_parallel_calls: 8,
            max_console_bytes: 16_384,
            max_calls_per_program: 16,
            tags_required: false,
            surface_section: false,
        });
        let conceal = Arc::new(Concealment::new(cfg.conceal));
        let run = Arc::new(bough_plugin_tools_codemode::run::Run {
            cfg: cfg.clone(),
            ctx: f.root.clone(),
            fiber: f.root.fiber_uid(),
            js,
            tools: f.tools.clone(),
            ledger: f.ledger.clone(),
            conceal: conceal.clone(),
        });
        f.tools
            .register(&f.root, bough_plugin_tools_codemode::run::spec(run.clone()))
            .await
            .expect("`run` registers");
        conceal
            .install(&f.root, &f.tools, &AgentName::new(LEADER))
            .await
            .expect("the concealment installs");
        (f, run)
    }

    /// Run one program AS THE LEADER, exactly as the loop would.
    async fn program(
        run: &Arc<bough_plugin_tools_codemode::run::Run>,
        root: &Context,
        source: &str,
    ) -> ToolOutcome {
        let call = Arc::new(ToolCall {
            id: ToolCallId::new("call_1"),
            name: ToolName::new("run"),
            args: serde_json::json!({ "program": source }),
            agent: AgentName::new(LEADER),
            wake: WakeId::new("w1"),
            step_index: 1,
        });
        let cx = ToolCx {
            ctx: root.clone(),
            cancel: Default::default(),
            deadline: None,
            initiator: None,
        };
        run.call(call, cx).await.expect("the program ran")
    }

    /// The surviving spelling is a real, callable function: a program that calls `curate` in the
    /// sandbox lands a `timeline/entry` step in the ledger. Nothing is stubbed — this goes
    /// through QuickJS, the host binding, the tools pipeline and the real `curate` tool.
    #[tokio::test]
    async fn a_program_curates_through_the_surviving_function() {
        let (f, run) = open().await;
        assert_eq!(f.timeline_entries().await, 0, "nothing noted yet");

        let out = program(
            &run,
            &f.root,
            &format!(
                "const r = await curate({});\nconsole.log('typeof curate', typeof curate);",
                serde_json::to_string(&timeline_only()).unwrap()
            ),
        )
        .await;

        assert!(
            out.content.contains("typeof curate function"),
            "`curate` must be an injected function, not a name the sandbox never heard: \
             {:?}",
            out.content
        );
        assert_eq!(
            f.timeline_entries().await,
            1,
            "the call must have really noted the moment"
        );
    }

    /// And `create_lane` — the structural survivor — really bears a lane from inside a program.
    #[tokio::test]
    async fn a_program_creates_a_lane_through_the_surviving_function() {
        let (f, run) = open().await;
        program(
            &run,
            &f.root,
            &format!(
                "await create_lane({});",
                serde_json::to_string(&lane_args()).unwrap()
            ),
        )
        .await;
        assert!(
            f.ledger
                .0
                .agent(&AgentName::new("infra"))
                .await
                .expect("the read runs")
                .is_some(),
            "the lane must exist after the program ran"
        );
    }

    /// The old spellings are GONE from the code-mode surface: not injected, and a program
    /// that reaches for one gets a `ReferenceError` rather than a working tool under an old name.
    #[tokio::test]
    async fn the_retired_spellings_are_not_defined_in_the_sandbox() {
        let (f, run) = open().await;
        let probes: String = RETIRED
            .iter()
            .map(|n| format!("console.log('{n}', typeof globalThis.{n});\n"))
            .collect();
        let out = program(&run, &f.root, &probes).await;
        for n in RETIRED {
            assert!(
                out.content.contains(&format!("{n} undefined")),
                "`{n}` must not be injected under code mode: {:?}",
                out.content
            );
        }

        // And calling one is an error, not a silent success.
        let out = program(
            &run,
            &f.root,
            "try { await adopt_unsorted({}); console.log('CALLED'); } \
             catch (e) { console.log('threw', e.constructor.name); }",
        )
        .await;
        assert!(
            out.content.contains("threw ReferenceError"),
            "calling a retired spelling must throw: {:?}",
            out.content
        );
        assert_eq!(
            f.timeline_entries().await,
            0,
            "and it must not have adopted or noted anything"
        );
    }

    /// The BINDING list the consumer builds for the leader — what actually becomes globals —
    /// carries the three and none of the retired. This is the code-mode twin of
    /// `the_set_is_create_lane_merge_lanes_and_curate`, read off the real registry rather than a
    /// constant.
    #[tokio::test]
    async fn the_injected_leader_functions_are_exactly_the_two() {
        let (_f, run) = open().await;
        // The roster the consumer INJECTS is the one it cached before hiding the typed schemas —
        // `visible_specs` after the restriction is on would answer the concealed view (`run`
        // alone), which is a true answer to the wrong question.
        let specs = run
            .conceal
            .cached_specs(&AgentName::new(LEADER))
            .expect("the consumer cached the leader's unconcealed surface");
        let bindings = bough_plugin_tools_codemode::bind::bindings(
            &specs,
            &Default::default(),
            &Default::default(),
        )
        .expect("the bindings build");
        let js: Vec<String> = bindings.iter().map(|b| b.js.clone()).collect();
        for name in TOOL_NAMES {
            assert!(
                js.contains(&name.to_string()),
                "`{name}` must be injected for the leader: {js:?}"
            );
        }
        for gone in RETIRED {
            assert!(
                !js.iter().any(|j| j == gone),
                "`{gone}` is still bound into the sandbox: {js:?}"
            );
            assert!(
                !bindings.iter().any(|b| b.tool == gone),
                "`{gone}` is still a registered tool behind some binding: {bindings:?}"
            );
        }
    }
}
