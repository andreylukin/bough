//! Invariant: a sweep is REF-GUARDED THEN WATERMARKED, in that order, exactly as
//! `old-feed-adapter` does it, so a restart re-sweep delivers nothing twice. Everything it
//! delivers is EVIDENCE and carries its `gh:` ref, so Phase 5's `mail-router` can route on refs
//! without this row changing.
//!
//! A `deliver_to` naming an agent that does not exist is reported EVERY sweep — a `disabled` entry
//! and a `tracing::warn!` — and never silently skipped (§0.2).

pub mod invariant;
pub mod sweep;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::AgentsHandle;
use bough_plugin_collect_core::{CollectError, SweepReport, WakeClass, WatermarkStore};
use bough_plugin_gh_cli::Gh;
use bough_plugin_ledger::LedgerHandle;
use bough_plugin_schedule::Cadence;
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "collector-github";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GithubCollectorConfig {
    pub cadence: Cadence,
    /// `"gh"`. A config field, not a constant, because the tests put a recording shim here.
    pub gh_bin: String,
    /// `"owner/repo"`.
    pub repos: Vec<String>,
    /// Which sweeps run. Each is a SOURCE with its own watermark.
    pub prs: bool,
    pub review_requests: bool,
    pub mentions: bool,
    pub checks: bool,
    /// Agent names. Phase 5's `mail-router` replaces this; the refs are cited so it can.
    pub deliver_to: Vec<String>,
    pub wake_classes: Vec<WakeClass>,
    /// `"dependabot[bot]"`, `"github-actions[bot]"`, … Feeds `gh_cli::classify`.
    pub known_bots: Vec<String>,
    pub state_db: PathBuf,
    pub batch: usize,
    pub timeout_ms: u64,
}

/// The live collector.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct GithubCollector {
    cfg: Arc<GithubCollectorConfig>,
    gh: Gh,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    state: WatermarkStore,
    last: parking_lot::Mutex<SweepReport>,
}

impl GithubCollector {
    /// Open the watermark store and build the `gh` invoker. WP-2.
    pub fn open(
        cfg: Arc<GithubCollectorConfig>,
        ledger: LedgerHandle,
        agents: AgentsHandle,
    ) -> Result<GithubCollector, CollectError> {
        let _ = (cfg, ledger, agents);
        todo!("WP-2")
    }

    /// One sweep with its clock injected (AGENTS.md: `now` is passed in). WP-2.
    pub async fn sweep_at(&self, now: DateTime<Utc>) -> Result<SweepReport, CollectError> {
        let _ = now;
        todo!("WP-2: per source — read a bounded batch, ref-guard, deliver, THEN watermark")
    }

    /// What the last sweep did. WP-2.
    pub fn status(&self) -> SweepReport {
        todo!("WP-2")
    }
}

/// The row.
pub struct GithubCollectorPlugin;

#[async_trait::async_trait]
impl Plugin for GithubCollectorPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = GithubCollectorConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["schedule", "agents", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-2: `cadence.check()`, non-empty `deliver_to`, `batch > 0`, well-formed `owner/repo`")
    }

    /// Build the handle and register ONE `JobSpec { name: \"collector-github\", catch_up: true }`
    /// on `ctx.schedule` as an effect. Disabling the row unloads the fiber, which unwinds the
    /// registration, which removes the job — the SWAP bullet, with no code of its own. WP-2.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-2")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(GithubCollectorPlugin);
