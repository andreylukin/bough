//! Invariant (§8, §3): this row seals a raw segment EXACTLY ONCE. A block's id is a deterministic
//! function of `(traj, tier, from_seq, to_seq, generation)` and EXCLUDES `prompt_ver`, so bumping
//! the prompt can never re-open a sealed range (P4-D4); a sealed row is immutable afterwards and
//! `superseded_by` is the one set-once write. The model is reached through the `agent/request`
//! waterfall, so nothing here names a model and `model-policy` picks terra for unattended work
//! (P4-D3).

pub mod call;
pub mod command;
pub mod digest;
pub mod invariant;
pub mod prompts;
pub mod render;
pub mod resolve;
pub mod seal;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{ClassRule, StepTypeDef};
use bough_plugin_rollups::{
    DigestReport, DigestRequest, RollupsError, SealPlan, SealReport, SealRequest, Summarizer,
    SupersedeReport, SupersedeRequest,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "rollups-summarizer";

/// The step type this crate owns, spelled once.
pub const ROLLUP_REQUEST: &str = "rollup/request";

/// `rollup/request` — a THOUGHT. Model-visible ⟺ ledgered (§0.2): the summarizer's request is
/// reconstructible from `(range, prompt_ver, model)`, and this is the row that records the last
/// two. It is also where the cost bench reads its token counts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RollupRequest {
    pub pass: String,
    pub phase: call::Phase,
    pub prompt_ver: String,
    pub model: String,
    pub tier: u8,
    pub from_seq: u64,
    pub to_seq: u64,
    /// A hash of the rendered input, so a replay can prove the same input produced the same block.
    pub input_digest: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub token_source: call::TokenSource,
}

/// The step types this crate owns.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![
        StepTypeDef::of::<RollupRequest>(ROLLUP_REQUEST, PLUGIN_NAME)
            .class_rule(ClassRule::Thought),
    ]
}

/// The row's validated configuration. No tunable in this crate is a constant (§0.2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SummarizerConfig {
    /// Stamped on every block. Bumping it does NOT re-open a sealed range (P4-D4).
    pub prompt_ver: String,
    /// The episode cut (§8). Minutes, not a `Duration`: a bundle patch is YAML.
    pub gap_minutes: u64,
    pub max_window_steps: usize,
    pub min_window_steps: usize,
    /// §3: ~10.
    pub fanout: usize,
    pub max_tier: u8,
    /// P4-D11: never seal within this many steps of the head.
    pub seal_lag_steps: usize,
    /// A pass makes at most this many model calls (P4-D16).
    pub max_calls_per_pass: usize,
    pub max_notable_refs: usize,
    pub max_evidence_refs: usize,
    pub max_block_chars: usize,
    pub map_max_tokens: i64,
    pub reduce_max_tokens: i64,
}

/// The provider's live state: everything a seal pass needs, resolved once at `apply`.
pub struct SummarizerInner {
    pub ctx: Context,
    pub cfg: Arc<SummarizerConfig>,
    pub ledger: bough_plugin_ledger::LedgerHandle,
    pub llm: bough_plugin_llm::LlmHandle,
    pub agents: bough_plugin_agents::AgentsHandle,
}

/// The recap summarizer.
#[derive(Clone)]
pub struct RecapSummarizer(pub Arc<SummarizerInner>);

#[async_trait::async_trait]
impl Summarizer for RecapSummarizer {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }

    fn prompt_ver(&self) -> &str {
        &self.0.cfg.prompt_ver
    }

    async fn plan(&self, _req: &SealRequest) -> Result<SealPlan, RollupsError> {
        todo!("WP-2: plan a seal pass from the ledger's own rows")
    }

    async fn seal(&self, _req: &SealRequest) -> Result<SealReport, RollupsError> {
        todo!("WP-2: map over episode windows, reduce to themes, seal each block")
    }

    async fn supersede(&self, _req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError> {
        todo!("WP-2: mint generation n+1 and append the expiry note")
    }

    async fn rebuild_digest(&self, _req: &DigestRequest) -> Result<DigestReport, RollupsError> {
        todo!("WP-2: rebuild the standing digest from raw evidence")
    }
}

/// The provider row.
pub struct RollupsSummarizerPlugin;

#[async_trait::async_trait]
impl Plugin for RollupsSummarizerPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SummarizerConfig;

    fn inject() -> Inject {
        // `commands` is OPTIONAL: a headless profile mounts this row with no surface at all and
        // still seals on the schedule hook.
        Inject::required(["ledger", "llm", "agents"]).union(&Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        resolve::validate(cfg)
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-2: provide `rollups`, declare the step type, register `/seal`")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![
            bough_plugin_rollups::invariant::seal_once(),
            bough_plugin_rollups::invariant::tiers_are_an_index(),
        ]
    }
}

bough_kernel::register_plugin!(RollupsSummarizerPlugin);
