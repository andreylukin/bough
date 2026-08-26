//! Invariant (§8): a reconsolidation pass ADDS AND NEVER EDITS. Every write it makes is an append
//! of one of its own kinds — `claim/proposed` for a judged contradiction, `memory/expired` for
//! stale evidence — or a call through the rollups seam for the distilled block. It never calls
//! `seal_rollup` or `supersede_rollup` directly, never modifies a raw step, and never deletes
//! anything: at quiesce, no row hash observed before the first pass has changed.

pub mod command;
pub mod detect;
pub mod invariant;
pub mod pass;
pub mod resolve;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_ledger::{AgentName, RollupId, SeqRange, StepId, StepType, TrajId};
use bough_plugin_rollups::Attribution;
use chrono::{DateTime, Utc};

pub use vocabulary::{MemoryExpired, ReconError, ReconKind, MEMORY_EXPIRED};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "reconsolidation";

/// The `reconsolidation` service key.
pub struct Reconsolidation;

impl ServiceKey for Reconsolidation {
    type Value = ReconHandle;
    const NAME: &'static str = "reconsolidation";
}

bough_util::brand_id!(
    /// One reconsolidation pass. Also the synthetic wake id every step of the pass carries.
    pub struct ReconPassId;
);

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct ReconHandle(pub Arc<ReconInner>);

/// The row's live state: the pass registry and everything a pass needs, resolved once at `apply`.
pub struct ReconInner {
    pub ctx: Context,
    pub cfg: Arc<ReconConfig>,
    pub ledger: bough_plugin_ledger::LedgerHandle,
    pub llm: bough_plugin_llm::LlmHandle,
    pub agents: bough_plugin_agents::AgentsHandle,
    pub rollups: bough_plugin_rollups::RollupsHandle,
}

impl ReconHandle {
    /// What a pass WOULD do. No model call, no write.
    pub async fn plan(&self, _req: &PassRequest) -> Result<PassPlan, ReconError> {
        todo!("WP-3: plan a reconsolidation pass")
    }

    /// Run it. ADDS ONLY (§8).
    pub async fn run(&self, _req: &PassRequest) -> Result<PassReport, ReconError> {
        todo!("WP-3: run a reconsolidation pass")
    }
}

/// One reconsolidation pass.
#[derive(Clone, Debug)]
pub struct PassRequest {
    pub agent: AgentName,
    pub traj: TrajId,
    pub at: DateTime<Utc>,
    /// Distil from this seq onward. `None` ⇒ the newest `batch_steps`.
    pub since: Option<bough_plugin_ledger::Seq>,
    /// Phase 4 always `System`; Phase 5's leader writes `Agent { name }` with no shape change.
    pub attribution: Attribution,
    pub max_calls: Option<usize>,
}

/// What a pass WOULD do.
#[derive(Clone, Debug, PartialEq)]
pub struct PassPlan {
    pub range: SeqRange,
    pub distil: bool,
    pub contradiction_candidates: Vec<Pair>,
    pub expiry_candidates: Vec<Candidate>,
}

/// Two EVIDENCE steps sharing at least one ref, ordered oldest-first.
///
/// The pure half of contradiction detection: the pairing is arithmetic, the judgement is the
/// model's.
#[derive(Clone, Debug, PartialEq)]
pub struct Pair {
    pub older: StepId,
    pub newer: StepId,
    pub shared: Vec<bough_plugin_ledger::Ref>,
}

/// One stale-evidence candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub step: StepId,
    pub kind: StepType,
    pub age_days: i64,
    pub why: StaleReason,
}

/// Why a step is a stale-evidence candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StaleReason {
    /// Older than `stale_after_days` and of an expirable kind.
    Age,
    /// A newer EVIDENCE step on the same ref contradicts it (the model said so).
    Contradicted,
}

/// What a pass DID.
#[derive(Clone, Debug, PartialEq)]
pub struct PassReport {
    pub pass: ReconPassId,
    /// The distilled digest, when the pass produced one.
    pub distilled: Option<RollupId>,
    /// The `claim/proposed` steps the contradictions became.
    pub contradictions: Vec<StepId>,
    /// The `memory/expired` markers appended.
    pub expired: Vec<StepId>,
    pub calls: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

/// The row's validated configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReconConfig {
    pub batch_steps: usize,
    pub stale_after_days: i64,
    /// The ONLY kinds a pass may expire. [`bough_plugin_rollups::NEVER_EXPIRABLE`] is intersected
    /// out in code, so a misconfiguration cannot expire a pin (V7).
    pub expirable_kinds: Vec<String>,
    pub max_contradiction_pairs: usize,
    pub max_calls_per_pass: usize,
    pub distill_max_tokens: i64,
}

/// The reconsolidation row.
pub struct ReconsolidationPlugin;

#[async_trait::async_trait]
impl Plugin for ReconsolidationPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ReconConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "llm", "agents", "rollups"])
            .union(&Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        resolve::validate(cfg)
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-3: provide `reconsolidation`, declare `memory/expired`, register /reconsolidate")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::a_pass_adds_and_never_edits()]
    }
}

bough_kernel::register_plugin!(ReconsolidationPlugin);
