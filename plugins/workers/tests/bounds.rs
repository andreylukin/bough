//! §7/§10: the three bounds live in the DEFINITION. Every one of them refuses the excess with a
//! `BoundsExceeded` that NAMES the bound, its current value and its limit — a spawner that hits
//! one can tell the model something true instead of "failed" — and the per-wake cap resets at the
//! next wake, because it is a property of the wake and not of the agent.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::AgentId;
use bough_plugin_ledger::{AgentName, StepId, WakeId};
use bough_plugin_workers::{
    AskMode, Bounds, SealSpec, StartWorker, WorkerError, WorkerKind, WorkerOutcome, WorkerProvider,
    WorkerResult, WorkerRun, WorkersHandle,
};

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

fn bounds() -> Bounds {
    Bounds {
        max_in_flight: 2,
        max_depth: 3,
        per_wake_spawn_cap: 2,
    }
}

fn req(wake: &str, depth: u8) -> StartWorker {
    StartWorker {
        kind: WorkerKind::Spawn,
        spawner: AgentName::new("sol"),
        spawner_id: AgentId::new("a1"),
        wake: WakeId::new(wake),
        step: StepId::new("s1"),
        depth,
        task: "do the thing".into(),
        seal: SealSpec::report(),
        tools: None,
        ask_mode: AskMode::End,
        at: chrono::Utc::now(),
    }
}

/// A provider that parks until it is released, so a test can hold runs in flight.
struct Parking(Arc<tokio::sync::Semaphore>);

#[async_trait::async_trait]
impl WorkerProvider for Parking {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Spawn]
    }
    async fn start(
        &self,
        _req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError> {
        let _permit = self.0.acquire().await.expect("semaphore open");
        Ok(WorkerResult {
            worker: run.id().clone(),
            outcome: WorkerOutcome::Done,
            report: None,
            steps: 0,
            usage: Default::default(),
            report_step: None,
        })
    }
}

/// A provider that returns at once.
struct Instant;

#[async_trait::async_trait]
impl WorkerProvider for Instant {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Spawn]
    }
    async fn start(
        &self,
        _req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError> {
        Ok(WorkerResult {
            worker: run.id().clone(),
            outcome: WorkerOutcome::Done,
            report: None,
            steps: 0,
            usage: Default::default(),
            report_step: None,
        })
    }
}

async fn with_provider(p: Arc<dyn WorkerProvider>) -> (WorkersHandle, Context) {
    let ctx = ctx();
    let h = WorkersHandle::new(bounds());
    h.provider(&ctx, p).await.expect("provider registers");
    (h, ctx)
}

#[tokio::test]
async fn max_depth_refuses_the_generation_past_the_limit_and_names_it() {
    let (h, ctx) = with_provider(Arc::new(Instant)).await;
    h.start(&ctx, req("w1", 3))
        .await
        .expect("depth 3 is allowed");
    let err = h
        .start(&ctx, req("w2", 4))
        .await
        .expect_err("depth 4 is one generation too many");
    match err {
        WorkerError::BoundsExceeded {
            bound,
            current,
            limit,
        } => {
            assert_eq!(bound, "max_depth");
            assert_eq!((current, limit), (4, 3));
        }
        other => panic!("wrong refusal: {other}"),
    }
    assert!(
        err.to_string().contains("max_depth"),
        "the message does not name the bound: {err}"
    );
}

