//! Invariant: `steps`, `edges` and `rollups` are append-only; `superseded_by` is the ONE permitted
//! write to a sealed rollup and it is set once. `agents` is MUTABLE CONFIG and is explicitly
//! exempt from append-only (§3).

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::id::{ActionId, AgentName, IdemKey, Ref, RollupId, Seq, TrajId, WakeId};

/// How two trajectories are related.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// `child` forked from `parent` at `at_seq`.
    Ancestor,
    /// `child` merged `parent`'s trajectory in at `at_seq`.
    Merge,
}

/// One edge of the trajectory graph.
#[derive(Clone, PartialEq, Debug)]
pub struct Edge {
    pub child: TrajId,
    pub parent: TrajId,
    pub at_seq: Seq,
    pub kind: EdgeKind,
    pub at: DateTime<Utc>,
}

/// What a rollup is. Phase 4 produces them; Phase 1 stores and consumes them.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum RollupKind {
    /// A tier summary over a seq range.
    Tier,
    /// An agent's standing digest.
    Digest,
    /// A reconciliation of divergent trajectories.
    Reconciliation,
}

/// A rollup as the caller seals it.
#[derive(Clone, Debug)]
pub struct NewRollup {
    /// `None` ⇒ the provider mints one.
    pub id: Option<RollupId>,
    pub traj: TrajId,
    pub kind: RollupKind,
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub src_trajs: Vec<TrajId>,
    pub body: serde_json::Value,
    /// The refs this rollup is notable for. EMPTY means "notable to everyone" (P1-D13).
    pub notable_refs: BTreeSet<Ref>,
    pub prompt_ver: String,
    pub sealed_at: DateTime<Utc>,
}

/// A sealed rollup.
#[derive(Clone, Debug, PartialEq)]
pub struct Rollup {
    pub id: RollupId,
    pub traj: TrajId,
    pub kind: RollupKind,
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub src_trajs: Vec<TrajId>,
    pub body: serde_json::Value,
    pub notable_refs: BTreeSet<Ref>,
    pub prompt_ver: String,
    pub sealed_at: DateTime<Utc>,
    /// Set once, NULL → value, and never back (§3).
    pub superseded_by: Option<RollupId>,
}

/// §3: `agents` is MUTABLE CONFIG, explicitly exempt from append-only.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub name: AgentName,
    pub traj: TrajId,
    /// The refs this agent is routed by; `connected()` matches steps against them.
    pub routing_refs: BTreeSet<Ref>,
    pub wake_classes: BTreeSet<String>,
    pub model_override: Option<String>,
    pub tick_floor: Option<Duration>,
    /// The agent's standing digest, rendered in the identity band.
    pub digest_rollup: Option<RollupId>,
}

/// Where an action stands. Phase 1 stores it; Phase 2 owns the policy (P1-D11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionStatus {
    Intent,
    Done,
    Failed,
}

/// An action as the caller declares its intent.
#[derive(Clone, Debug)]
pub struct NewAction {
    pub id: Option<ActionId>,
    pub wake: WakeId,
    pub idem_key: IdemKey,
    pub kind: String,
    pub payload: serde_json::Value,
    pub at: DateTime<Utc>,
}

/// A row of the actions journal.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionRow {
    pub id: ActionId,
    pub wake: WakeId,
    pub idem_key: IdemKey,
    pub kind: String,
    pub payload: serde_json::Value,
    pub status: ActionStatus,
    pub result: Option<serde_json::Value>,
    pub at: DateTime<Utc>,
    pub done_at: Option<DateTime<Utc>>,
}
