//! Invariant: this crate is a CONSUMER (§0.2). It writes no rollup itself — every digest goes
//! through `ctx.rollups`, which is the Phase 4 seam and the only place a model is called — and it
//! writes no delivery itself: an ambiguous op asks through `ctx.mail.ask_leader`. What it owns is
//! the ORDER of an op (P5-D8) and the refusal to guess routing (§4).

pub mod bud;
pub mod error;
pub mod invariant;
pub mod merge;
pub mod plan;
pub mod route;
pub mod seq;
pub mod split;
pub mod undo;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};

pub use error::GraphError;
pub use plan::{
    merge_head, plan_for, BudRequest, ChildSpec, DigestPlan, ForkRequest, MergeRequest, OpKind,
    OpOutcome, OpPlan, OpRequest, SplitRequest, UndoRequest, UndoShape,
};
pub use route::{merged_row, plan_merge, plan_split, Ambiguity, RoutingPlan, RoutingVerdict};
pub use seq::{inside_open_wake, resolve_point};
pub use vocabulary::{
    step_types, ChildRecord, GraphBud, GraphMerge, GraphSplit, GraphUndo, GRAPH_BUD, GRAPH_MERGE,
    GRAPH_SPLIT, GRAPH_UNDO, OWNER,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "graph-ops";

/// The `graph` service key.
pub struct Graph;

impl ServiceKey for Graph {
    type Value = GraphHandle;
    const NAME: &'static str = "graph";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct GraphHandle(pub Arc<dyn GraphOps>);

/// What a graph-ops provider does.
#[async_trait::async_trait]
pub trait GraphOps: Send + Sync + 'static {
    /// Catalog name of the plugin behind this binding; the swap test reads it.
    fn provider(&self) -> &'static str;

    /// PURE with respect to the world: what an op WOULD write, and every refusal with a reason.
    async fn plan(&self, req: &OpRequest) -> Result<OpPlan, GraphError>;

    /// Execute. Every op is one transaction in the sense that a failure leaves NOTHING half-done
    /// that a later op would trip over: the cited op step is appended LAST (P5-D8).
    async fn apply(&self, req: &OpRequest) -> Result<OpOutcome, GraphError>;

    /// §4's undo rules. [`UndoShape::Pointers`] for an unused split, [`UndoShape::Merge`] for a
    /// lived-in one.
    async fn undo(&self, req: &UndoRequest) -> Result<OpOutcome, GraphError>;
}

/// The synthetic wake every step of one graph op carries (P4-D2's shape: a system pass runs under
/// a synthetic wake id).
pub fn op_wake() -> bough_plugin_ledger::WakeId {
    bough_plugin_ledger::WakeId::new(format!("op:{}", uuid::Uuid::now_v7()))
}

/// How this crate asks Andrey. §4's "ambiguous routing becomes a leader question, never a guess"
/// goes through `ctx.mail.ask_leader`; the trait exists so the ops can be driven — and their
/// refusals asserted — without a live mail seam bound.
#[async_trait::async_trait]
pub trait LeaderAsk: Send + Sync + 'static {
    async fn ask(
        &self,
        q: bough_plugin_mail_router::Question,
    ) -> Result<bough_plugin_ledger::StepId, GraphError>;
}

/// The live seam: `ctx.mail`.
pub struct MailAsk(pub bough_plugin_mail_router::MailHandle);

#[async_trait::async_trait]
impl LeaderAsk for MailAsk {
    async fn ask(
        &self,
        q: bough_plugin_mail_router::Question,
    ) -> Result<bough_plugin_ledger::StepId, GraphError> {
        Ok(self.0.ask_leader(q).await?)
    }
}

/// The bound seams one graph op spends.
pub struct GraphInner {
    pub ledger: bough_plugin_ledger::LedgerHandle,
    pub rollups: bough_plugin_rollups::RollupsHandle,
    pub ask: Arc<dyn LeaderAsk>,
    pub cfg: Arc<GraphConfig>,
}

