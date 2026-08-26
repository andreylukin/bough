//! Invariant: this Provider NEVER fires on its own. It has no timer, no tick and no clock; a job
//! runs only through [`Scheduler::fire_now`] or [`ManualScheduler::fire_at`], and `jobs()` reports
//! `next: None` to say so. That is what makes every collector, ward and system-schedule test
//! hermetic (P6-D1).
//!
//! It also keeps no last-run store: `catch_up` is a property of a clock this Provider does not
//! have, so a job registered here is never caught up — a test that wants a catch-up fire names it,
//! `fire_at(name, at, FireReason::CatchUp)`.
//!
//! In the catalog, in NO bundle (the `ledger-memory` precedent).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError};
use bough_plugin_schedule::{
    FireReason, Job, JobFire, JobInfo, JobName, JobOutcome, JobRun, JobSpec, Schedule,
    ScheduleError, ScheduleFired, ScheduleHandle, Scheduler,
};
use parking_lot::Mutex;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "schedule-manual";

/// No configuration: a scheduler that never fires has nothing to tune.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualConfig {}

/// The manual scheduler: a job table and nothing else.
#[derive(Default)]
pub struct ManualScheduler {
    jobs: Mutex<bough_plugin_schedule::JobTable>,
    /// The row's context, when this scheduler belongs to a mounted row. A bare `new()` (a unit
    /// test holding the handle directly) has none and therefore announces nothing.
    ctx: Mutex<Option<Context>>,
    /// A self-reference for the disposers. Weak, so a disposer never keeps the scheduler alive.
    weak: Mutex<std::sync::Weak<ManualScheduler>>,
}

impl ManualScheduler {
    /// An empty scheduler with no context: it records runs and announces nothing.
    pub fn new() -> Arc<ManualScheduler> {
        let me = Arc::new(ManualScheduler::default());
        *me.weak.lock() = Arc::downgrade(&me);
        me
    }

    /// An empty scheduler that announces `schedule/fired` through `ctx`.
    pub fn new_in(ctx: Context) -> Arc<ManualScheduler> {
        let me = ManualScheduler::new();
        *me.ctx.lock() = Some(ctx);
        me
    }

    /// Fire a job at an EXPLICIT instant, so a test names its own clock.
    pub async fn fire_at(
        &self,
        name: &JobName,
        at: chrono::DateTime<chrono::Utc>,
        reason: FireReason,
    ) -> Result<JobRun, ScheduleError> {
        let job = {
            let jobs = self.jobs.lock();
            jobs.iter()
                .find(|(n, _, _)| n == name)
                .map(|(_, _, job)| job.clone())
                .ok_or_else(|| ScheduleError::Unknown(name.clone()))?
        };
        Ok(self
            .run_one(
                job,
                JobFire {
                    name: name.clone(),
                    at,
                    scheduled_for: at,
                    reason,
                },
            )
            .await)
    }

    /// Run one body and record it: exactly one `JobRun` in the listing and one emit.
    ///
    /// A panicking job is `Failed` here too — a test asserting the seam's behaviour must not have
    /// to swap Providers to see it.
    async fn run_one(&self, job: Arc<dyn Job>, fire: JobFire) -> JobRun {
        let (name, at, reason) = (fire.name.clone(), fire.at, fire.reason);
        let outcome = match tokio::spawn(async move { job.run(fire).await }).await {
            Ok(outcome) => outcome,
            Err(e) if e.is_panic() => JobOutcome::Failed {
                error: format!("`{name}` panicked; the scheduler keeps ticking"),
            },
            Err(e) => JobOutcome::Failed {
                error: e.to_string(),
            },
        };
        let run = JobRun {
            at,
            reason,
            outcome,
        };
        {
            let mut jobs = self.jobs.lock();
            if let Some(row) = jobs.iter_mut().find(|(n, _, _)| *n == name) {
                row.1.last = Some(run.clone());
            }
        }
        if let Some(ctx) = self.ctx.lock().as_ref() {
            ctx.emit::<ScheduleFired>(run.clone());
        }
        run
    }
}

#[async_trait::async_trait]
impl Scheduler for ManualScheduler {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }

    async fn register(&self, ctx: &Context, spec: JobSpec) -> Result<EffectHandle, PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: ScheduleError| PluginError::new(entry.clone(), e);
        // The SAME refusals as the production Provider: a test Provider that accepts what
        // `schedule-cron` refuses would make its tests a lie.
        spec.cadence.check().map_err(fail)?;
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
                    // NEVER a next: this Provider has no clock.
                    next: None,
                    last: None,
                },
                spec.job.clone(),
            ));
        }
        let me = self.self_ref();
        let name = spec.name.clone();
        ctx.effect(move |e| async move {
            e.defer_sync(move || {
                if let Some(me) = me.upgrade() {
                    me.jobs.lock().retain(|(n, _, _)| *n != name);
                }
            });
            Ok(())
        })
        .await
    }

    fn jobs(&self) -> Vec<JobInfo> {
        let mut out: Vec<JobInfo> = self.jobs.lock().iter().map(|(_, i, _)| i.clone()).collect();
        out.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        out
    }

    async fn fire_now(&self, name: &JobName) -> Result<JobRun, ScheduleError> {
        self.fire_at(name, chrono::Utc::now(), FireReason::Manual)
            .await
    }
}

impl ManualScheduler {
    /// The disposer needs a handle to the table without keeping the scheduler alive. The table is
    /// an `Arc` field of its own for exactly that reason.
    fn self_ref(&self) -> std::sync::Weak<ManualScheduler> {
        self.weak.lock().clone()
    }
}

/// The test Provider row.
pub struct ScheduleManualPlugin;

#[async_trait::async_trait]
impl Plugin for ScheduleManualPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ManualConfig;

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let sched = ManualScheduler::new_in(ctx.clone());
        ctx.provide::<Schedule>(ScheduleHandle(sched as Arc<dyn Scheduler>))
            .await
            .map_err(|e| PluginError::new(entry, e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ScheduleManualPlugin);
