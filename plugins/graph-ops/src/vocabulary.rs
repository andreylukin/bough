//! Invariant (§4): a structure change is a FACT and it cites what justified it — so all four step
//! types are [`bough_plugin_ledger::ClassRule::Evidence`]. "A cited split event" is §4's own
//! phrase, and an uncited one would make the graph's history unauditable.

use bough_plugin_ledger::{AgentName, Ref, RollupId, Seq, StepId, TrajId};
use bough_plugin_rollups::Attribution;

use crate::plan::UndoShape;

/// The owner string every step type below is registered under.
pub const OWNER: &str = "graph-ops";

/// `graph/split`.
pub const GRAPH_SPLIT: &str = "graph/split";
/// `graph/merge`.
pub const GRAPH_MERGE: &str = "graph/merge";
/// `graph/bud` — a fork is a bud with `agent: None`.
pub const GRAPH_BUD: &str = "graph/bud";
/// `graph/undo`.
pub const GRAPH_UNDO: &str = "graph/undo";

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

/// The four step types this row declares. All EVIDENCE: a structure change is a fact and the
/// ledger itself refuses one that cannot say what justified it.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    vec![
        StepTypeDef::of::<GraphSplit>(GRAPH_SPLIT, OWNER).class_rule(ClassRule::Evidence),
        StepTypeDef::of::<GraphMerge>(GRAPH_MERGE, OWNER).class_rule(ClassRule::Evidence),
        StepTypeDef::of::<GraphBud>(GRAPH_BUD, OWNER).class_rule(ClassRule::Evidence),
        StepTypeDef::of::<GraphUndo>(GRAPH_UNDO, OWNER).class_rule(ClassRule::Evidence),
    ]
}