impl GraphInner {
    /// One agent's row, or the refusal that names it.
    pub async fn row(
        &self,
        name: &bough_plugin_ledger::AgentName,
    ) -> Result<bough_plugin_ledger::AgentRow, GraphError> {
        self.ledger
            .0
            .agent(name)
            .await?
            .ok_or_else(|| GraphError::NoSuchAgent(name.clone()))
    }

    /// The name of the agent whose row points at this trajectory.
    pub async fn name_of(
        &self,
        traj: &bough_plugin_ledger::TrajId,
    ) -> Result<bough_plugin_ledger::AgentName, GraphError> {
        self.ledger
            .0
            .agents()
            .await?
            .into_iter()
            .find(|r| &r.traj == traj)
            .map(|r| r.name)
            .ok_or_else(|| {
                GraphError::NoSuchAgent(bough_plugin_ledger::AgentName::new(traj.as_str()))
            })
    }

    /// A trajectory's head seq, or `Seq(0)` for an empty one.
    pub async fn head(
        &self,
        traj: &bough_plugin_ledger::TrajId,
    ) -> Result<bough_plugin_ledger::Seq, GraphError> {
        Ok(self
            .ledger
            .0
            .head_seq(traj)
            .await?
            .unwrap_or(bough_plugin_ledger::Seq(0)))
    }

    /// P5-D7. An EXPLICIT point inside an open wake is an ERROR; an absent one is RESOLVED down to
    /// the last legal seq. Neither pauses the parent and neither clips silently.
    pub async fn resolve_point(
        &self,
        parent: &bough_plugin_ledger::AgentRow,
        at_seq: Option<bough_plugin_ledger::Seq>,
    ) -> Result<bough_plugin_ledger::Seq, GraphError> {
        let chain = self
            .ledger
            .0
            .steps(&bough_plugin_ledger::StepQuery {
                trajs: vec![parent.traj.clone()],
                order: bough_plugin_ledger::Order::SeqDesc,
                ..Default::default()
            })
            .await?;
        match at_seq {
            Some(at) => {
                if let Some(wake) = open_wake_id(&chain, at) {
                    return Err(GraphError::OpenWake { wake, at_seq: at });
                }
                Ok(at)
            }
            None => seq::resolve_point(&chain)
                .ok_or_else(|| GraphError::NoForkPoint(parent.name.clone())),
        }
    }

    /// Ask, then refuse. The question is asked FIRST and the op returns `Ambiguous`, so nothing is
    /// written while it is open (§4).
    pub async fn refuse<T>(
        &self,
        questions: &[String],
        cites: Vec<bough_plugin_ledger::Cite>,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<T, GraphError> {
        let detail = questions.join("; ");
        if self.cfg.question_on_ambiguity {
            self.ask
                .ask(bough_plugin_mail_router::Question {
                    asked_by: PLUGIN_NAME,
                    about: detail.clone(),
                    options: questions.to_vec(),
                    cites,
                    // §5's wake urgency: a question only Andrey can settle reactivates a dormant
                    // leader, and `class:ask` is how P5-D3 spells that.
                    refs: [bough_plugin_ledger::Ref::new(
                        bough_plugin_mail_router::ASK_CLASS_REF,
                    )]
                    .into_iter()
                    .collect(),
                    at,
                })
                .await?;
        }
        Err(GraphError::Ambiguous { detail })
    }
}

/// The wake id of the open wake `at` sits inside, if any.
fn open_wake_id(
    chain_desc: &[bough_plugin_ledger::Step],
    at: bough_plugin_ledger::Seq,
) -> Option<bough_plugin_ledger::WakeId> {
    if !seq::inside_open_wake(chain_desc, at) {
        return None;
    }
    let mut asc: Vec<&bough_plugin_ledger::Step> = chain_desc.iter().collect();
    asc.sort_by_key(|s| s.seq);
    let mut open: Vec<&bough_plugin_ledger::Step> = Vec::new();
    for s in asc.iter().filter(|s| s.seq <= at) {
        match s.kind.as_str() {
            "wake/start" => open.push(s),
            "wake/end" => open.retain(|o| o.wake != s.wake),
            _ => {}
        }
    }
    open.first().map(|s| s.wake.clone())
}

#[async_trait::async_trait]
impl GraphOps for GraphInner {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }

