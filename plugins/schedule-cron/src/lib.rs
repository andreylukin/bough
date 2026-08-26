//! Invariant: this Provider is the ONLY thing in the tree that reads a wall clock on a job's
//! behalf. A job is handed `at` and `scheduled_for`; a run that outlives `job_timeout_ms` is
//! ABANDONED and recorded [`JobOutcome::Failed`], never left to hold the tick; and a job that
//! panics is recorded `Failed` while the scheduler keeps ticking.
//!
//! `catch_up: true` is honoured across a restart because the last run of every job is persisted in
//! this row's OWN sqlite file (`state_db`), not in the ledger: a fire is not model-visible (§0.2).

pub mod invariant;
pub mod state;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use bough_kernel::{ConfigError, Context, EffectHandle, InvariantSpec, Plugin, PluginError};
use bough_plugin_schedule::{
    Cadence, FireReason, Job, JobFire, JobInfo, JobName, JobOutcome, JobRun, JobSpec, Schedule,
    ScheduleError, ScheduleFired, ScheduleHandle, Scheduler,
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio_cron_scheduler::{Job as CronJob, JobScheduler};
use uuid::Uuid;

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
}

/// The scheduler's tick, in ms. NOT a config field: `tokio-cron-scheduler` 0.15 sleeps a
/// hardcoded 500ms between ticks (`scheduler.rs`) and exposes no setter, so a `tick_ms` a
/// deployment could set would be a number the scheduler does not honour — and the floor check
/// below, which is the whole reason the tick is named anywhere, would be calibrated against
/// fiction. It is a PROTOCOL CONSTANT of the library this Provider is built on, and it moves
/// only when the dependency does.
pub const SCHEDULER_TICK_MS: u64 = 500;

/// The live scheduler: the tokio-cron-scheduler instance, the job table and the state store.
pub struct CronScheduler {
    cfg: Arc<CronConfig>,
    /// The row's own context: the one thing that may emit `schedule/fired`.
    ctx: Context,
    state: state::RunStore,
    jobs: Mutex<bough_plugin_schedule::JobTable>,
    /// The tokio-cron-scheduler id of each registered job, so a disposer removes EXACTLY its own.
    uuids: Mutex<BTreeMap<JobName, Uuid>>,
    sched: JobScheduler,
    /// A self-reference for the tick callbacks. Weak, so the callbacks never keep the row alive.
    me: Mutex<Weak<CronScheduler>>,
}

impl CronScheduler {
    /// Open the state store and start the tokio-cron-scheduler tick.
    ///
    /// `ctx` is the DEVIATION from the plan's `start(cfg)`: a fire emits `schedule/fired`, and an
    /// emit needs the row's context (see the work-package report).
    pub async fn start(
        cfg: Arc<CronConfig>,
        ctx: Context,
    ) -> Result<Arc<CronScheduler>, ScheduleError> {
        let state = state::RunStore::open(&cfg.state_db)?;
        let sched = JobScheduler::new()
            .await
            .map_err(|e| ScheduleError::State(e.to_string()))?;
        sched
            .start()
            .await
            .map_err(|e| ScheduleError::State(e.to_string()))?;
        let me = Arc::new(CronScheduler {
            cfg,
            ctx,
            state,
            jobs: Mutex::new(Vec::new()),
            uuids: Mutex::new(BTreeMap::new()),
            sched,
            me: Mutex::new(Weak::new()),
        });
        *me.me.lock() = Arc::downgrade(&me);
        Ok(me)
    }

    /// Every run this row has persisted. The invariant reads it.
    pub fn stored(&self) -> Result<BTreeMap<String, JobRun>, ScheduleError> {
        self.state.all()
    }

    /// Shut the tick down. Called from the row's disposer.
    pub async fn stop(&self) {
        let mut sched = self.sched.clone();
        if let Err(e) = sched.shutdown().await {
            tracing::warn!(target: "schedule-cron", error = %e, "the scheduler did not shut down cleanly");
        }
    }

