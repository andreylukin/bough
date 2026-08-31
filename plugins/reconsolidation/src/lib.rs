//! Invariant (§8): a reconsolidation pass ADDS AND NEVER EDITS. Every write it makes is an append
//! of one of its own kinds — `claim/proposed` for a judged contradiction, `memory/expired` for
//! stale evidence — or a call through the rollups seam for the distilled block. It never calls
//! `seal_rollup` or `supersede_rollup` directly, never modifies a raw step, and never deletes
//! anything: at quiesce, no row hash observed before the first pass has changed.

pub mod command;
pub mod detect;
pub mod invariant;
pub mod pass;
pub mod prompts;
pub mod resolve;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_ledger::{AgentName, RollupId, SeqRange, StepId, StepType, TrajId};
use bough_plugin_rollups::Attribution;
use chrono::{DateTime, Utc};

pub use vocabulary::{
    MemoryExpired, ReconError, ReconKind, ReconRequest, MEMORY_EXPIRED, RECON_REQUEST,
};

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
    /// Who a pass with no attribution of its own is written by. `None` ⇒ [`Attribution::System`],
    /// which is what Phase 4 always wrote. §8 makes the pass leader-attributed ONCE A LEADER
    /// EXISTS, and the leader is the only thing that knows its own name, so it installs itself
    /// here as an effect: moving the leader set moves the attribution with it, and unloading it
    /// puts `System` back.
    pub attribution: parking_lot::Mutex<Option<Attribution>>,
}

impl ReconHandle {
    /// The standing attribution for a pass whose caller does not name one.
    pub fn attribution(&self) -> Attribution {
        self.0
            .attribution
            .lock()
            .clone()
            .unwrap_or(Attribution::System)
    }

    /// Install the standing attribution. An EFFECT (§0.2): the `leader` row calls it from its own
    /// fiber, and its inverse restores whatever was there before.
    pub async fn attribute_to(
        &self,
        ctx: &Context,
        by: Attribution,
    ) -> Result<bough_kernel::EffectHandle, PluginError> {
        let inner = self.0.clone();
        let mine = by;
        ctx.effect(move |e| async move {
            let previous = { inner.attribution.lock().replace(mine.clone()) };
            e.defer_sync(move || {
                let mut slot = inner.attribution.lock();
                // Only give the slot back if it is still OURS: a later installer is not ours to
                // evict.
                if slot.as_ref() == Some(&mine) {
                    *slot = previous;
                }
            });
            Ok(())
        })
        .await
    }

    /// What a pass WOULD do. No model call, no write.
    pub async fn plan(&self, req: &PassRequest) -> Result<PassPlan, ReconError> {
        pass::plan(&self.0, req).await
    }

    /// Run it. ADDS ONLY (§8).
    pub async fn run(&self, req: &PassRequest) -> Result<PassReport, ReconError> {
        pass::run(&self.0, req).await
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
    /// The older half's KIND, carried so the expiry path can refuse a `NEVER_EXPIRABLE` one
    /// without a second ledger read. A pin may legitimately be one half of a contradiction — the
    /// claim is still surfaced — but it is never expired by it (§3, V7).
    pub older_kind: StepType,
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
    /// The judge prompt's version, stamped on every `recon/request` step. Validated at boot: it
    /// must name a prompt in [`prompts::PROMPTS`], so editing the prompt without bumping the
    /// stamp is a boot failure rather than a silent re-use (§0.2, the `rollups-summarizer`
    /// precedent).
    pub judge_prompt_ver: String,
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

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);

        let ledger = bough_plugin_ledger::LedgerHandle(
            ctx.get::<bough_plugin_ledger::Ledger>()
                .map_err(fail)?
                .0
                .clone(),
        );
        // Model-visible ⟺ ledgered (§0.2): the marker this row appends is a declared step type,
        // and the declaration is an EFFECT, so unloading the row leaves the map as it was.
        // `memory/expired` has TWO declarers — this row and `rollups-summarizer`'s supersession
        // note. Both declare the SEAM's definition unconditionally: the map refcounts identical
        // declarations, so neither row's presence, absence or position decides the schema, and
        // unloading one leaves the type standing for the other.
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

        let llm = bough_plugin_llm::LlmHandle(
            ctx.get::<bough_plugin_llm::Llm>().map_err(fail)?.0.clone(),
        );
        let agents = bough_plugin_agents::AgentsHandle(
            ctx.get::<bough_plugin_agents::Agents>()
                .map_err(fail)?
                .0
                .clone(),
        );
        let rollups = bough_plugin_rollups::RollupsHandle(
            ctx.get::<bough_plugin_rollups::Rollups>()
                .map_err(fail)?
                .0
                .clone(),
        );

        let handle = ReconHandle(Arc::new(ReconInner {
            ctx: ctx.clone(),
            cfg,
            ledger,
            llm,
            agents,
            rollups,
            attribution: parking_lot::Mutex::new(None),
        }));
        ctx.provide::<Reconsolidation>(handle.clone())
            .await
            .map_err(fail)?;

        // The recorded stream this row's invariant reads is per fiber LIFE: a reload starts
        // clean, or the reload itself would look like an edit.
        ctx.effect(move |e| async move {
            e.defer_sync(invariant::reset);
            Ok(())
        })
        .await?;

        // `commands` is OPTIONAL: a headless profile mounts this row with no surface at all and
        // still reconsolidates.
        command::register(&ctx, &handle).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::a_pass_adds_and_never_edits()]
    }
}

bough_kernel::register_plugin!(ReconsolidationPlugin);
