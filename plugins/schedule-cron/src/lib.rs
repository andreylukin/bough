//! Invariant: this Provider is the ONLY thing in the tree that reads a wall clock on a job's
//! behalf. A job is handed `at` and `scheduled_for`; a run that outlives `job_timeout_ms` is
//! ABANDONED and recorded [`JobOutcome::Failed`], never left to hold the tick; and a job that
//! panics is recorded `Failed` while the scheduler keeps ticking.
//!
//! `catch_up: true` is honoured across a restart because the last run of every job is persisted in
//! this row's OWN sqlite file (`state_db`), not in the ledger: a fire is not model-visible (§0.2).

pub mod invariant;
pub mod state;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, EffectHandle, InvariantSpec, Plugin, PluginError};
use bough_plugin_schedule::{
    Job, JobFire, JobInfo, JobName, JobRun, JobSpec, ScheduleError, Scheduler,
};
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "schedule-cron";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CronConfig {
    /// Where last-run times live, so `catch_up: true` survives a restart.
    pub state_db: PathBuf,
    /// How long a single job run may take before it is abandoned and recorded `Failed`.
    pub job_timeout_ms: u64,
    /// tokio-cron-scheduler's tick. Deployment-varying, so it is config (§0.2).
    pub tick_ms: u64,
}

/// The live scheduler: the tokio-cron-scheduler instance, the job table and the state store.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct CronScheduler {
    cfg: Arc<CronConfig>,
    state: state::RunStore,
    jobs: parking_lot::Mutex<bough_plugin_schedule::JobTable>,
}

impl CronScheduler {
    /// Open the state store and start the tokio-cron-scheduler tick. WP-1.
    pub async fn start(cfg: Arc<CronConfig>) -> Result<Arc<CronScheduler>, ScheduleError> {
        let _ = cfg;
        todo!("WP-1: open `state_db`, build the JobScheduler with `tick_ms`, start it")
    }

    /// Run one job under `job_timeout_ms`, catching a panic, and record the outcome. PURE of the
    /// clock: `fire` carries it. WP-1.
    pub async fn run_one(&self, job: Arc<dyn Job>, fire: JobFire) -> JobRun {
        let _ = (job, fire);
        todo!("WP-1: timeout + catch_unwind + persist the last run")
    }
}

#[async_trait::async_trait]
impl Scheduler for CronScheduler {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }

    async fn register(&self, ctx: &Context, spec: JobSpec) -> Result<EffectHandle, PluginError> {
        let _ = (ctx, spec);
        todo!("WP-1: refuse a duplicate name, schedule the cadence, fire once for CatchUp, defer removal")
    }

    fn jobs(&self) -> Vec<JobInfo> {
        todo!("WP-1: the job table, sorted by name")
    }

    async fn fire_now(&self, name: &JobName) -> Result<JobRun, ScheduleError> {
        let _ = name;
        todo!("WP-1: FireReason::Manual")
    }
}

/// The Provider row.
pub struct SchedulerCronPlugin;

#[async_trait::async_trait]
impl Plugin for SchedulerCronPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CronConfig;

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-1: refuse a zero `tick_ms` / `job_timeout_ms`")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: start the scheduler, provide `schedule`, defer shutdown")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SchedulerCronPlugin);

/// The next fire of a job, given its stored last run. PURE, so `catch_up` is testable without a
/// clock or a database. WP-1.
pub fn next_fire(
    spec: &JobSpec,
    last: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<(DateTime<Utc>, bough_plugin_schedule::FireReason)> {
    let _ = (spec, last, now);
    todo!("WP-1: CatchUp when `last` is older than one cadence, else the next cadence")
}
