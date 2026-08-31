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
use bough_plugin_ledger::{ClassRule, Ledger, StepTypeDef};
use bough_plugin_rollups::{
    DigestReport, DigestRequest, Rollups, RollupsError, RollupsHandle, SealPlan, SealReport,
    SealRequest, Summarizer, SupersedeReport, SupersedeRequest,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "rollups-summarizer";

/// The step type this crate owns, spelled once.
pub const ROLLUP_REQUEST: &str = "rollup/request";

/// §8's APPENDED expiry marker. This row declares it as well as `reconsolidation` does, because a
/// supersession leaves one and this row must work in a composition where `reconsolidation` is not
/// mounted. BOTH declare the seam's one definition unconditionally: the step-type map refcounts
/// identical declarations, so mount order decides nothing and unloading either row leaves the
/// type standing for the other (§0.2).
pub use bough_plugin_rollups::EXPIRED_STEP_TYPE as MEMORY_EXPIRED;

/// The step kinds a governance pass itself writes. They ride the agent's own trajectory (P4-D2)
/// and are therefore EXCLUDED from the material a pass windows: a summarizer must not summarize
/// its own request log, and a pass's own appends must not move the seal-lag ceiling.
pub const GOVERNANCE_KINDS: &[&str] = &[ROLLUP_REQUEST, "rollup/sealed", MEMORY_EXPIRED];

/// The `kind` a supersession's marker carries.
pub const EXPIRY_KIND_SUPERSESSION: bough_plugin_rollups::ExpiryKind =
    bough_plugin_rollups::ExpiryKind::Supersession;

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
    /// `true` when the stream failed before it ended. The call still happened and may still have
    /// been billed, so it is still recorded: a model call the ledger cannot see is exactly what
    /// §0.2 forbids, and the cost bench reads its dollars from these rows.
    #[serde(default)]
    pub failed: bool,
}

/// `memory/expired` — EVIDENCE, and the SEAM's body, not this row's own: one field, one spelling
/// (see [`bough_plugin_rollups::ExpiredBody`]).
pub use bough_plugin_rollups::ExpiredBody as MemoryExpired;

/// The step types this row declares. `rollup/request` is its own; `memory/expired` is the seam's,
/// declared identically by every row that appends one.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![
        StepTypeDef::of::<RollupRequest>(ROLLUP_REQUEST, PLUGIN_NAME)
            .class_rule(ClassRule::Thought),
        bough_plugin_rollups::expiry::step_type_def(),
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

/// The values the `rollups` row carries in `bough-base`, spelled once so a test and the bundle
/// cannot drift apart.
pub fn bundle_config() -> SummarizerConfig {
    SummarizerConfig {
        prompt_ver: prompts::R4_1.to_string(),
        gap_minutes: 45,
        max_window_steps: 10,
        min_window_steps: 2,
        fanout: 10,
        max_tier: 3,
        seal_lag_steps: 20,
        max_calls_per_pass: 8,
        max_notable_refs: 12,
        max_evidence_refs: 24,
        max_block_chars: 1200,
        map_max_tokens: 1024,
        reduce_max_tokens: 1536,
    }
}

/// The provider's live state: everything a seal pass needs, resolved once at `apply`.
pub struct SummarizerInner {
    pub ctx: Context,
    pub cfg: Arc<SummarizerConfig>,
    pub ledger: bough_plugin_ledger::LedgerHandle,
    pub llm: bough_plugin_llm::LlmHandle,
    /// The composition fingerprint, for the facts a policy listener reads. Empty when the kernel
    /// resolves none — a pass appends no `request/header`, so an absent fingerprint is a missing
    /// FACT rather than a missing stamp, and stating it empty is honest.
    pub composition: String,
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

    async fn plan(&self, req: &SealRequest) -> Result<SealPlan, RollupsError> {
        seal::plan(&self.0, req).await
    }

    async fn seal(&self, req: &SealRequest) -> Result<SealReport, RollupsError> {
        seal::run(&self.0, req).await
    }

    async fn supersede(&self, req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError> {
        seal::supersede(&self.0, req).await
    }

    async fn rebuild_digest(&self, req: &DigestRequest) -> Result<DigestReport, RollupsError> {
        digest::rebuild(&self.0, req).await
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
        // `agents` is REQUIRED and no handle is kept: this row reads and repoints agent ROWS
        // through the ledger (`agent`, `put_agent`, `agents`), and those rows are the `agents`
        // row's to own. The key is the activation gate that says so — a digest rebuild that
        // repoints a row nobody manages is not a rebuild — and holding a handle it never calls
        // would misstate the contract in the other direction.
        Inject::required(["ledger", "llm", "agents"]).union(&Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        resolve::validate(cfg)
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let err = |e: anyhow::Error| PluginError::new(ctx.entry_id().clone(), e);
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        let llm = ctx
            .get::<bough_plugin_llm::Llm>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        // Read to PROVE the required key is bound at this point, then dropped: see `inject`.
        let _agents = ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        // Declaration is an EFFECT, so unloading this row leaves the step-type map as it was.
        // `memory/expired` is declared unconditionally even though `reconsolidation` declares it
        // too: identical declarations are refcounted, so neither row depends on the other's
        // presence or position.
        ledger.declare_step_types(&ctx, step_types()).await?;

        let summarizer = RecapSummarizer(Arc::new(SummarizerInner {
            ctx: ctx.clone(),
            cfg: cfg.clone(),
            ledger: (*ledger).clone(),
            llm: (*llm).clone(),
            composition: ctx
                .kernel()
                .and_then(|k| k.composition())
                .map(|c| c.fingerprint.as_str().to_string())
                .unwrap_or_default(),
        }));
        command::register(&ctx, &summarizer).await?;
        ctx.provide::<Rollups>(RollupsHandle(Arc::new(summarizer)))
            .await
            .map_err(|e| err(anyhow::anyhow!(e)))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![
            bough_plugin_rollups::invariant::seal_once(),
            bough_plugin_rollups::invariant::tiers_are_an_index(),
        ]
    }
}

bough_kernel::register_plugin!(RollupsSummarizerPlugin);
