//! Invariant (§8): a reset REBUILDS and never RESEALS. `/reset <agent>` rebuilds the digest, the
//! identity and the about-line's STATE half from raw evidence, leaves the intent half empty, and
//! leaves every sealed tier exactly as it was — the tier count on the trajectory is reported
//! before and after and is equal by construction. A suspected-bad tier block is SUPERSEDED (a new
//! block plus an expiry note), never re-summarized in place.
//!
//! Signals are read-only: computing them appends nothing.

pub mod command;
pub mod invariant;
pub mod reset;
pub mod resolve;
pub mod signals;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_agents::Agents;
use bough_plugin_ledger::{
    AgentName, Ledger, LedgerHandle, Order, Rollup, RollupId, RollupKind, RollupQuery, SeqRange,
    Step, StepId, StepQuery, TrajId,
};
use bough_plugin_rollups::{Attribution, Rollups, SupersedeReport, SupersedeRequest};
use chrono::{DateTime, Utc};

pub use vocabulary::{DriftError, DriftReset, DRIFT_RESET};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "drift-watch";

/// The `drift` service key.
pub struct Drift;

impl ServiceKey for Drift {
    type Value = DriftHandle;
    const NAME: &'static str = "drift";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct DriftHandle(pub Arc<DriftInner>);

/// The row's live state: the signal-window cache and everything a reset needs.
pub struct DriftInner {
    pub ctx: Context,
    pub cfg: Arc<DriftConfig>,
    pub ledger: bough_plugin_ledger::LedgerHandle,
    pub agents: bough_plugin_agents::AgentsHandle,
    pub rollups: bough_plugin_rollups::RollupsHandle,
}

impl DriftHandle {
    /// Per-agent stability signals, computed from the ledger. Reads only; appends nothing.
    ///
    /// `at` is injected and unused by the arithmetic today: every signal of §8 is a shape of the
    /// last `window_steps` rows, not an age. It stays in the signature because the
    /// claim-rejection signal Phase 5 activates is a RATE, and a rate needs the instant it was
    /// taken at — adding the parameter later would change every caller.
    pub async fn signals(
        &self,
        agent: &AgentName,
        _at: DateTime<Utc>,
    ) -> Result<Signals, DriftError> {
        let traj = traj_of(&self.0, agent).await?;
        let (window, steps) = read_window(&self.0, &traj).await?;
        Ok(signals::compute(agent, window, &steps, &self.0.cfg))
    }

    /// §8's one-command reset.
    pub async fn reset(&self, req: &ResetRequest) -> Result<ResetReport, DriftError> {
        reset::run(&self.0, req).await
    }

    /// Supersede a suspected-bad tier block: a thin call through the rollups seam (§3's relief
    /// valve). It lives here because §8 puts "if a tier block itself is suspected bad" inside the
    /// drift-watch paragraph, and the suspicion is what this row surfaces. Nothing here writes a
    /// sealed row: the provider mints generation n+1 and sets `superseded_by` once.
    pub async fn supersede(&self, req: &SupersedeRequest) -> Result<SupersedeReport, DriftError> {
        Ok(self.0.rollups.0.supersede(req).await?)
    }

    /// The trajectory of an agent, as the ledger's mutable `agents` row records it.
    pub async fn trajectory(&self, agent: &AgentName) -> Result<TrajId, DriftError> {
        traj_of(&self.0, agent).await
    }
}

/// The agent's trajectory, refused loudly when there is none. The explicit resolution step (§0.2):
/// never a `?? default` trajectory, which would compute signals over somebody else's rows.
async fn traj_of(inner: &DriftInner, agent: &AgentName) -> Result<TrajId, DriftError> {
    let row = inner
        .ledger
        .0
        .agent(agent)
        .await?
        .ok_or_else(|| DriftError::NoSuchAgent(agent.to_string()))?;
    if row.traj.as_str().is_empty() {
        return Err(DriftError::NoTrajectory(agent.to_string()));
    }
    Ok(row.traj)
}

/// The signal window and the steps in it. READS ONLY.
pub(crate) async fn read_window(
    inner: &DriftInner,
    traj: &TrajId,
) -> Result<(SeqRange, Vec<Step>), DriftError> {
    let head = inner.ledger.0.head_seq(traj).await?;
    let window = resolve::window(head, &inner.cfg);
    if window.from > window.to {
        return Ok((window, Vec::new()));
    }
    let steps = inner
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            // `after` is exclusive on both providers, so the window's first seq is included by
            // asking for everything above the seq below it.
            after: Some(bough_plugin_ledger::Seq(window.from.0.saturating_sub(1))),
            before: Some(bough_plugin_ledger::Seq(window.to.0 + 1)),
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?;
    Ok((window, steps))
}

/// Sealed `tier` rollups on a trajectory, superseded ones included.
///
/// SUPERSEDED ROWS COUNT. They are still sealed rows, and excluding them would let a reset that
/// superseded a tier report an unchanged count — which is precisely the violation the count
/// exists to catch.
pub(crate) async fn count_tiers(inner: &DriftInner, traj: &TrajId) -> Result<usize, DriftError> {
    Ok(tier_rollups(inner, traj).await?.len())
}

pub(crate) async fn tier_rollups(
    inner: &DriftInner,
    traj: &TrajId,
) -> Result<Vec<Rollup>, DriftError> {
    Ok(inner
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![traj.clone()],
            kind: Some(RollupKind::Tier),
            include_superseded: true,
            ..Default::default()
        })
        .await?)
}