    /// Run one job under `job_timeout_ms`, catching a panic, and record the outcome. PURE of the
    /// clock: `fire` carries it.
    ///
    /// The run is a TASK, which is what makes both bounds real: a panic comes back as a
    /// `JoinError` instead of unwinding the tick, and a timeout ABANDONS the task (`abort`)
    /// instead of waiting for a body that has hung.
    pub async fn run_one(&self, job: Arc<dyn Job>, fire: JobFire) -> JobRun {
        let (name, at, reason) = (fire.name.clone(), fire.at, fire.reason);
        let timeout = std::time::Duration::from_millis(self.cfg.job_timeout_ms);
        let mut task = tokio::spawn(async move { job.run(fire).await });
        let outcome = match tokio::time::timeout(timeout, &mut task).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) if e.is_panic() => JobOutcome::Failed {
                error: format!("`{name}` panicked; the scheduler keeps ticking"),
            },
            Ok(Err(e)) => JobOutcome::Failed {
                error: e.to_string(),
            },
            Err(_) => {
                task.abort();
                JobOutcome::Failed {
                    error: format!(
                        "`{name}` outlived job_timeout_ms ({}ms) and was abandoned",
                        self.cfg.job_timeout_ms
                    ),
                }
            }
        };
        self.record(
            &name,
            JobRun {
                at,
                reason,
                outcome,
            },
        )
    }

    /// The one place a completed run is written down: the row's own store, the listing row, and
    /// exactly one `schedule/fired`.
    fn record(&self, name: &JobName, mut run: JobRun) -> JobRun {
        // MILLISECONDS, once, here: the store keeps `at` as ms, and a listing row that kept
        // microseconds would make the invariant ("the listing's `last` IS the stored row") fail
        // on a truncation rather than on a lost write.
        run.at = DateTime::from_timestamp_millis(run.at.timestamp_millis()).unwrap_or(run.at);
        if let Err(e) = self.state.set(name, &run) {
            tracing::warn!(target: "schedule-cron", job = %name, error = %e, "could not persist a job run");
        }
        {
            let mut jobs = self.jobs.lock();
            if let Some(row) = jobs.iter_mut().find(|(n, _, _)| n == name) {
                row.1.last = Some(run.clone());
            }
        }
        self.ctx.emit::<ScheduleFired>(run.clone());
        run
    }

    /// One fire, start to finish: look the body up, run it, record it. EXACTLY ONE `JobRun` and
    /// one emit per call.
    async fn fire(
        &self,
        name: &JobName,
        at: DateTime<Utc>,
        reason: FireReason,
    ) -> Result<JobRun, ScheduleError> {
        let (job, scheduled_for) = {
            let jobs = self.jobs.lock();
            let row = jobs
                .iter()
                .find(|(n, _, _)| n == name)
                .ok_or_else(|| ScheduleError::Unknown(name.clone()))?;
            (row.2.clone(), row.1.next.unwrap_or(at))
        };
        Ok(self
            .run_one(
                job,
                JobFire {
                    name: name.clone(),
                    at,
                    scheduled_for,
                    reason,
                },
            )
            .await)
    }
}

