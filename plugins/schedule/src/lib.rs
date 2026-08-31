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
        match self {
            Cadence::Cron { cron } => match cron.parse::<cron::Schedule>() {
                Ok(_) => Ok(()),
                Err(e) => Err(ScheduleError::BadCadence(format!("`{cron}`: {e}"))),
            },
            Cadence::Every { every_ms } => {
                if *every_ms == 0 {
                    Err(ScheduleError::BadCadence(
                        "`every_ms` is 0: a job that fires with no interval would spin".into(),
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// PURE: the next fire at or after `from`. `None` when the cadence can never fire again.
    ///
    /// WP-1.
    pub fn next_after(&self, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Cadence::Cron { cron } => cron.parse::<cron::Schedule>().ok()?.after(&from).next(),
            Cadence::Every { every_ms } => {
                if *every_ms == 0 {
                    return None;
                }
                from.checked_add_signed(chrono::Duration::milliseconds(*every_ms as i64))
            }
        }
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
    /// A short-backoff retry of a run that came back `Pending` — a missing referent at boot
    /// (a command not registered yet, an agent not up yet) must not wait a whole cadence to be
    /// tried again (drivability, 2026-08-31: the nightly pass raced the command registry at
    /// catch-up and then waited for a 04:00 cron the laptop never sees).
    Retry,
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

    /// The Definition row holds no live state of its own: a Provider provides the key. What it
    /// DOES own is the observation the seam invariant reads — every `schedule/fired` this tree
    /// dispatches, recorded per fiber life so a reload forgets its own past (the `ledger-memory`
    /// precedent).
    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || crate::invariant::forget(mine));
            Ok(())
        })
        .await?;
        ctx.on::<ScheduleFired, _, _>(move |run| async move {
            crate::invariant::record(mine, run);
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SchedulePlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0)
            .single()
            .expect("a fixed instant")
    }

    #[test]
    fn check_rejects_a_malformed_cron() {
        let bad = Cadence::Cron {
            cron: "not a cron".into(),
        };
        let err = bad
            .check()
            .expect_err("a malformed cron is refused at validate");
        assert!(matches!(err, ScheduleError::BadCadence(_)), "{err}");
    }

    #[test]
    fn check_rejects_a_zero_interval() {
        let err = Cadence::Every { every_ms: 0 }
            .check()
            .expect_err("a zero interval would spin");
        assert!(err.to_string().contains("`every_ms` is 0"), "{err}");
    }

    #[test]
    fn check_accepts_a_six_field_cron_and_a_positive_interval() {
        Cadence::Cron {
            cron: "0 */5 * * * *".into(),
        }
        .check()
        .expect("a 6-field expression");
        Cadence::Every { every_ms: 1 }
            .check()
            .expect("one millisecond");
    }

    #[test]
    fn next_after_is_pure_and_monotone() {
        for cadence in [
            Cadence::Cron {
                cron: "0 */5 * * * *".into(),
            },
            Cadence::Every { every_ms: 300_000 },
        ] {
            let from = at(0);
            let once = cadence.next_after(from).expect("a next fire");
            // PURE: the same input twice is the same answer.
            assert_eq!(once, cadence.next_after(from).expect("a next fire"));
            // MONOTONE: strictly after `from`, and never earlier for a later `from`.
            assert!(once > from, "{once} is not after {from}");
            let later = cadence.next_after(at(600)).expect("a next fire");
            assert!(later >= once, "{later} went backwards from {once}");
        }
    }

    #[test]
    fn next_after_on_a_cadence_that_can_never_fire_is_none() {
        assert_eq!(Cadence::Every { every_ms: 0 }.next_after(at(0)), None);
        assert_eq!(
            Cadence::Cron {
                cron: "nonsense".into()
            }
            .next_after(at(0)),
            None
        );
    }

    #[test]
    fn a_cadence_round_trips_through_its_untagged_config_shape() {
        let cron: Cadence =
            serde_yaml::from_str("{ cron: \"0 0 * * * *\" }").expect("the cron shape");
        assert_eq!(
            cron,
            Cadence::Cron {
                cron: "0 0 * * * *".into()
            }
        );
        let every: Cadence =
            serde_yaml::from_str("{ every_ms: 300000 }").expect("the interval shape");
        assert_eq!(every, Cadence::Every { every_ms: 300_000 });
    }
}