/// One agent's stability signals.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Signals {
    pub agent: AgentName,
    pub window: SeqRange,
    pub samples: usize,
    /// Thought-length variance (§8), over `thought/text` step bodies, in o200k tokens.
    pub thought_len: Stat,
    /// Tool-use distribution, over `tool/call` steps: share per tool, most-used first.
    pub tool_use: Vec<ToolShare>,
    /// Normalised Shannon entropy of `tool_use`, 0.0 (one tool only) .. 1.0 (uniform).
    pub tool_entropy: f64,
    /// Wired, INACTIVE until Phase 5's accept/reject surface exists (§8).
    pub claim_rejection: SignalState,
    pub flags: Vec<DriftFlag>,
}

/// A one-dimensional summary of a sample.
#[derive(
    Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Stat {
    pub n: usize,
    pub mean: f64,
    pub variance: f64,
    pub cv: f64,
    pub p50: f64,
    pub p95: f64,
}

/// One tool's share of the window's calls.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ToolShare {
    pub tool: String,
    pub calls: usize,
    pub share: f64,
}

/// A signal that exists but cannot be computed yet says SO, rather than reporting a zero that
/// reads like "no rejections" (§16: uncertainty never becomes assertion).
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SignalState {
    Inactive { since: String },
    Active { value: f64, n: usize },
}

/// What the signals flagged.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DriftFlag {
    ThoughtLengthUnstable,
    ToolUseCollapsed,
    TooFewSamples,
}

/// §8's one-command reset.
#[derive(Clone, Debug)]
pub struct ResetRequest {
    pub agent: AgentName,
    pub traj: TrajId,
    pub at: DateTime<Utc>,
    pub attribution: Attribution,
}

/// What a reset did.
#[derive(Clone, Debug, PartialEq)]
pub struct ResetReport {
    /// The rebuilt digest (`Summarizer::rebuild_digest` with `from_raw: true`).
    pub digest: RollupId,
    pub replaced_digest: Option<RollupId>,
    /// The fresh `about/line` step: state half from raw evidence, intent half EMPTY.
    pub about_line: StepId,
    /// The `drift/reset` step recording the act.
    pub reset_step: StepId,
    /// Sealed tier rows on the trajectory, before and after. Equal, by construction (§8).
    pub tiers_before: usize,
    pub tiers_after: usize,
}

/// The row's validated configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftConfig {
    pub window_steps: usize,
    pub min_samples: usize,
    /// Coefficient of variation above which [`DriftFlag::ThoughtLengthUnstable`] is raised.
    pub thought_len_cv_flag: f64,
    /// Normalised entropy below which [`DriftFlag::ToolUseCollapsed`] is raised.
    pub tool_entropy_flag: f64,
    /// How many raw steps a `/reset` cites under the rebuilt state half.
    pub max_evidence_cites: usize,
    /// How long the rebuilt STATE half may be, in characters. The same quantity the `about-line`
    /// row carries as `max_state_chars`; both are patchable, so a deployment that widens the line
    /// widens it in both places rather than having one of them frozen in code (§0.2).
    pub max_state_chars: usize,
}

/// The drift-watch row.
pub struct DriftWatchPlugin;

#[async_trait::async_trait]
impl Plugin for DriftWatchPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DriftConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "agents", "rollups"]).union(&Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        resolve::validate(cfg)
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);

        let ledger = LedgerHandle(ctx.get::<Ledger>().map_err(fail)?.0.clone());
        let agents = (*ctx.get::<Agents>().map_err(fail)?).clone();
        let rollups = (*ctx.get::<Rollups>().map_err(fail)?).clone();

        // Model-visible ⟺ ledgered (§0.2): the reset's own step type, declared as an EFFECT so
        // unloading this row leaves the type map as if it had never mounted.
        //
        // `about/line` is declared here TOO, from the `about-line` row's own definition. `/reset`
        // appends one (§8 requires it to), and `about-line` is an ordinary bundle row a patch may
        // disable — without this, a composition with `drift-watch` and no `about-line` would boot
        // clean and fail at `/reset` with `UnknownStepTypeOnAppend`, which is the "silently skip a
        // missing referent, then fail late" shape §0.2 forbids. Identical declarations are
        // refcounted, so both rows may declare it and unloading either leaves it standing.
        ledger
            .declare_step_types(
                &ctx,
                vocabulary::step_types()
                    .into_iter()
                    .chain(bough_plugin_about_line::step_types())
                    .collect(),
            )
            .await?;

        let handle = DriftHandle(Arc::new(DriftInner {
            ctx: ctx.clone(),
            cfg,
            ledger,
            agents,
            rollups,
        }));
        ctx.provide::<Drift>(handle.clone())
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // The recorded stream this row's invariant reads is per fiber LIFE: a reload starts
        // clean, or observations from a previous instance would be judged against this one's
        // store (the `reconsolidation` precedent).
        ctx.effect(move |e| async move {
            e.defer_sync(invariant::reset);
            Ok(())
        })
        .await?;

        command::register(&ctx, &handle).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::a_reset_rebuilds_and_never_reseals()]
    }
}

bough_kernel::register_plugin!(DriftWatchPlugin);
