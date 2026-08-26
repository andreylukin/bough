//! Invariant: this Provider NEVER fires on its own. It has no timer, no tick and no clock; a job
//! runs only through [`Scheduler::fire_now`], and `jobs()` reports `next: None` to say so. That is
//! what makes every collector, ward and system-schedule test hermetic (P6-D1).
//!
//! In the catalog, in NO bundle (the `ledger-memory` precedent).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError};
use bough_plugin_schedule::{JobInfo, JobName, JobRun, JobSpec, ScheduleError, Scheduler};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "schedule-manual";

/// No configuration: a scheduler that never fires has nothing to tune.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualConfig {}

/// The manual scheduler: a job table and nothing else.
#[derive(Default)]
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct ManualScheduler {
    jobs: parking_lot::Mutex<bough_plugin_schedule::JobTable>,
}

impl ManualScheduler {
    /// An empty scheduler. WP-1.
    pub fn new() -> Arc<ManualScheduler> {
        Arc::new(ManualScheduler::default())
    }

    /// Fire a job at an EXPLICIT instant, so a test names its own clock. WP-1.
    pub async fn fire_at(
        &self,
        name: &JobName,
        at: chrono::DateTime<chrono::Utc>,
        reason: bough_plugin_schedule::FireReason,
    ) -> Result<JobRun, ScheduleError> {
        let _ = (name, at, reason);
        todo!("WP-1")
    }
}

#[async_trait::async_trait]
impl Scheduler for ManualScheduler {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }

    async fn register(&self, ctx: &Context, spec: JobSpec) -> Result<EffectHandle, PluginError> {
        let _ = (ctx, spec);
        todo!("WP-1: same duplicate refusal and same disposer as the cron Provider, no timer")
    }

    fn jobs(&self) -> Vec<JobInfo> {
        todo!("WP-1: `next: None` for every row")
    }

    async fn fire_now(&self, name: &JobName) -> Result<JobRun, ScheduleError> {
        let _ = name;
        todo!("WP-1")
    }
}

/// The test Provider row.
pub struct ScheduleManualPlugin;

#[async_trait::async_trait]
impl Plugin for ScheduleManualPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ManualConfig;

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: provide `schedule` with a ManualScheduler")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ScheduleManualPlugin);
