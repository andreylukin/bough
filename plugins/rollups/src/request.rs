//! Invariant: a request never carries a clock and a report never carries a lie. `at` is injected
//! by the caller (AGENTS.md), and a [`SealPlan`] is TOTAL — every candidate range is either in
//! `blocks` or in `skipped` with the reason it was skipped.

use bough_plugin_ledger::{AgentName, RollupId, Seq, StepId, TrajId};
use chrono::{DateTime, Utc};

use crate::window::Window;

bough_util::brand_id!(
    /// One governance pass. Also the synthetic wake id every step of the pass carries (P4-D2).
    pub struct PassId;
);

/// Who a pass is attributed to.
///
/// Phase 4 always writes [`Attribution::System`]; Phase 5's leader writes
/// [`Attribution::Agent`] with no shape change (§8: "leader-attributed once the leader exists").
#[derive(
    Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase", tag = "by")]
pub enum Attribution {
    Andrey,
    Agent { name: AgentName },
    System,
}

/// One seal pass.
#[derive(Clone, Debug)]
pub struct SealRequest {
    pub agent: AgentName,
    pub traj: TrajId,
    /// Injected, never read from a clock inside the provider (AGENTS.md).
    pub at: DateTime<Utc>,
    /// Seal nothing above this seq. `None` ⇒ `head - seal_lag_steps` (P4-D11).
    pub upto: Option<Seq>,
    /// Cap on model calls for this pass. `None` ⇒ the row's `max_calls_per_pass`.
    pub max_calls: Option<usize>,
    pub attribution: Attribution,
}

/// What a pass WOULD do. Deterministic and total.
#[derive(Clone, Debug, PartialEq)]
pub struct SealPlan {
    pub traj: TrajId,
    pub head: Seq,
    pub upto: Seq,
    pub blocks: Vec<PlannedBlock>,
    pub skipped: Vec<Skip>,
}

/// One block a pass would seal.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedBlock {
    /// The id this block WILL carry. Deterministic; the seal-once guard is its existence (P4-D4).
    pub id: RollupId,
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub inputs: Inputs,
    /// The episode windows this block reduces. One window at tier 1, `fanout` children above.
    pub windows: Vec<Window>,
}

/// What a planned block reduces.
#[derive(Clone, Debug, PartialEq)]
pub enum Inputs {
    /// Tier 1: the raw steps beneath.
    Raw(Vec<StepId>),
    /// Tier k>1: the tier k-1 blocks beneath.
    Blocks(Vec<RollupId>),
}

/// One candidate range the pass did NOT plan, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Skip {
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub why: SkipReason,
}

/// Why a candidate range was skipped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SkipReason {
    /// A `tier:`-namespace block already covers this range at this tier. THE seal-once refusal.
    AlreadySealed,
    /// Inside the `seal_lag_steps` window below the head; the verbatim tail still shows it.
    TooCloseToHead,
    /// Fewer than `min_window_steps` steps; a window this thin is not worth a model call.
    TooShort,
    /// Fewer than `fanout` children exist at the tier below.
    NotEnoughChildren,
    /// The pass hit `max_calls`.
    CallBudget,
    /// The bound provider seals nothing, ever (`rollups-none`).
    Refused,
}

/// What a pass DID.
#[derive(Clone, Debug, PartialEq)]
pub struct SealReport {
    pub pass: PassId,
    pub planned: usize,
    pub sealed: Vec<RollupId>,
    pub skipped: Vec<Skip>,
    pub calls: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub stop: Stop,
}

/// Why a pass stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stop {
    Complete,
    CallBudget,
    NothingToDo,
}

/// Supersede one block: the §3 relief valve.
#[derive(Clone, Debug)]
pub struct SupersedeRequest {
    pub block: RollupId,
    pub reason: String,
    pub at: DateTime<Utc>,
    pub attribution: Attribution,
}

/// What a supersession did.
#[derive(Clone, Debug, PartialEq)]
pub struct SupersedeReport {
    pub old: RollupId,
    /// Generation n+1 over the same `(traj, tier, from_seq, to_seq)`.
    pub new: RollupId,
    /// The appended `memory/expired` marker naming `old`.
    pub note: StepId,
}

/// Rebuild an agent's standing digest.
#[derive(Clone, Debug)]
pub struct DigestRequest {
    pub agent: AgentName,
    pub traj: TrajId,
    pub at: DateTime<Utc>,
    pub attribution: Attribution,
    /// `true` ⇒ ignore the existing digest AND the sealed tiers entirely, and read raw evidence
    /// only. `/reset` sets it: §8's "rebuilds the digest from raw evidence" is a rebuild from the
    /// raw, so nothing a suspected-drifted tier says can seed the replacement.
    pub from_raw: bool,
    /// §3's INHERITANCE digest: the parent chain this digest summarizes FOR the child named by
    /// `traj`. Empty ⇒ the agent's own standing digest, which is the ordinary case. When it is
    /// non-empty the raw evidence is read from these trajectories, the block is written on the
    /// CHILD's trajectory in its own id namespace, and `src_trajs` names the parents — which is
    /// what makes an inheritance digest distinguishable from a standing one in the store, for
    /// graph-ops.
    pub parents: Vec<TrajId>,
}

/// What a digest rebuild did.
#[derive(Clone, Debug, PartialEq)]
pub struct DigestReport {
    pub digest: RollupId,
    pub replaced: Option<RollupId>,
    /// Sealed tier rows READ while building it. Named so a test can assert none were written.
    pub tiers_read: usize,
    pub calls: usize,
}
