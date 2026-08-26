//! Invariant (§4): a structure change is a FACT and it cites what justified it — so all four step
//! types are [`bough_plugin_ledger::ClassRule::Evidence`]. "A cited split event" is §4's own
//! phrase, and an uncited one would make the graph's history unauditable.

use bough_plugin_ledger::{AgentName, Ref, RollupId, Seq, StepId, TrajId};
use bough_plugin_rollups::Attribution;

use crate::plan::UndoShape;

/// The owner string every step type below is registered under.
pub const OWNER: &str = "graph-ops";

/// One child of a split or a bud, as the step records it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ChildRecord {
    pub traj: TrajId,
    /// `None` ⇒ a fork: no `agents` row.
    #[serde(default)]
    pub agent: Option<AgentName>,
    #[serde(default)]
    pub routing_refs: Vec<Ref>,
    #[serde(default)]
    pub digest: Option<RollupId>,
}

/// `graph/split` — Evidence.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GraphSplit {
    pub parent: TrajId,
    pub at_seq: Seq,
    pub children: Vec<ChildRecord>,
    pub reason: String,
    pub by: Attribution,
}

/// `graph/merge` — Evidence.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GraphMerge {
    pub survivor: AgentName,
    pub absorbed: AgentName,
    pub survivor_traj: TrajId,
    pub absorbed_traj: TrajId,
    pub at_seq: Seq,
    pub reconciliation: RollupId,
    pub reason: String,
    pub by: Attribution,
}

/// `graph/bud` — Evidence. A fork is a bud with `agent: None`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GraphBud {
    pub parent: TrajId,
    pub child: TrajId,
    pub at_seq: Seq,
    #[serde(default)]
    pub agent: Option<AgentName>,
    #[serde(default)]
    pub routing_refs: Vec<Ref>,
    pub reason: String,
    pub by: Attribution,
}

/// `graph/undo` — Evidence.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GraphUndo {
    pub of: StepId,
    pub shape: UndoShape,
    pub trajs: Vec<TrajId>,
    pub by: Attribution,
}

/// Declare this crate's four step types on the bound ledger. Called once, from `apply`.
pub fn declare(_ledger: &bough_plugin_ledger::LedgerHandle) -> Result<(), crate::GraphError> {
    todo!("WP-3: declare graph/split, graph/merge, graph/bud, graph/undo as Evidence")
}
