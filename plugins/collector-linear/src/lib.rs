//! Invariant: the API KEY NEVER APPEARS anywhere a human or a log can read it. `Debug`, the sweep
//! report, every error string and `--dump-config` render it as `<redacted>`; the row records only
//! that it resolved (P6-D7). A MISSING key disables the row's sources LOUDLY — a `disabled` entry
//! every sweep — and does not fail the boot: a machine without a Linear key must still boot.
//!
//! The sweep order is the same as `collector-github`'s and for the same reason: ref-guard, deliver,
//! then watermark.

pub mod graphql;
pub mod invariant;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::AgentsHandle;
use bough_plugin_collect_core::{CollectError, SweepReport, WakeClass, WatermarkStore};
use bough_plugin_ledger::LedgerHandle;
use bough_plugin_schedule::Cadence;
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "collector-linear";

/// What a redacted secret renders as, everywhere, in one place.
pub const REDACTED: &str = "<redacted>";

/// The row's config.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearCollectorConfig {
    pub cadence: Cadence,
    /// The GraphQL endpoint. A config field, not a constant, because the test stub is a local URL.
    pub endpoint: String,
    /// `!!expr 'env("LINEAR_API_KEY")'`. NEVER logged, never in an error, never in `--dump-config`.
    pub api_key: String,
    /// `"TEAM"`.
    pub teams: Vec<String>,
    pub projects: Vec<String>,
    pub deliver_to: Vec<String>,
    pub wake_classes: Vec<WakeClass>,
    pub state_db: PathBuf,
    pub batch: usize,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for LinearCollectorConfig {
    /// The redaction, at the type, so no call site has to remember it (P6-D7). WP-2.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f;
        todo!("WP-2: every field, with `api_key` rendered as REDACTED")
    }
}

/// The live collector.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct LinearCollector {
    cfg: Arc<LinearCollectorConfig>,
    http: reqwest::Client,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    state: WatermarkStore,
    last: parking_lot::Mutex<SweepReport>,
}

impl LinearCollector {
    /// Open the watermark store and build the HTTP client. WP-2.
    pub fn open(
        cfg: Arc<LinearCollectorConfig>,
        ledger: LedgerHandle,
        agents: AgentsHandle,
    ) -> Result<LinearCollector, CollectError> {
        let _ = (cfg, ledger, agents);
        todo!("WP-2")
    }

    /// One sweep with its clock injected. WP-2.
    pub async fn sweep_at(&self, now: DateTime<Utc>) -> Result<SweepReport, CollectError> {
        let _ = now;
        todo!("WP-2")
    }

    /// What the last sweep did. WP-2.
    pub fn status(&self) -> SweepReport {
        todo!("WP-2")
    }
}

/// The row.
pub struct LinearCollectorPlugin;

#[async_trait::async_trait]
impl Plugin for LinearCollectorPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LinearCollectorConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["schedule", "agents", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!(
            "WP-2: `cadence.check()`, a parseable `endpoint`, non-empty `deliver_to`, `batch > 0`"
        )
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-2: register ONE job on ctx.schedule as an effect")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(LinearCollectorPlugin);
