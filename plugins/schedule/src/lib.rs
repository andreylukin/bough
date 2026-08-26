//! Invariant: this crate is the schedule SERVICE DEFINITION (§9, §0.2). It owns the `schedule`
//! key, the cadence vocabulary, the job contract and the one observability event — and NOT ONE
//! LINE of timing. Every fire comes from a Provider (`schedule-cron` in production,
//! `schedule-manual` in tests), and every registration is an EFFECT, so a row that registers a job
//! takes the job with it when it unloads.
//!
//! A job never reads a clock: `JobFire` carries `at` and `scheduled_for` (AGENTS.md).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{
    ConfigError, Context, EffectHandle, EmitEvent, EntryId, InvariantSpec, Plugin, PluginError,
    ServiceKey,
};
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "schedule";

/// The `schedule` service key.
pub struct Schedule;

impl ServiceKey for Schedule {
    type Value = ScheduleHandle;
    const NAME: &'static str = "schedule";
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct ScheduleHandle(pub Arc<dyn Scheduler>);

bough_util::brand_id!(
    /// A job's name; unique per tree.
    pub struct JobName;
);

/// How often a job fires. Exactly one of the two spellings; the config shape is `{ cron: "…" }`
/// or `{ every_ms: 300000 }`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum Cadence {
    /// A 6-field (sec min hour dom mon dow) tokio-cron-scheduler expression.
    Cron { cron: String },
    /// A fixed interval in milliseconds. Zero is refused.
    Every { every_ms: u64 },
}

impl Cadence {
    /// PURE: rejects a malformed cron string and a zero interval. Called from `Plugin::validate`,
    /// so a bad cadence is a BOOT failure and never a silent job that never fires (§0.2).
    ///
    /// WP-1.
    pub fn check(&self) -> Result<(), ScheduleError> {
        todo!("WP-1: parse `cron` through the `cron` crate; refuse `every_ms == 0`")
    }

    /// PURE: the next fire at or after `from`. `None` when the cadence can never fire again.
    ///
    /// WP-1.
    pub fn next_after(&self, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let _ = from;
        todo!("WP-1: cron schedule `after`, or `from + every_ms`")
    }
}

/// One registration: a name, a cadence, a catch-up policy and the body.
#[derive(Clone)]
pub struct JobSpec {
    pub name: JobName,
    pub cadence: Cadence,
    /// A job whose last recorded run is older than one cadence fires ONCE at activation
    /// ([`FireReason::CatchUp`]) before its ordinary schedule resumes.
    pub catch_up: bool,
    pub job: Arc<dyn Job>,
}

/// What a scheduled job does. One call per fire; the Provider bounds it by `job_timeout_ms`.
#[async_trait::async_trait]
pub trait Job: Send + Sync + 'static {
    async fn run(&self, fire: JobFire) -> JobOutcome;
}

/// One firing, as the job sees it. The clock is PASSED IN.
#[derive(Clone, Debug, PartialEq)]
pub struct JobFire {
    pub name: JobName,
    /// When the Provider actually fired.
    pub at: DateTime<Utc>,
    /// When it was due.
    pub scheduled_for: DateTime<Utc>,
    pub reason: FireReason,
}

/// Why a fire happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FireReason {
    /// The ordinary cadence came round.
    Cadence,
    /// The stored last-run was older than one cadence at activation.
    CatchUp,
    /// [`Scheduler::fire_now`].
    Manual,
}

/// What a run did.
///
/// `Pending` is NOT a failure: the job could not act because a referent it needs is not in this
/// tree yet, it says so, and it is tried again next cadence (P6-D2).
#[derive(Clone, Debug, PartialEq)]
pub enum JobOutcome {
    Ran { detail: String },
    Pending { reason: String },
    Failed { error: String },
}

/// One row of [`Scheduler::jobs`].
#[derive(Clone, Debug, PartialEq)]
pub struct JobInfo {
    pub name: JobName,
    pub cadence: Cadence,
    /// The row that registered it. `jobs()` is how the SWAP test sees a job leave with its row.
    pub owner: EntryId,
    pub next: Option<DateTime<Utc>>,
    pub last: Option<JobRun>,
}

/// One completed run.
#[derive(Clone, Debug, PartialEq)]
pub struct JobRun {
    pub at: DateTime<Utc>,
    pub reason: FireReason,
    pub outcome: JobOutcome,
}

/// The job table a Provider keeps: name, listing row, body. Named so both Providers spell it the
/// same way and neither grows a nest of tuples in its own file.
pub type JobTable = Vec<(JobName, JobInfo, std::sync::Arc<dyn Job>)>;

/// What a schedule Provider does.
#[async_trait::async_trait]
pub trait Scheduler: Send + Sync + 'static {
    /// Catalog name of the plugin behind this binding; the swap test reads it.
    fn provider(&self) -> &'static str;

    /// Registration is an EFFECT: the returned disposer removes exactly this job, so a collector
    /// row unloading takes its schedule registration with it (SWAP).
    async fn register(&self, ctx: &Context, spec: JobSpec) -> Result<EffectHandle, PluginError>;

    /// Every registered job, sorted by name.
    fn jobs(&self) -> Vec<JobInfo>;

    /// Fire now, out of band. Used by tests, by `bough` subcommands, and by a ward's `schedule`
    /// action whose delay has already elapsed.
    async fn fire_now(&self, name: &JobName) -> Result<JobRun, ScheduleError>;
}

/// What the schedule seam refuses.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("no job named `{0}`")]
    Unknown(JobName),
    #[error("a job named `{0}` is already registered")]
    Duplicate(JobName),
    #[error("bad cadence: {0}")]
    BadCadence(String),
    #[error("schedule state: {0}")]
    State(String),
}

/// `schedule/fired` — EMIT (observe-only). Surfaces and the invariant read it; nothing durable
/// rides it (P2-D25). A job firing is not model-visible, so it is NOT a step type (§0.2).
pub struct ScheduleFired;

impl EmitEvent for ScheduleFired {
    const NAME: &'static str = "schedule/fired";
    type Payload = JobRun;
}

/// No configuration: the cadences belong to the rows that register jobs, not to the seam.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {}

/// The Service Definition row.
pub struct SchedulePlugin;

#[async_trait::async_trait]
impl Plugin for SchedulePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ScheduleConfig;

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    /// WP-1: the Definition row holds no live state of its own; a Provider provides the key.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: declare the seam's vocabulary; the Providers provide `schedule`")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SchedulePlugin);
