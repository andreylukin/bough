//! V5: projection assembly stays under 50ms on `ledger-sqlite` over a large trajectory.
//!
//! The fixture is built through the REAL append path — the live `ledger` binding of a booted
//! tree — so what is measured is the provider and the assembler, not a shortcut. `assemble()` is
//! timed as the BEST OF THREE runs (P1-D18): a wall clock on a shared machine is noisy, and a
//! looser bound would be a number nobody chose.
//!
//! The 10k case is always on. The 100k case runs only with `BOUGH_BENCH=1` and prints its number.

mod support;

use std::time::{Duration, Instant};

use bough_plugin_hello::trace;
use bough_plugin_ledger::vocabulary::{StepEnd, StepOutcome, StepStart};
use bough_plugin_ledger::{Append, Class, Ledger, LedgerHandle, StepType, TrajId, WakeId};
use bough_plugin_projection::{AssembleRequest, Projection};

/// The bound §17 Phase 1 sets. One number, both cases.
const BOUND: Duration = Duration::from_millis(50);

const P1: &str = "\
- id: ledger
  plugin: ledger-sqlite
  config:
    path: !!expr 'bough_path(\"ledger.db\")'
    busy_timeout_ms: 5000
- id: projection
  plugin: projection-assembler
  config:
    budget_tokens: 160000
    headroom: 0.6
    tail_steps: 60
    tail_floor_steps: 10
    mail_newest_n: 5
    max_tiers: 3
    file_view_dir: !!expr 'bough_path(\"views\")'
- id: probe
  plugin: projection-probe
  config:
    traj: t1
    agent: a1
    steps: 1
";

/// Append `n` more steps to the probe's trajectory through the live `ledger` binding, in batches,
/// each wake properly opened and closed so the enclosure invariant still holds.
async fn grow(ledger: &LedgerHandle, n: usize) {
    let at = chrono::Utc::now();
    let traj = TrajId::new("t1");
    const BATCH: usize = 1000;
    let mut written = 0usize;
    let mut wake_no = 0usize;
    while written < n {
        wake_no += 1;
        let wake = WakeId::new(format!("t1-bench-{wake_no}"));
        let mut reqs = Vec::with_capacity(BATCH);
        reqs.push(one(&traj, &wake, "wake/start", wake_start(), at));
        let body = BATCH.min(n - written).saturating_sub(2).max(2);
        for i in 0..body / 2 {
            let index = i as u32;
            reqs.push(one(
                &traj,
                &wake,
                "step/start",
                serde_json::to_value(StepStart { index }).unwrap(),
                at,
            ));
            reqs.push(one(
                &traj,
                &wake,
                "step/end",
                serde_json::to_value(StepEnd {
                    index,
                    outcome: StepOutcome::Ok,
                    detail: None,
                })
                .unwrap(),
                at,
            ));
        }
        reqs.push(one(&traj, &wake, "wake/end", wake_end(), at));
        written += reqs.len();
        ledger
            .0
            .append_batch(reqs)
            .await
            .expect("the bench fixture appends");
    }
}

fn wake_start() -> serde_json::Value {
    serde_json::to_value(bough_plugin_ledger::vocabulary::WakeStart {
        urgency: bough_plugin_ledger::vocabulary::Urgency::Immediate,
        trigger: None,
        claimed: Vec::new(),
    })
    .unwrap()
}

fn wake_end() -> serde_json::Value {
    serde_json::to_value(bough_plugin_ledger::vocabulary::WakeEnd {
        reason: bough_plugin_ledger::vocabulary::WakeEndReason::Completed,
        cause: None,
        consumed: Vec::new(),
    })
    .unwrap()
}

fn one(
    traj: &TrajId,
    wake: &WakeId,
    kind: &str,
    body: serde_json::Value,
    at: chrono::DateTime<chrono::Utc>,
) -> Append {
    Append {
        traj: traj.clone(),
        wake: wake.clone(),
        kind: StepType::new(kind),
        class: Class::Thought,
        body,
        cites: Vec::new(),
        at,
        id: None,
    }
}

/// Boot the tree, grow the trajectory to `n` steps, and return the best of three `assemble()`s.
async fn best_of_three(
    n: usize,
) -> (
    Duration,
    std::sync::Arc<bough_kernel::Kernel>,
    support::TempDir,
) {
    // The dir is RETURNED, not leaked: its `Drop` is the only thing that removes `$BOUGH_HOME`,
    // and a 100k-step db left in the system temp dir on every `make gates` is litter.
    let (kernel, dir) = support::boot_with(P1).await;
    let ledger = kernel
        .root()
        .peek_live::<Ledger>()
        .expect("ledger is bound")
        .as_ref()
        .clone();
    grow(&ledger, n).await;
    assert!(
        ledger
            .0
            .head_seq(&TrajId::new("t1"))
            .await
            .expect("head_seq")
            .expect("a non-empty trajectory")
            .0
            >= n as u64,
        "the fixture must actually hold {n} steps, or the measurement is of nothing"
    );

    let projection = kernel
        .root()
        .peek_live::<Projection>()
        .expect("projection is bound")
        .as_ref()
        .clone();
    let req = AssembleRequest {
        agent: bough_plugin_ledger::AgentName::new("a1"),
        wake: None,
        at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .into(),
        budget: None,
    };
    let mut best = Duration::MAX;
    for _ in 0..3 {
        let t0 = Instant::now();
        let out = projection
            .0
            .assemble(&req)
            .await
            .expect("the projection assembles");
        best = best.min(t0.elapsed());
        assert!(
            !out.to_text().is_empty(),
            "an empty projection is not a measurement"
        );
    }
    (best, kernel, dir)
}

#[tokio::test]
async fn assembly_over_10k_steps_is_under_the_bound() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (best, kernel, _dir) = best_of_three(10_000).await;
    println!("assemble(10k) = {:.2}ms", best.as_secs_f64() * 1000.0);
    assert!(
        best <= BOUND,
        "assemble(10k) took {:.2}ms, over the {}ms bound",
        best.as_secs_f64() * 1000.0,
        BOUND.as_millis()
    );
    kernel.shutdown().await;
}

/// `#[ignore]` and not an early `return`: a skipped test that reports `ok` is indistinguishable
/// from coverage. Run it with `BOUGH_BENCH=1 cargo test -- --ignored`.
#[tokio::test]
#[ignore = "the 100k bench is slow; run with --ignored"]
async fn assembly_over_100k_steps_is_under_50ms() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    let (best, kernel, _dir) = best_of_three(100_000).await;
    println!("assemble(100k) = {:.2}ms", best.as_secs_f64() * 1000.0);
    assert!(
        best <= BOUND,
        "assemble(100k) took {:.2}ms, over the {}ms bound",
        best.as_secs_f64() * 1000.0,
        BOUND.as_millis()
    );
    kernel.shutdown().await;
}