#[async_trait::async_trait]
impl Scheduler for CronScheduler {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }

    async fn register(&self, ctx: &Context, spec: JobSpec) -> Result<EffectHandle, PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: ScheduleError| PluginError::new(entry.clone(), e);
        spec.cadence.check().map_err(fail)?;
        if let Cadence::Every { every_ms } = &spec.cadence {
            if *every_ms < SCHEDULER_TICK_MS {
                return Err(fail(ScheduleError::BadCadence(format!(
                    "`{}` asks for every {every_ms}ms but the scheduler ticks every \
                     {SCHEDULER_TICK_MS}ms: a cadence finer than the tick cannot be honoured",
                    spec.name
                ))));
            }
        }

        let now = Utc::now();
        let last = self.state.get(&spec.name).map_err(fail)?;
        {
            let mut jobs = self.jobs.lock();
            if jobs.iter().any(|(n, _, _)| *n == spec.name) {
                return Err(fail(ScheduleError::Duplicate(spec.name.clone())));
            }
            jobs.push((
                spec.name.clone(),
                JobInfo {
                    name: spec.name.clone(),
                    cadence: spec.cadence.clone(),
                    owner: entry.clone(),
                    next: spec.cadence.next_after(now),
                    last: last.clone(),
                },
                spec.job.clone(),
            ));
        }

        // CATCH-UP FIRST, before the cadence is armed: a job whose stored last run is older than
        // one cadence has missed a fire, and the whole point of `catch_up` is that the miss is
        // made good at activation rather than at the next tick.
        if let Some((at, FireReason::CatchUp)) = next_fire(&spec, last.map(|r| r.at), now) {
            let _ = self.fire(&spec.name, at, FireReason::CatchUp).await;
        }

        // The tick callback holds a WEAK self-reference and the name only: the body itself is
        // read from the table at fire time, so a disposed job's callback finds nothing and does
        // nothing.
        let weak = self.me.lock().clone();
        let name = spec.name.clone();
        let make = move || {
            let weak = weak.clone();
            let name = name.clone();
            move |_uuid: Uuid, _l: JobScheduler| {
                let weak = weak.clone();
                let name = name.clone();
                Box::pin(async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.fire(&name, Utc::now(), FireReason::Cadence).await;
                    }
                })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            }
        };
        let cron_job = match &spec.cadence {
            Cadence::Cron { cron } => CronJob::new_async(cron.as_str(), make()),
            Cadence::Every { every_ms } => {
                CronJob::new_repeated_async(std::time::Duration::from_millis(*every_ms), make())
            }
        }
        .map_err(|e| fail(ScheduleError::BadCadence(e.to_string())))?;
        let uuid = self
            .sched
            .add(cron_job)
            .await
            .map_err(|e| fail(ScheduleError::State(e.to_string())))?;
        self.uuids.lock().insert(spec.name.clone(), uuid);

        // THE DISPOSER: exactly this job leaves, and the others stay (SWAP).
        let weak = self.me.lock().clone();
        let name = spec.name.clone();
        ctx.effect(move |e| async move {
            e.defer(move || {
                let weak = weak.clone();
                let name = name.clone();
                async move {
                    let Some(me) = weak.upgrade() else { return };
                    me.jobs.lock().retain(|(n, _, _)| *n != name);
                    let uuid = me.uuids.lock().remove(&name);
                    if let Some(uuid) = uuid {
                        let _ = me.sched.remove(&uuid).await;
                    }
                }
            });
            Ok(())
        })
        .await
    }

    fn jobs(&self) -> Vec<JobInfo> {
        let now = Utc::now();
        let mut out: Vec<JobInfo> = self
            .jobs
            .lock()
            .iter()
            .map(|(_, info, _)| {
                let mut info = info.clone();
                info.next = info.cadence.next_after(now);
                info
            })
            .collect();
        out.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        out
    }

    async fn fire_now(&self, name: &JobName) -> Result<JobRun, ScheduleError> {
        self.fire(name, Utc::now(), FireReason::Manual).await
    }
}

/// The Provider row.
pub struct SchedulerCronPlugin;

#[async_trait::async_trait]
impl Plugin for SchedulerCronPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CronConfig;

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.job_timeout_ms == 0 {
            return Err(ConfigError::Rejected {
                detail: "`job_timeout_ms` is 0: every run would be abandoned before it started"
                    .to_string(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let sched = CronScheduler::start(cfg, ctx.clone())
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let fiber = ctx.fiber_uid();
        crate::invariant::publish(fiber, &sched);
        let stopping = sched.clone();
        ctx.effect(move |e| async move {
            e.defer(move || {
                let stopping = stopping.clone();
                async move {
                    crate::invariant::withdraw(fiber);
                    stopping.stop().await
                }
            });
            Ok(())
        })
        .await?;
        ctx.provide::<Schedule>(ScheduleHandle(sched as Arc<dyn Scheduler>))
            .await
            .map_err(|e| PluginError::new(entry, e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SchedulerCronPlugin);

/// The next fire of a job, given its stored last run. PURE, so `catch_up` is testable without a
/// clock or a database.
pub fn next_fire(
    spec: &JobSpec,
    last: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<(DateTime<Utc>, FireReason)> {
    if spec.catch_up {
        // A job that has never run has, by definition, missed every fire there was.
        let missed = match last {
            None => true,
            Some(last) => spec
                .cadence
                .next_after(last)
                .map(|due| due <= now)
                .unwrap_or(false),
        };
        if missed {
            return Some((now, FireReason::CatchUp));
        }
    }
    spec.cadence
        .next_after(now)
        .map(|at| (at, FireReason::Cadence))
}
