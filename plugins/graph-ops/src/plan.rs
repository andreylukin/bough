//! Invariant: [`GraphOps::plan`] is PURE with respect to the world — it reads the ledger, calls no
//! model and writes nothing — and it is TOTAL: every child is either planned or named in
//! `questions`. A plan that silently omits a child is a plan that would half-apply.

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, EdgeKind, Ref, RollupId, Seq, StepId, TrajId};
use bough_plugin_rollups::Attribution;
use chrono::{DateTime, Utc};

use crate::route::RoutingPlan;

/// Which op a plan or an outcome is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Split,
    Merge,
    Bud,
    Fork,
    Undo,
}

/// One op, as a caller asks for it.
#[derive(Clone, Debug)]
pub enum OpRequest {
    Split(SplitRequest),
    Merge(MergeRequest),
    Bud(BudRequest),
    Fork(ForkRequest),
}

/// One new branch a split or a bud creates.
#[derive(Clone, Debug, PartialEq)]
pub struct ChildSpec {
    /// `None` ⇒ a HEADLESS branch: a trajectory and an ancestor edge, no `agents` row (§4's
    /// fork). Promotable later by adding the row and nothing else.
    pub agent: Option<AgentName>,
    pub traj: TrajId,
    pub routing_refs: BTreeSet<Ref>,
    pub wake_classes: BTreeSet<String>,
}

/// A split: two heads from one, at the parent's head.
#[derive(Clone, Debug)]
pub struct SplitRequest {
    pub parent: AgentName,
    /// `None` ⇒ the parent's head, resolved to the last seq outside an open wake (P5-D7).
    pub at_seq: Option<Seq>,
    /// Exactly two. Each names the new lane and the refs it takes with it.
    pub children: Vec<ChildSpec>,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<bough_plugin_ledger::Cite>,
    pub at: DateTime<Utc>,
}

/// A bud: a split at a PAST point, and the parent never pauses (§4).
#[derive(Clone, Debug)]
pub struct BudRequest {
    pub parent: AgentName,
    /// The PAST point. Mandatory: a bud whose point is the head is a split.
    pub at_seq: Seq,
    pub child: ChildSpec,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<bough_plugin_ledger::Cite>,
    pub at: DateTime<Utc>,
}

/// A fork: a bud with no `agents` row and no routing.
#[derive(Clone, Debug)]
pub struct ForkRequest {
    pub parent: AgentName,
    pub at_seq: Option<Seq>,
    pub traj: TrajId,
    pub reason: String,
    pub by: Attribution,
    pub at: DateTime<Utc>,
}

/// A merge: two lanes into one surviving row.
#[derive(Clone, Debug)]
pub struct MergeRequest {
    /// ANDREY'S CHOICE. Never inferred; the absence of one is a leader question, not a default.
    pub survivor: AgentName,
    pub absorbed: AgentName,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<bough_plugin_ledger::Cite>,
    pub at: DateTime<Utc>,
}

/// An undo of a previous `graph/split` or `graph/bud`.
#[derive(Clone, Debug)]
pub struct UndoRequest {
    /// The `graph/split` or `graph/bud` step being undone.
    pub of: StepId,
    pub by: Attribution,
    pub at: DateTime<Utc>,
}

/// What an op WOULD write.
#[derive(Clone, Debug, PartialEq)]
pub struct OpPlan {
    pub kind: OpKind,
    pub at_seq: Seq,
    pub new_trajs: Vec<TrajId>,
    pub edges: Vec<(TrajId, TrajId, EdgeKind)>,
    pub digests: Vec<DigestPlan>,
    pub routing: RoutingPlan,
    /// Non-empty ⇒ `apply` refuses and `ask_leader` is the caller's next move.
    pub questions: Vec<String>,
}

/// One digest an op would ask `ctx.rollups` for. `reconcile` selects P5-D13's `recon:` namespace.
#[derive(Clone, Debug, PartialEq)]
pub struct DigestPlan {
    pub traj: TrajId,
    pub parents: Vec<TrajId>,
    pub reconcile: bool,
}

/// What an op DID.
#[derive(Clone, Debug, PartialEq)]
pub struct OpOutcome {
    pub kind: OpKind,
    /// The cited op step (`graph/split` | `graph/merge` | `graph/bud` | `graph/undo`), appended
    /// LAST (P5-D8).
    pub step: StepId,
    pub trajs: Vec<TrajId>,
    pub edges: usize,
    pub digests: Vec<RollupId>,
    pub rows_written: Vec<AgentName>,
    pub rows_deleted: Vec<AgentName>,
    /// `Pointers` did no summarising; `Merge` produced a reconciliation digest.
    pub undo_shape: Option<UndoShape>,
}

/// The two shapes §4's undo rules allow.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum UndoShape {
    /// The children were never lived in: delete the rows, restore the refs, no model call.
    Pointers,
    /// A child has steps beyond its `fork/end-seed`: run the merge path, which reconciles.
    Merge,
}

/// PURE over what the caller hands it: the plan for one request, given the parent's chain and the
/// rows in play.
pub fn plan_for(_req: &OpRequest, _at_seq: Seq, _rows: &[bough_plugin_ledger::AgentRow]) -> OpPlan {
    todo!("WP-3: build the total plan, naming every ambiguity as a question")
}
