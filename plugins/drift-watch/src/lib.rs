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
use bough_plugin_ledger::{AgentName, RollupId, SeqRange, StepId, TrajId};
use bough_plugin_rollups::Attribution;
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
    pub async fn signals(
        &self,
        _agent: &AgentName,
        _at: DateTime<Utc>,
    ) -> Result<Signals, DriftError> {
        todo!("WP-4: compute the signals")
    }

    /// §8's one-command reset.
    pub async fn reset(&self, _req: &ResetRequest) -> Result<ResetReport, DriftError> {
        todo!("WP-4: rebuild identity from raw evidence")
    }
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

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4: provide `drift`, declare `drift/reset`, register /drift /reset /supersede")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::a_reset_rebuilds_and_never_reseals()]
    }
}

bough_kernel::register_plugin!(DriftWatchPlugin);
