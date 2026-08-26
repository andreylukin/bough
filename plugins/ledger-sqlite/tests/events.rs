//! Invariant under test: `ledger/step` is DURABLE and CONTAINED (§0.2, §3). The row is committed
//! and readable before any listener sees the event; a listener that panics does not fail the
//! append; a listener that blocks does not delay it.
//!
//! Every case here awaits a RECEIPT from the tap — never a sleep — because Phase 0's `emit`
//! dispatch is spawned and never awaited (a deferral Phase 1 lives with).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{Append, Class, LedgerStep, LedgerStore, Step, StepType, TrajId, WakeId};
use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};
use chrono::Utc;
use parking_lot::Mutex;
use tokio::sync::Notify;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// A recording listener with a receipt: `wait_for(n)` returns once `n` steps have arrived.
#[derive(Clone, Default)]
struct Tap {
    seen: Arc<Mutex<Vec<Arc<Step>>>>,
    bell: Arc<Notify>,
}

impl Tap {
    fn push(&self, step: Arc<Step>) {
        self.seen.lock().push(step);
        self.bell.notify_waiters();
    }
    async fn wait_for(&self, n: usize) -> Vec<Arc<Step>> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let seen = self.seen.lock();
                if seen.len() >= n {
                    return seen.clone();
                }
            }
            let notified = self.bell.notified();
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                panic!(
                    "only {} of {n} ledger/step events arrived",
                    self.seen.lock().len()
                );
            }
        }
    }
}

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

fn store(ctx: Context) -> Arc<SqliteStore> {
    SqliteStore::open(
        &SqliteConfig {
            path: ":memory:".into(),
            busy_timeout_ms: 5000,
        },
        ctx,
    )
    .expect("in-memory ledger opens")
}

fn note(traj: &str, index: u32) -> Append {
    Append {
        traj: TrajId::new(traj),
        wake: WakeId::new("w1"),
        kind: StepType::new("step/start"),
        class: Class::Thought,
        body: serde_json::json!({ "index": index }),
        cites: vec![],
        at: Utc::now(),
        id: None,
    }
}

// ---------------------------------------------------------------------------
// cases
// ---------------------------------------------------------------------------

/// V7: the event is POST-COMMIT. A listener that reads the ledger back must find the row — if the
/// event fired from inside the transaction, this would see `None`.
#[tokio::test]
async fn ledger_step_arrives_after_the_row_is_readable() {
    let ctx = ctx();
    let s = store(ctx.clone());
    let tap = Tap::default();

    let readable = Arc::new(Mutex::new(Vec::<bool>::new()));
    {
        let (s, tap, readable) = (s.clone(), tap.clone(), readable.clone());
        ctx.on::<LedgerStep, _, _>(move |step| {
            let (s, tap, readable) = (s.clone(), tap.clone(), readable.clone());
            async move {
                let found = s.step(&step.id).await.expect("read back").is_some();
                readable.lock().push(found);
                tap.push(step);
            }
        })
        .await
        .expect("listener registers");
    }

    let step = s.append(note("t", 0)).await.expect("append");
    let seen = tap.wait_for(1).await;
    assert_eq!(seen[0].id, step.id);
    assert_eq!(
        *readable.lock(),
        vec![true],
        "the row was not readable when the event fired"
    );
}

/// §3: "observers never block it and observer failures are contained per listener."
#[tokio::test]
async fn a_panicking_listener_does_not_fail_the_append() {
    let ctx = ctx();
    let s = store(ctx.clone());
    let tap = Tap::default();

    ctx.on::<LedgerStep, _, _>(|_step| async move {
        panic!("a listener that blows up on purpose");
    })
    .await
    .expect("listener registers");
    {
        let tap = tap.clone();
        ctx.on::<LedgerStep, _, _>(move |step| {
            let tap = tap.clone();
            async move { tap.push(step) }
        })
        .await
        .expect("second listener registers");
    }

    let step = s
        .append(note("t", 0))
        .await
        .expect("the append still commits");
    assert_eq!(s.head_seq(&step.traj).await.unwrap().unwrap().0, 1);
    // The panic is contained per listener: the one after it still ran.
    assert_eq!(tap.wait_for(1).await.len(), 1);
}

/// The append is a synchronous commit on the single writer; dispatch is fire-and-forget.
#[tokio::test]
async fn a_blocking_listener_does_not_delay_the_append() {
    let ctx = ctx();
    let s = store(ctx.clone());
    let started = Arc::new(AtomicUsize::new(0));
    {
        let started = started.clone();
        ctx.on::<LedgerStep, _, _>(move |_step| {
            let started = started.clone();
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        })
        .await
        .expect("listener registers");
    }

    let t0 = std::time::Instant::now();
    s.append(note("t", 0)).await.expect("append");
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "the append waited on a listener: {elapsed:?}"
    );
}

#[tokio::test]
async fn batch_appends_emit_one_event_per_step_in_seq_order() {
    let ctx = ctx();
    let s = store(ctx.clone());
    let tap = Tap::default();
    {
        let tap = tap.clone();
        ctx.on::<LedgerStep, _, _>(move |step| {
            let tap = tap.clone();
            async move { tap.push(step) }
        })
        .await
        .expect("listener registers");
    }

    let batch: Vec<Append> = (0..5).map(|i| note("t", i)).collect();
    let steps = s.append_batch(batch).await.expect("batch append");
    assert_eq!(
        steps.iter().map(|s| s.seq.0).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5],
        "a batch is one contiguous run"
    );

    let seen = tap.wait_for(5).await;
    assert_eq!(seen.len(), 5, "one event per step, no more");
    assert_eq!(
        seen.iter().map(|s| s.seq.0).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5],
        "events must arrive in seq order"
    );
}
