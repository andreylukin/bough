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
    plan_for, BudRequest, ChildSpec, DigestPlan, ForkRequest, MergeRequest, OpKind, OpOutcome,
    OpPlan, OpRequest, SplitRequest, UndoRequest, UndoShape,
};
pub use route::{plan_merge, plan_split, Ambiguity, RoutingPlan, RoutingVerdict};
pub use seq::{inside_open_wake, resolve_point};
pub use vocabulary::{ChildRecord, GraphBud, GraphMerge, GraphSplit, GraphUndo, OWNER};

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

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-3: declare the step types, provide `graph`")
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
