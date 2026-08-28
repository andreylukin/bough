//! The fixture the two leader test files share: an in-memory ledger, a live agents seam with an
//! idle driver, the real mail router, the real claims seam over a graph seam that refuses every
//! op, a real assembler, and the `leader` row applied over all of it.
//!
//! DEVIATION from the WP-5 file list, named on purpose: the plan lists two test files and no
//! support module. Two copies of this harness would be two places for it to drift, and
//! `plugins/projection-assembler/tests/support` is the precedent for putting it in one.

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, EntryId, KernelCore, Plugin};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, Agents, AgentsHandle, Attach,
    CancelCause, CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_claims::{Claims, ClaimsConfig, ClaimsHandle};
use bough_plugin_graph_ops::{
    Graph, GraphError, GraphHandle, GraphOps, OpOutcome, OpPlan, OpRequest, UndoRequest,
};
use bough_plugin_leader::{Leader, LeaderConfig, LeaderHandle, LeaderPlugin};
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, Ref, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_mail_router::{Mail, MailConfig, MailHandle};
use bough_plugin_projection::{Projection, ProjectionHandle, Projector};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};

/// The unsorted trajectory every case here shares.
pub const UNSORTED: &str = "t-unsorted";

pub struct Fixture {
    pub root: Context,
    /// The `leader` row's own context: its fiber owns every registration the row makes.
    pub row: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub mail: MailHandle,
    pub claims: ClaimsHandle,
    pub projection: ProjectionHandle,
    _dir: tempfile::TempDir,
    _slots: Vec<Box<dyn std::any::Any>>,
}

impl Fixture {
    pub async fn open() -> Fixture {
        let root = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(root.clone()) as Arc<_>);
        let dir = tempfile::tempdir().expect("a temp dir");

        let agents = AgentsHandle::new(root.clone(), ledger.clone());
        agents
            .set_factory(&root, Arc::new(IdleFactory) as Arc<dyn AgentFactory>)
            .await
            .expect("the slot is free");

        // The mail router's own step types: its `apply` declares them, and these tests drive the
        // handle rather than the row.
        ledger
            .declare_step_types(&root, bough_plugin_mail_router::vocabulary::step_types())
            .await
            .expect("the mail step types declare");
        let mail = MailHandle::new(
            root.clone(),
            ledger.clone(),
            agents.clone(),
            Arc::new(MailConfig {
                unsorted_traj: UNSORTED.to_string(),
                unsorted_limit: 50,
                tolerate_absent_lane: true,
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
            Box::new(
                root.provide::<Claims>(claims.clone())
                    .await
                    .expect("claims"),
            ),
            Box::new(root.provide::<Graph>(graph).await.expect("graph")),
            Box::new(
                root.provide::<Projection>(projection.clone())
                    .await
                    .expect("projection"),
            ),
        ];

        let row = row_ctx(&root);
        Fixture {
            root,
            row,
            ledger,
            agents,
            mail,
            claims,
            projection,
            _dir: dir,
            _slots: slots,
        }
    }

    /// A FRESH row context, so a second `apply` is a second life of the row rather than a second
    /// registration on the first one — which is what a reload is.
    pub fn fresh_row(&self) -> Context {
        row_ctx(&self.root)
    }

    /// Apply the `leader` row for `target`, on `row`.
    pub async fn mount_leader_on(&self, row: &Context, target: &str) -> LeaderHandle {
        LeaderPlugin::apply(row.clone(), Arc::new(config(target)))
            .await
            .expect("the leader row applies");
        (*row.get::<Leader>().expect("ctx.leader is bound")).clone()
    }

    /// Apply the `leader` row for `target` on this fixture's row context.
    pub async fn mount_leader(&self, target: &str) -> LeaderHandle {
        let row = self.row.clone();
        self.mount_leader_on(&row, target).await
    }

    /// A live agent with an `agents` row: `routing_refs` decide who mail reaches.
    pub async fn lane(&self, name: &str, refs: &[&str]) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                TrajId::new(format!("t-{name}")),
                now(),
            ))
            .await
            .expect("the agent is created");
        std::mem::forget(disposer);
        if !refs.is_empty() {
            self.mail
                .link_ref(
                    &AgentName::new(name),
                    refs.iter().map(|r| Ref::new(*r)).collect(),
                    now(),
                )
                .await
                .expect("the refs link");
        }
        agent
    }
}

pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

pub fn config(target: &str) -> LeaderConfig {
    LeaderConfig {
        agent: target.to_string(),
        persona: format!("You are {target}, and you lead."),
        adopt_batch: 8,
        attribute_reconsolidation: true,
    }
}

fn row_ctx(root: &Context) -> Context {
    root.for_row(
        root.core().new_fiber_uid(),
        EntryId::new("leader"),
        LeaderPlugin::NAME,
        LeaderPlugin::inject(),
    )
}

/// An envelope nobody's routing refs match.
pub fn envelope(subject: &str, refs: &[&str]) -> bough_plugin_mail_router::Envelope {
    bough_plugin_mail_router::Envelope {
        from: bough_plugin_agents::Sender::System("test"),
        class: bough_plugin_agents::MailClass::Ordinary,
        subject: subject.to_string(),
        summary: format!("{subject} (summary)"),
        text: format!("{subject} (text)"),
        cites: vec![bough_plugin_ledger::Cite {
            r#ref: Ref::new("gh:bough/rebuild#1"),
            url: None,
        }],
        refs: refs.iter().map(|r| Ref::new(*r)).collect(),
        dedupe_on: None,
        at: now(),
    }
}

// ---- the stubs -------------------------------------------------------------------------------

/// A graph seam that refuses every op: nothing in these two files performs one, and a refusal is
/// a louder failure than a silent success would be.
pub struct RefusingGraph;

#[async_trait::async_trait]
impl GraphOps for RefusingGraph {
    fn provider(&self) -> &'static str {
        "refusing-graph"
    }
    async fn plan(&self, _req: &OpRequest) -> Result<OpPlan, GraphError> {
        Err(GraphError::Other(anyhow::anyhow!(
            "no graph provider in this fixture"
        )))
    }
    async fn apply(&self, _req: &OpRequest) -> Result<OpOutcome, GraphError> {
        Err(GraphError::Other(anyhow::anyhow!(
            "no graph provider in this fixture"
        )))
    }
    async fn undo(&self, _req: &UndoRequest) -> Result<OpOutcome, GraphError> {
        Err(GraphError::Other(anyhow::anyhow!(
            "no graph provider in this fixture"
        )))
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
