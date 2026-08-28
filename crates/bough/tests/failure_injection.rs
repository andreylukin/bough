//! V4 (WP-5, §17 Phase 8, §7, §12): a plugin fiber failing mid-wake ends THAT wake with reason
//! error and the loop continues; a FAILED row is reported and not retried; a panicking listener is
//! contained; and an llm failure arrives as a TERMINAL CHUNK rather than a thrown error.
//!
//! Every case boots the SHIPPED tree (`--root` at the repo, a throwaway `$BOUGH_HOME`) and mounts
//! `fault-inject` through a `--patch` layer of its own, because each case breaks a different named
//! site with different `after`/`times` counters — a committed fixture row would fix them for all
//! six. The fault row is catalog-only (decision D-C8): in the binary, in no bundle.
//!
//! `fault-inject`'s hit counters are process-global, so every case holds BOTH the harness's
//! `$BOUGH_HOME` lock and `fault_inject::test_lock()` for its whole body.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bough_kernel::FiberState;
use bough_plugin_agents::{AgentKind, Agents, CreateAgent, MailClass, Message, MessageId, Sender};
use bough_plugin_hello::trace;
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{AgentName, Ledger, Step, TrajId};
use support::{boot_real, fixture, row_ctx};

/// A `--patch` layer written for one case. Returned as a path because a layer is a FILE (§0.5).
struct Layer(std::path::PathBuf);