#[tokio::test]
async fn max_in_flight_refuses_the_third_concurrent_run_and_names_it() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (h, ctx) = with_provider(Arc::new(Parking(gate.clone()))).await;

    // Two runs park inside the provider; the seam sees two in flight.
    let (h1, c1) = (h.clone(), ctx.clone());
    let a = tokio::spawn(async move { h1.start(&c1, req("w1", 1)).await });
    let (h2, c2) = (h.clone(), ctx.clone());
    let b = tokio::spawn(async move { h2.start(&c2, req("w2", 1)).await });
    while h.in_flight() < 2 {
        tokio::task::yield_now().await;
    }

    let err = h
        .start(&ctx, req("w3", 1))
        .await
        .expect_err("a third run exceeds max_in_flight");
    match err {
        WorkerError::BoundsExceeded {
            bound,
            current,
            limit,
        } => {
            assert_eq!(bound, "max_in_flight");
            assert_eq!((current, limit), (2, 2));
        }
        other => panic!("wrong refusal: {other}"),
    }
    assert!(err.to_string().contains("max_in_flight"), "{err}");

    gate.add_permits(2);
    a.await.unwrap().expect("first finishes");
    b.await.unwrap().expect("second finishes");
    assert_eq!(h.in_flight(), 0, "a finished run leaves the table");

    // And with the table empty the same request is admitted: the bound was in-flight, not total.
    h.start(&ctx, req("w4", 1)).await.expect("room again");
}

#[tokio::test]
async fn the_per_wake_cap_refuses_the_third_spawn_of_one_wake_and_names_it() {
    let (h, ctx) = with_provider(Arc::new(Instant)).await;
    h.start(&ctx, req("w1", 1))
        .await
        .expect("first of the wake");
    h.start(&ctx, req("w1", 1))
        .await
        .expect("second of the wake");
    let err = h
        .start(&ctx, req("w1", 1))
        .await
        .expect_err("a third spawn in one wake is refused");
    match err {
        WorkerError::BoundsExceeded {
            bound,
            current,
            limit,
        } => {
            assert_eq!(bound, "per_wake_spawn_cap");
            assert_eq!((current, limit), (2, 2));
        }
        other => panic!("wrong refusal: {other}"),
    }
    assert!(err.to_string().contains("per_wake_spawn_cap"), "{err}");
}

/// The cap is a property of the WAKE. The next wake starts from zero even though nothing was
/// released and the agent is the same one.
#[tokio::test]
async fn the_per_wake_cap_resets_at_the_next_wake() {
    let (h, ctx) = with_provider(Arc::new(Instant)).await;
    h.start(&ctx, req("w1", 1)).await.expect("first");
    h.start(&ctx, req("w1", 1)).await.expect("second");
    assert!(h.start(&ctx, req("w1", 1)).await.is_err());
    assert_eq!(h.spawned_in_wake(&WakeId::new("w1")), 2);

    h.start(&ctx, req("w2", 1))
        .await
        .expect("a new wake starts from zero");
    h.start(&ctx, req("w2", 1)).await.expect("and gets its two");
    assert!(h.start(&ctx, req("w2", 1)).await.is_err());
    // The earlier wake's counter is untouched: the two wakes have separate budgets.
    assert_eq!(h.spawned_in_wake(&WakeId::new("w1")), 2);
}

/// A kind nobody provides is refused BEFORE anything is reserved: the depth check is a property
/// of the request, but the run table must not move for a start that cannot happen.
#[tokio::test]
async fn a_kind_with_no_provider_is_refused_and_reserves_nothing() {
    let ctx = ctx();
    let h = WorkersHandle::new(bounds());
    let err = h
        .start(&ctx, req("w1", 1))
        .await
        .expect_err("no provider is registered");
    assert!(
        matches!(err, WorkerError::NoProvider(WorkerKind::Spawn)),
        "{err}"
    );
    assert_eq!(h.in_flight(), 0);
    assert_eq!(h.spawned_in_wake(&WakeId::new("w1")), 0);
}

/// A refusal must not spend the budget it refused: the counters are the seam's, not the caller's.
#[tokio::test]
async fn a_refused_start_does_not_spend_the_per_wake_budget() {
    let (h, ctx) = with_provider(Arc::new(Instant)).await;
    assert!(h.start(&ctx, req("w1", 9)).await.is_err(), "too deep");
    assert_eq!(h.spawned_in_wake(&WakeId::new("w1")), 0);
    h.start(&ctx, req("w1", 1))
        .await
        .expect("still has its two");
    h.start(&ctx, req("w1", 1))
        .await
        .expect("still has its two");
}
