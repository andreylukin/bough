//! P6-D1: the test Provider fires ONLY on demand, refuses exactly what the production Provider
//! refuses, and its registration is the same effect — so a downstream test written against it is
//! a test of the seam and not of a toy.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_schedule::{
    Cadence, FireReason, Job, JobFire, JobName, JobOutcome, JobSpec, ScheduleError, Scheduler,
};
use bough_plugin_schedule_manual::ManualScheduler;

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

#[derive(Default)]
struct Counting(AtomicUsize);

#[async_trait::async_trait]
impl Job for Counting {
    async fn run(&self, fire: JobFire) -> JobOutcome {
        self.0.fetch_add(1, Ordering::SeqCst);
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

fn spec(name: &str, job: Arc<dyn Job>) -> JobSpec {
    JobSpec {
        name: JobName::new(name),
        cadence: Cadence::Every { every_ms: 60_000 },
        catch_up: true,
        job,
    }
}

#[tokio::test]
async fn nothing_fires_without_a_call_not_even_with_catch_up_on() {
    let ctx = ctx();
    let sched = ManualScheduler::new_in(ctx.clone());
    let job = Arc::new(Counting::default());
    sched
        .register(&ctx, spec("sweep", job.clone()))
        .await
        .expect("registered");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(job.0.load(Ordering::SeqCst), 0);
    assert_eq!(sched.jobs()[0].next, None, "this Provider has no clock");
    assert!(sched.jobs()[0].last.is_none());
}

#[tokio::test]
async fn fire_now_runs_the_body_once_and_records_it() {
    let ctx = ctx();
    let sched = ManualScheduler::new_in(ctx.clone());
    let job = Arc::new(Counting::default());
    sched
        .register(&ctx, spec("sweep", job.clone()))
        .await
        .expect("registered");
    let run = sched.fire_now(&JobName::new("sweep")).await.expect("fired");
    assert_eq!(run.reason, FireReason::Manual);
    assert_eq!(job.0.load(Ordering::SeqCst), 1);
    assert_eq!(sched.jobs()[0].last.as_ref(), Some(&run));
}

#[tokio::test]
async fn fire_at_names_the_clock_and_the_reason() {
    let ctx = ctx();
    let sched = ManualScheduler::new_in(ctx.clone());
    sched
        .register(&ctx, spec("sweep", Arc::new(Counting::default())))
        .await
        .expect("registered");
    let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant");
    let run = sched
        .fire_at(&JobName::new("sweep"), at, FireReason::CatchUp)
        .await
        .expect("fired");
    assert_eq!(run.at, at);
    assert_eq!(run.reason, FireReason::CatchUp);
}

#[tokio::test]
async fn a_duplicate_name_is_refused_and_a_bad_cadence_too() {
    let ctx = ctx();
    let sched = ManualScheduler::new_in(ctx.clone());
    sched
        .register(&ctx, spec("sweep", Arc::new(Counting::default())))
        .await
        .expect("the first registration");
    match sched
        .register(&ctx, spec("sweep", Arc::new(Counting::default())))
        .await
    {
        Err(e) => assert!(e.to_string().contains("already registered"), "{e}"),
        Ok(_) => panic!("a job name is unique in the tree"),
    }

    let bad = JobSpec {
        cadence: Cadence::Every { every_ms: 0 },
        ..spec("zero", Arc::new(Counting::default()))
    };
    match sched.register(&ctx, bad).await {
        Err(e) => assert!(e.to_string().contains("every_ms"), "{e}"),
        Ok(_) => panic!("the same refusal as the production Provider"),
    }
}

#[tokio::test]
async fn a_disposer_removes_exactly_that_job() {
    let ctx = ctx();
    let sched = ManualScheduler::new_in(ctx.clone());
    let mine = sched
        .register(&ctx, spec("mine", Arc::new(Counting::default())))
        .await
        .expect("registered");
    sched
        .register(&ctx, spec("theirs", Arc::new(Counting::default())))
        .await
        .expect("registered");
    mine.dispose().await;
    let left: Vec<String> = sched.jobs().iter().map(|j| j.name.to_string()).collect();
    assert_eq!(left, vec!["theirs".to_string()]);
    assert!(matches!(
        sched.fire_now(&JobName::new("mine")).await,
        Err(ScheduleError::Unknown(_))
    ));
}

#[tokio::test]
async fn fire_now_on_an_unknown_name_is_unknown() {
    let ctx = ctx();
    let sched = ManualScheduler::new_in(ctx.clone());
    match sched.fire_now(&JobName::new("nobody")).await {
        Err(ScheduleError::Unknown(name)) => assert_eq!(name.as_str(), "nobody"),
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[tokio::test]
async fn a_panicking_job_is_failed_here_too() {
    let ctx = ctx();
    let sched = ManualScheduler::new_in(ctx.clone());
    sched
        .register(&ctx, spec("broken", Arc::new(Panicking)))
        .await
        .expect("registered");
    let run = sched
        .fire_now(&JobName::new("broken"))
        .await
        .expect("the fire itself succeeds");
    assert!(matches!(run.outcome, JobOutcome::Failed { .. }), "{run:?}");
}
