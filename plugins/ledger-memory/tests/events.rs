//! `ledger/step` is DURABLE (§0.2): the row is committed and readable before the event fires, and
//! an observer can neither fail nor delay the append. The same four cases run against
//! `ledger-sqlite`; a divergence shows up as the same named test failing on one provider.
//!
//! Every case awaits a RECEIPT rather than sleeping: Phase 0's `emit` dispatch is spawned and
//! never awaited (a Phase 1 deferral, not a Phase 1 fix).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    Append, Class, LedgerStep, LedgerStore, Seq, Step, StepId, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use chrono::{TimeZone, Utc};
use parking_lot::Mutex;
use tokio::sync::Notify;

/// A recording listener with a receipt: `wait_for(n)` returns once `n` steps have arrived.
#[derive(Clone, Default)]
struct Tap {
    seen: Arc<Mutex<Vec<Arc<Step>>>>,
    bell: Arc<Notify>,
}

impl Tap {
    async fn wait_for(&self, n: usize) -> Vec<Arc<Step>> {
        loop {
            {
                let seen = self.seen.lock();
                if seen.len() >= n {
                    return seen.clone();
                }
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), self.bell.notified())
                .await
                .expect("ledger/step never arrived");
        }
    }
}

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

fn note(traj: &str, i: u32) -> Append {
    Append {
        traj: TrajId::new(traj),
        wake: WakeId::new("w1"),
        kind: StepType::new("step/start"),
        class: Class::Thought,
        body: serde_json::json!({ "index": i }),
        cites: vec![],
        at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        id: Some(StepId::new(format!("{traj}-{i}"))),
    }
}

async fn tapped(ctx: &Context) -> Tap {
    let tap = Tap::default();
    let t = tap.clone();
    ctx.on::<LedgerStep, _, _>(move |step| {
        let t = t.clone();
        async move {
            t.seen.lock().push(step);
            t.bell.notify_waiters();
        }
    })
    .await
    .expect("listener registers");
    tap
}

/// The event is DURABLE: by the time a listener sees a step, `step(id)` already answers with it.
#[tokio::test]
async fn ledger_step_arrives_after_the_row_is_readable() {
    let ctx = ctx();
    let store = MemoryStore::new(ctx.clone());
    let readable = Arc::new(Mutex::new(Vec::<bool>::new()));
    let bell = Arc::new(Notify::new());

    let (s, r, b) = (store.clone(), readable.clone(), bell.clone());
    ctx.on::<LedgerStep, _, _>(move |step| {
        let (s, r, b) = (s.clone(), r.clone(), b.clone());
        async move {
            let seen = s.step(&step.id).await.unwrap().is_some();
            r.lock().push(seen);
            b.notify_waiters();
        }
    })
    .await
    .unwrap();

    store.append(note("t", 0)).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), bell.notified())
        .await
        .expect("ledger/step never arrived");
    assert_eq!(
        *readable.lock(),
        vec![true],
        "the row must be readable when the event fires"
    );
}

/// A listener that panics is the listener's problem: the append already committed.
#[tokio::test]
async fn a_panicking_listener_does_not_fail_the_append() {
    let ctx = ctx();
    let store = MemoryStore::new(ctx.clone());
    ctx.on::<LedgerStep, _, _>(|_step| async move { panic!("a listener blew up") })
        .await
        .unwrap();
    // A second listener gives the test a receipt that dispatch survived the first one.
    let tap = tapped(&ctx).await;

    let step = store
        .append(note("t", 0))
        .await
        .expect("append still commits");
    assert_eq!(step.seq, Seq(1));
    assert!(store.step(&step.id).await.unwrap().is_some());
    let seen = tap.wait_for(1).await;
    assert_eq!(seen[0].id, step.id);
}

/// Emit mode: dispatch is fire-and-forget, so a slow observer cannot hold up a writer.
#[tokio::test]
async fn a_blocking_listener_does_not_delay_the_append() {
    let ctx = ctx();
    let store = MemoryStore::new(ctx.clone());
    let released = Arc::new(AtomicBool::new(false));
    let r = released.clone();
    ctx.on::<LedgerStep, _, _>(move |_step| {
        let r = r.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            r.store(true, Ordering::SeqCst);
        }
    })
    .await
    .unwrap();

    let started = std::time::Instant::now();
    store.append(note("t", 0)).await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "append waited {elapsed:?} on a blocked listener"
    );
    assert!(
        !released.load(Ordering::SeqCst),
        "the listener is still blocked, which is what makes the timing meaningful"
    );
}

/// One event per step, in seq order — a batch is one commit, not one event.
#[tokio::test]
async fn batch_appends_emit_one_event_per_step_in_seq_order() {
    let ctx = ctx();
    let store = MemoryStore::new(ctx.clone());
    let tap = tapped(&ctx).await;

    let reqs: Vec<Append> = (0..5).map(|i| note("t", i)).collect();
    let committed = store.append_batch(reqs).await.unwrap();
    assert_eq!(
        committed.iter().map(|s| s.seq.0).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );

    let seen = tap.wait_for(5).await;
    assert_eq!(seen.len(), 5, "one event per step, no more");
    assert_eq!(
        seen.iter().map(|s| s.seq.0).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5],
        "in seq order"
    );
}