impl Layer {
    fn new(tag: &str, yaml: &str) -> Layer {
        let p = std::env::temp_dir().join(format!(
            "bough-fault-{tag}-{}-{}.yml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&p, yaml).expect("the patch layer is writable");
        Layer(p)
    }
    fn path(&self) -> std::path::PathBuf {
        self.0.clone()
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A `fault-inject` row inserted at the end of the tree, breaking one named site.
///
/// Two things here are not obvious and are both load-bearing.
///
/// `needs` is an ENTRY-LEVEL inject (§0.3's entry ∪ plugin-static). `fault-inject` declares its
/// three seams OPTIONAL, and an optional key creates no ordering edge — so a row inserted at the
/// root end applied BEFORE `projection` had committed its binding and refused to boot with "the
/// `projection` seam is not mounted". Naming the seam required at the entry is the sanctioned way
/// to order it, and it keeps the fix in the layer rather than in a crate this package may not edit.
///
/// `agent:` is deliberately NOT set. `fault-inject` filters by `AgentName::new(p.agent.as_str())`
/// over `AgentWakeStopping`, whose `agent` is an `AgentId` (a uuid), so an agent filter can never
/// match a name at that site — recorded as hook H-C6 in docs/track-c-merge-notes.md.
fn fault_row(at: &str, how: &str, after: u32, times: u32, needs: &[&str]) -> String {
    let inject = if needs.is_empty() {
        String::new()
    } else {
        format!("      inject:\n        required: [{}]\n", needs.join(", "))
    };
    format!(
        "insert:\n  - entry:\n      id: fault\n      plugin: fault-inject\n{inject}      \
         config:\n        at: {at}\n        how: {how}\n        after: {after}\n        \
         times: {times}\n"
    )
}

/// Create `sol` and run `n` Andrey wakes over it, returning its chain.
async fn run_wakes(kernel: &bough_kernel::Kernel, n: usize) -> Vec<Step> {
    let ctx = row_ctx(kernel, "exec");
    let agents = ctx.get::<Agents>().expect("the agents key is bound");
    let ledger = ctx.get::<Ledger>().expect("the ledger key is bound");
    let traj = TrajId::new("lane/sol");
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new("sol"),
            traj: traj.clone(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the agent is created");

    for i in 0..n {
        agent
            .followup(Message {
                id: MessageId::new(format!("m-{i}")),
                from: Sender::Andrey,
                class: MailClass::Wake,
                text: format!("task {i}"),
                subject: format!("task {i}"),
                cites: Vec::new(),
                refs: Default::default(),
                mail_seq: None,
                at: chrono::Utc::now(),
            })
            .await
            .expect("mail lands");
        // A hang is a FAILURE, not a hung suite.
        tokio::time::timeout(Duration::from_secs(30), agent.when_idle())
            .await
            .unwrap_or_else(|_| panic!("the agent never went idle after task {i}"));
    }

    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the chain reads back");
    disposer.dispose().await;
    steps
}

/// The `reason` of every `wake/end`, in order.
fn wake_end_reasons(steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .filter(|s| s.kind.as_str() == "wake/end")
        .map(|s| s.body["reason"].as_str().unwrap_or("?").to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// A plugin fiber failing MID-WAKE ends that wake, and the loop continues.
// ---------------------------------------------------------------------------------------------

/// Both halves of §5's "a plugin failure ends the WAKE, not the loop" are one run: the fault fires
/// once, on the first assembly, and the second wake has to complete on the same live driver.
/// Splitting them into two runs would let the second pass with the fault never having fired.
async fn a_faulted_wake_and_the_one_after_it() -> Vec<String> {
    let _guard = trace::test_lock();
    let _faults = bough_plugin_fault_inject::test_lock();
    let layer = Layer::new(
        "section",
        &fault_row("projection_section", "error", 1, 1, &["projection"]),
    );
    let (kernel, _dir) = boot_real("headless", &[fixture("fault.patch.yml"), layer.path()]).await;
    let steps = run_wakes(&kernel, 2).await;
    kernel.shutdown().await;
    wake_end_reasons(&steps)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_section_fault_ends_that_wake_with_reason_error() {
    let reasons = a_faulted_wake_and_the_one_after_it().await;
    assert_eq!(
        reasons.first().map(String::as_str),
        Some("error"),
        "a contributed section that returns Err ends THAT wake with reason error: {reasons:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_next_wake_after_a_faulted_one_completes() {
    let reasons = a_faulted_wake_and_the_one_after_it().await;
    assert_eq!(
        reasons.len(),
        2,
        "the driver stayed up and opened a second wake: {reasons:?}"
    );
    assert_eq!(
        reasons[1], "completed",
        "the wake after the faulted one completes — the failure ended a wake, not the loop: \
         {reasons:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// A FAILED row is REPORTED and NOT RETRIED.
// ---------------------------------------------------------------------------------------------

/// Boot the shipped tree, then add a row whose `apply` fails through the launcher's LIVE patch
/// path, and hold the result still for long enough that a retry would have shown up.
async fn a_failed_row() -> (usize, Vec<bough_kernel::RowSnapshot>, u32, u32) {
    let (kernel, dir) = boot_real("headless", &[fixture("fault.patch.yml")]).await;

    let reports = Arc::new(AtomicUsize::new(0));
    let sink = reports.clone();
    kernel
        .root()
        .on::<bough_kernel::event::RowsUnresolved, _, _>(move |_| {
            let sink = sink.clone();
            async move {
                sink.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await
        .expect("a listener registers");

    support::write_patch(&dir, &fault_row("apply", "error", 1, 0, &[]));
    support::recompose(&kernel, "", &dir).await.expect(
        "a row that fails to APPLY still composes: it is a runtime failure, not a bad layer",
    );
    let applies_at_failure = bough_plugin_fault_inject::applies();

    // A retry would happen after the recompose returns, so the tree is held still and asked again.
    // Quiescence is the kernel's own "nothing is in flight" answer, which is stronger than a sleep.
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(120)).await;
        kernel.quiesce().await;
    }
    let applies_later = bough_plugin_fault_inject::applies();
    let rows = kernel.snapshot().rows;
    let seen = reports.load(Ordering::SeqCst);
    kernel.shutdown().await;
    (seen, rows, applies_at_failure, applies_later)
}

/// Depth-first, flattened.
fn flatten(rows: &[bough_kernel::RowSnapshot], out: &mut Vec<bough_kernel::RowSnapshot>) {
    for r in rows {
        out.push(r.clone());
        flatten(&r.children, out);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_row_is_reported_once_and_apply_is_never_called_again() {
    let _guard = trace::test_lock();
    let _faults = bough_plugin_fault_inject::test_lock();
    let (reports, rows, at_failure, later) = a_failed_row().await;

    let mut all = Vec::new();
    flatten(&rows, &mut all);
    let fault = all
        .iter()
        .find(|r| r.id.as_str() == "fault")
        .expect("the live patch mounted the row");
    assert_eq!(
        fault.state,
        FiberState::Failed,
        "a row whose `apply` returns Err is FAILED: {:?}",
        fault.error
    );
    assert!(
        fault.error.is_some(),
        "and it names the reason, not merely the row (§0.2)"
    );
    assert_eq!(
        reports, 1,
        "the failure is REPORTED once on `kernel/rows-unresolved`, not repeatedly"
    );
    assert_eq!(at_failure, 1, "`apply` ran once");
    assert_eq!(
        later, at_failure,
        "and was never called again: a FAILED row is reported, not retried into a loop (§7)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_row_leaves_every_other_row_active() {
    let _guard = trace::test_lock();
    let _faults = bough_plugin_fault_inject::test_lock();
    let (_reports, rows, _at_failure, _later) = a_failed_row().await;

    let mut all = Vec::new();
    flatten(&rows, &mut all);
    let casualties: Vec<&bough_kernel::RowSnapshot> = all
        .iter()
        .filter(|r| r.id.as_str() != "fault" && !r.disabled && r.state != FiberState::Active)
        .collect();
    assert!(
        casualties.is_empty(),
        "one FAILED row took others down with it: {:#?}",
        casualties
            .iter()
            .map(|r| (r.id.as_str(), r.state, r.error.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        all.len() > 20,
        "vacuity guard: only {} rows were examined",
        all.len()
    );
}

// ---------------------------------------------------------------------------------------------
// A panicking LISTENER is contained, and the dispatch continues.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_listener_is_contained_and_the_dispatch_continues() {
    let _guard = trace::test_lock();
    let _faults = bough_plugin_fault_inject::test_lock();
    let layer = Layer::new(
        "panic",
        &fault_row("wake_stopping", "panic", 1, 1, &["agents"]),
    );
    let (kernel, _dir) = boot_real("headless", &[fixture("fault.patch.yml"), layer.path()]).await;

    let contained = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = contained.clone();
    kernel
        .root()
        .on::<bough_kernel::event::ListenerFailed, _, _>(move |f| {
            let sink = sink.clone();
            let line = format!("{} in `{}`: {}", f.event, f.entry, f.detail);
            async move {
                sink.lock().expect("not poisoned").push(line);
            }
        })
        .await
        .expect("a listener registers");

    let steps = run_wakes(&kernel, 2).await;
    let reasons = wake_end_reasons(&steps);
    kernel.shutdown().await;

    let seen = contained.lock().expect("not poisoned").clone();
    assert!(
        seen.iter().any(|l| l.contains("agent/wake-stopping")),
        "the panicking listener was contained and REPORTED on `kernel/listener-failed`: {seen:?}"
    );
    assert_eq!(
        reasons.len(),
        2,
        "the dispatch continued and both wakes closed durably: {reasons:?}"
    );
    assert!(
        reasons.iter().all(|r| r == "completed"),
        "a contained listener panic is not the wake's failure: {reasons:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// An LLM failure is a TERMINAL CHUNK, never a thrown error (§12).
// ---------------------------------------------------------------------------------------------

/// `llm-replay` in strict mode with a transcript that matches nothing: every request comes back
/// `Chunk::Failed(BadRequest)` as the stream's ONE terminal chunk.
const NO_ROUNDS: &str = "\
entries:
  llm.anthropic:
    plugin: llm-replay
    config:
      strict: true
      models: \"*\"
      rounds: []
";

#[tokio::test(flavor = "multi_thread")]
async fn an_unmatched_replay_arrives_as_a_terminal_failed_chunk() {
    let _guard = trace::test_lock();
    let layer = Layer::new("strict", NO_ROUNDS);
    let (kernel, _dir) = boot_real("headless", &[layer.path()]).await;

    // The whole point: the wake RETURNS. A thrown error would abort the driver and `when_idle`
    // would never resolve, which `run_wakes` reports as a failure rather than a hang.
    let steps = run_wakes(&kernel, 2).await;
    kernel.shutdown().await;

    let reasons = wake_end_reasons(&steps);
    assert_eq!(
        reasons,
        vec!["error", "error"],
        "an unmatched strict replay ends each wake with reason error: {reasons:?}"
    );
    // And it is durable evidence, not a log line: the wake closed with the failure recorded.
    let ends: Vec<&Step> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "wake/end")
        .collect();
    assert_eq!(ends.len(), 2);
    for e in ends {
        let body = serde_json::to_string(&e.body).expect("serialisable");
        assert!(
            body.contains("error"),
            "the closed wake records the failure: {body}"
        );
    }
}
