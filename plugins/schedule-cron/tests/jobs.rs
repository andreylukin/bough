//! §9: what the production Provider promises — a unique name, an effect-shaped registration, a
//! catch-up fire for a job whose stored last run is stale, a bounded run, and a scheduler that
//! keeps ticking after a body panics.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_schedule::{
    Cadence, FireReason, Job, JobFire, JobName, JobOutcome, JobRun, JobSpec, ScheduleError,
    Scheduler,
};
use bough_plugin_schedule_cron::{state::RunStore, CronConfig, CronScheduler};

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

fn cfg(dir: &std::path::Path, timeout_ms: u64) -> Arc<CronConfig> {
    Arc::new(CronConfig {
        state_db: dir.join("schedule.db"),
        job_timeout_ms: timeout_ms,
    })
}

/// A job that counts its fires and reports the reason it was given.
#[derive(Default)]
struct Counting {
    fires: AtomicUsize,
    reasons: parking_lot::Mutex<Vec<FireReason>>,
}

#[async_trait::async_trait]
impl Job for Counting {
    async fn run(&self, fire: JobFire) -> JobOutcome {
        self.fires.fetch_add(1, Ordering::SeqCst);
        self.reasons.lock().push(fire.reason);
        JobOutcome::Ran {
            detail: format!("{:?}", fire.reason),
        }
    }
}

struct Panicking;

#[async_trait::async_trait]
impl Job for Panicking {
    async fn run(&self, _fire: JobFire) -> JobOutcome {
        panic!("this body is broken")
    }
}

struct Hanging;

#[async_trait::async_trait]
impl Job for Hanging {
    async fn run(&self, _fire: JobFire) -> JobOutcome {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        JobOutcome::Ran {
            detail: "never".into(),
        }
    }
}

fn spec(name: &str, cadence: Cadence, catch_up: bool, job: Arc<dyn Job>) -> JobSpec {
    JobSpec {
        name: JobName::new(name),
        cadence,
        catch_up,
        job,
    }
}

fn every(ms: u64) -> Cadence {
    Cadence::Every { every_ms: ms }
}

#[tokio::test]
async fn a_duplicate_name_is_refused_at_registration() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 1000), ctx.clone())
        .await
        .expect("a scheduler");
    let job = Arc::new(Counting::default());
    sched
        .register(&ctx, spec("sweep", every(3_600_000), false, job.clone()))
        .await
        .expect("the first registration");
    let err = match sched
        .register(&ctx, spec("sweep", every(3_600_000), false, job))
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("a job name is unique in the tree"),
    };
    assert!(err.to_string().contains("already registered"), "{err}");
    assert_eq!(sched.jobs().len(), 1);
    sched.stop().await;
}

#[tokio::test]
async fn a_registrations_disposer_removes_exactly_that_job() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 1000), ctx.clone())
        .await
        .expect("a scheduler");
    let mine = sched
        .register(
            &ctx,
            spec(
                "mine",
                every(3_600_000),
                false,
                Arc::new(Counting::default()),
            ),
        )
        .await
        .expect("registered");
    sched
        .register(
            &ctx,
            spec(
                "theirs",
                every(3_600_000),
                false,
                Arc::new(Counting::default()),
            ),
        )
        .await
        .expect("registered");
    assert_eq!(sched.jobs().len(), 2);

    mine.dispose().await;

    let left: Vec<String> = sched.jobs().iter().map(|j| j.name.to_string()).collect();
    assert_eq!(left, vec!["theirs".to_string()]);
    // And the job that left is gone from the seam entirely.
    assert!(matches!(
        sched.fire_now(&JobName::new("mine")).await,
        Err(ScheduleError::Unknown(_))
    ));
    sched.stop().await;
}

#[tokio::test]
async fn fire_now_on_an_unknown_name_is_unknown() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 1000), ctx.clone())
        .await
        .expect("a scheduler");
    match sched.fire_now(&JobName::new("nobody")).await {
        Err(ScheduleError::Unknown(name)) => assert_eq!(name.as_str(), "nobody"),
        other => panic!("expected Unknown, got {other:?}"),
    }
    sched.stop().await;
}

#[tokio::test]
async fn a_stale_stored_last_run_fires_once_as_catch_up_and_then_follows_cadence() {
    let dir = tempfile::tempdir().expect("a temp dir");
    // A last run one hour ago, written by a previous process: `catch_up` is about surviving a
    // restart, so the state is seeded through the store rather than through a fire.
    {
        let store = RunStore::open(&dir.path().join("schedule.db")).expect("a fresh store");
        store
            .set(
                &JobName::new("sweep"),
                &JobRun {
                    at: chrono::Utc::now() - chrono::Duration::hours(1),
                    reason: FireReason::Cadence,
                    outcome: JobOutcome::Ran {
                        detail: "the last process".into(),
                    },
                },
            )
            .expect("seeded");
    }
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 2000), ctx.clone())
        .await
        .expect("a scheduler");
    let job = Arc::new(Counting::default());
    sched
        .register(
            &ctx, // The finest cadence the library's own 500ms tick can honour (`SCHEDULER_TICK_MS`).
            spec("sweep", every(500), true, job.clone()),
        )
        .await
        .expect("registered");

    // EXACTLY ONE catch-up fire, at activation, before the cadence has had a chance to tick.
    assert_eq!(job.reasons.lock().as_slice(), &[FireReason::CatchUp]);
    let last = sched.jobs()[0].last.clone().expect("a recorded run");
    assert_eq!(last.reason, FireReason::CatchUp);

    // …and then the ordinary cadence takes over.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if job.reasons.lock().contains(&FireReason::Cadence) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the cadence never came round"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let reasons = job.reasons.lock().clone();
    assert_eq!(
        reasons
            .iter()
            .filter(|r| **r == FireReason::CatchUp)
            .count(),
        1,
        "catch-up fires ONCE: {reasons:?}"
    );
    sched.stop().await;
}