    async fn plan(&self, req: &OpRequest) -> Result<OpPlan, GraphError> {
        let (parent, explicit) = match req {
            OpRequest::Split(r) => (r.parent.clone(), r.at_seq),
            OpRequest::Bud(r) => (r.parent.clone(), Some(r.at_seq)),
            OpRequest::Fork(r) => (r.parent.clone(), r.at_seq),
            OpRequest::Merge(r) => {
                let rows = self.ledger.0.agents().await?;
                let at = match rows.iter().find(|x| x.name == r.survivor) {
                    Some(row) => self.head(&row.traj).await?,
                    None => bough_plugin_ledger::Seq(0),
                };
                return Ok(plan::plan_for(req, at, &rows, &self.cfg));
            }
        };
        let row = self.row(&parent).await?;
        let at = self.resolve_point(&row, explicit).await?;
        let rows = self.ledger.0.agents().await?;
        Ok(plan::plan_for(req, at, &rows, &self.cfg))
    }

    async fn apply(&self, req: &OpRequest) -> Result<OpOutcome, GraphError> {
        match req {
            OpRequest::Split(r) => split::apply(self, r).await,
            OpRequest::Bud(r) => bud::apply(self, r).await,
            OpRequest::Fork(r) => bud::apply_fork(self, r).await,
            OpRequest::Merge(r) => merge::apply(self, r).await,
        }
    }

    async fn undo(&self, req: &UndoRequest) -> Result<OpOutcome, GraphError> {
        undo::apply(self, req).await
    }
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    /// A split takes exactly this many children.
    pub max_children: usize,
    /// Whether a headless fork gets an inheritance digest. `false` in `bough-base`: a fork has no
    /// row to carry one.
    pub digest_on_fork: bool,
    /// P5-D9: a TEST SEAM, stated openly. `bough-base` never sets it `false`; it exists so the
    /// ambiguity tests can assert the refusal path directly instead of through `plan()`.
    pub question_on_ambiguity: bool,
}

/// The `graph` row.
pub struct GraphOpsPlugin;

#[async_trait::async_trait]
impl Plugin for GraphOpsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = GraphConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "agents", "rollups", "mail"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);

        let ledger = bough_plugin_ledger::LedgerHandle(
            ctx.get::<bough_plugin_ledger::Ledger>()
                .map_err(fail)?
                .0
                .clone(),
        );
        // Model-visible ⟺ ledgered (§0.2): the four op steps are declared types, and the
        // declaration is an EFFECT, so unloading this row leaves the map as it was.
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

        let rollups = bough_plugin_rollups::RollupsHandle(
            ctx.get::<bough_plugin_rollups::Rollups>()
                .map_err(fail)?
                .0
                .clone(),
        );
        let mail = bough_plugin_mail_router::MailHandle(
            ctx.get::<bough_plugin_mail_router::Mail>()
                .map_err(fail)?
                .0
                .clone(),
        );
        // `agents` is a REQUIRED injection: an op that writes rows must not mount before the row
        // seam that owns them. Reading it here is what makes a missing binding a boot failure.
        let _agents = ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(fail)?
            .0
            .clone();

        let handle = GraphHandle(Arc::new(GraphInner {
            ledger,
            rollups,
            ask: Arc::new(MailAsk(mail)),
            cfg,
        }));
        ctx.provide::<Graph>(handle).await.map_err(fail)?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![
            invariant::split_has_two_edges(),
            invariant::merge_is_reconciled(),
            invariant::absorbed_row_is_gone(),
        ]
    }
}

bough_kernel::register_plugin!(GraphOpsPlugin);