#[tokio::test]
async fn a_fresh_job_with_catch_up_off_does_not_fire_at_activation() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 1000), ctx.clone())
        .await
        .expect("a scheduler");
    let job = Arc::new(Counting::default());
    sched
        .register(&ctx, spec("sweep", every(3_600_000), false, job.clone()))
        .await
        .expect("registered");
    assert_eq!(job.fires.load(Ordering::SeqCst), 0);
    assert!(sched.jobs()[0].last.is_none());
    sched.stop().await;
}

#[tokio::test]
async fn a_panicking_job_is_failed_and_the_scheduler_keeps_ticking() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 1000), ctx.clone())
        .await
        .expect("a scheduler");
    sched
        .register(
            &ctx,
            spec("broken", every(3_600_000), false, Arc::new(Panicking)),
        )
        .await
        .expect("registered");
    let ok = Arc::new(Counting::default());
    sched
        .register(&ctx, spec("fine", every(3_600_000), false, ok.clone()))
        .await
        .expect("registered");

    let run = sched
        .fire_now(&JobName::new("broken"))
        .await
        .expect("the fire itself succeeds");
    match &run.outcome {
        JobOutcome::Failed { error } => assert!(error.contains("panicked"), "{error}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    // The scheduler is still alive and still fires other jobs.
    let after = sched
        .fire_now(&JobName::new("fine"))
        .await
        .expect("still ticking");
    assert!(matches!(after.outcome, JobOutcome::Ran { .. }));
    assert_eq!(ok.fires.load(Ordering::SeqCst), 1);
    sched.stop().await;
}

#[tokio::test]
async fn a_run_that_outlives_the_timeout_is_abandoned_and_failed() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 100), ctx.clone())
        .await
        .expect("a scheduler");
    sched
        .register(
            &ctx,
            spec("hangs", every(3_600_000), false, Arc::new(Hanging)),
        )
        .await
        .expect("registered");
    let run = sched
        .fire_now(&JobName::new("hangs"))
        .await
        .expect("the fire returns");
    match &run.outcome {
        JobOutcome::Failed { error } => assert!(error.contains("job_timeout_ms"), "{error}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    sched.stop().await;
}

#[tokio::test]
async fn every_fire_leaves_exactly_one_recorded_run_that_survives_a_restart() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 1000), ctx.clone())
        .await
        .expect("a scheduler");
    sched
        .register(
            &ctx,
            spec(
                "sweep",
                every(3_600_000),
                false,
                Arc::new(Counting::default()),
            ),
        )
        .await
        .expect("registered");
    let run = sched.fire_now(&JobName::new("sweep")).await.expect("fired");
    // The recorded moment is millisecond-precise, so the listing and the store agree exactly.
    assert_eq!(run.at.timestamp_subsec_nanos() % 1_000_000, 0);
    assert_eq!(sched.jobs()[0].last.as_ref(), Some(&run));
    let stored = sched.stored().expect("the store");
    assert_eq!(stored.get("sweep"), Some(&run));
    sched.stop().await;

    // A fresh process reads the same last run — which is what `catch_up` depends on.
    let store = RunStore::open(&dir.path().join("schedule.db")).expect("the same store");
    assert_eq!(store.get(&JobName::new("sweep")).unwrap(), Some(run));
}

/// The floor is the LIBRARY's tick, not a config field a deployment could lower under it. Before
/// this, `tick_ms` was a shipped, validated field nothing gave to `tokio-cron-scheduler` — so a
/// row that set `tick_ms: 50` had its `every_ms: 50` job ACCEPTED and then fired ~10x slower.
#[tokio::test(flavor = "multi_thread")]
async fn a_cadence_finer_than_the_librarys_tick_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ctx = ctx();
    let sched = CronScheduler::start(cfg(dir.path(), 2000), ctx.clone())
        .await
        .expect("a scheduler");
    let job = Arc::new(Counting::default());
    let e = sched
        .register(
            &ctx,
            spec(
                "too-fine",
                every(bough_plugin_schedule_cron::SCHEDULER_TICK_MS - 1),
                false,
                job.clone(),
            ),
        )
        .await
        .err()
        .expect("finer than the tick is refused");
    let text = e.to_string();
    assert!(text.contains("499"), "{text}");
    assert!(
        text.contains(&format!(
            "{}",
            bough_plugin_schedule_cron::SCHEDULER_TICK_MS
        )),
        "the refusal names the tick it is measured against: {text}"
    );
    assert!(sched.jobs().is_empty(), "nothing was registered");
}
