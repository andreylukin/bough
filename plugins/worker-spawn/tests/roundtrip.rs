//! §10, the spawn roundtrip. What this file pins:
//!
//! * the boundary block is FIRST in what the worker is seeded with, and the task follows it;
//! * a report that matches the seal is accepted and one that does not is `SealInvalid`;
//! * the result lands in the SPAWNER's chain as `worker/report` carrying the report's EXTERNAL
//!   cites, and a claim citing only the worker's own report lands as `worker/claim` (Thought);
//! * `ask()` reaches the spawner's lane as WAKE-class mail targeted at the next wake with the
//!   wake flag set, and then blocks (mode `block`) or ends the worker (mode `end`).
//!
//! The one assertion this file cannot yet make offline is the one on the recorded `LlmRequest`:
//! it needs a mounted `agents` + `agent-loop-scripted` + `llm-replay`, which are other work
//! packages of this same phase. That test is `#[ignore]`d below with the reason, and the pure
//! seeding rule is pinned in `plugins/worker-spawn/src/lib.rs` in the meantime.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{AgentId, MailClass, Target};
use bough_plugin_ledger::{
    AgentName, AgentRow, Cite, Class, LedgerHandle, Ref, StepId, StepQuery, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_worker_spawn::{RecordingAskSink, SpawnProvider, WRITE_BOUNDARY};
use bough_plugin_workers::{
    AskAnswer, AskMode, Bounds, Report, ReportClaim, SealSpec, StartWorker, WorkerError,
    WorkerKind, WorkersHandle,
};

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

fn cite(r: &str) -> Cite {
    Cite {
        r#ref: Ref::new(r),
        url: None,
    }
}

async fn ledger_with_spawner(ctx: &Context, spawner: &AgentName, traj: &TrajId) -> LedgerHandle {
    let h = LedgerHandle(MemoryStore::new(ctx.clone()));
    for def in bough_plugin_workers::vocabulary::step_types() {
        h.0.register_step_type(def).expect("fresh step types");
    }
    h.0.put_agent(AgentRow {
        name: spawner.clone(),
        traj: traj.clone(),
        routing_refs: Default::default(),
        wake_classes: Default::default(),
        model_override: None,
        tick_floor: None,
        digest_rollup: None,
    })
    .await
    .expect("the spawner has a row");
    h
}

fn req(spawner: &AgentName, wake: &str) -> StartWorker {
    StartWorker {
        kind: WorkerKind::Spawn,
        spawner: spawner.clone(),
        spawner_id: AgentId::new("a1"),
        wake: WakeId::new(wake),
        step: StepId::new("s0"),
        depth: 1,
        task: "rename `foo` to `bar`".into(),
        seal: SealSpec::report(),
        tools: None,
        ask_mode: AskMode::End,
        at: chrono::Utc::now(),
    }
}

fn report() -> Report {
    Report {
        summary: "renamed it".into(),
        claims: vec![
            ReportClaim {
                text: "src/lib.rs line 3 now reads `bar`".into(),
                cites: vec![cite("step:evidence-1")],
            },
            ReportClaim {
                text: "nothing else refers to `foo`".into(),
                cites: vec![],
            },
        ],
    }
}

// ---------------------------------------------------------------------------------------------
// the seal
// ---------------------------------------------------------------------------------------------

#[test]
fn a_scripted_workers_report_validates_against_the_seal() {
    let seal = SealSpec::report();
    let body = serde_json::to_value(report()).unwrap();
    seal.validate(&body)
        .expect("a well-formed report validates");
    let round: Report = serde_json::from_value(body).expect("and round-trips");
    assert_eq!(round, report());
}

#[test]
fn an_invalid_report_is_seal_invalid_naming_the_seal_and_the_pointer() {
    let seal = SealSpec::report();
    let detail = seal
        .validate(&serde_json::json!({ "claims": [] }))
        .expect_err("a report with no summary is not a report");
    let err = WorkerError::SealInvalid {
        seal: seal.name.clone(),
        detail,
    };
    let text = err.to_string();
    assert!(text.contains("worker.report"), "{text}");
    assert!(text.contains("summary"), "{text}");
}

// ---------------------------------------------------------------------------------------------
// the spawner's chain
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_report_lands_in_the_spawners_chain_with_the_external_cites() {
    let ctx = ctx();
    let (spawner, traj) = (AgentName::new("sol"), TrajId::new("sol-traj"));
    let ledger = ledger_with_spawner(&ctx, &spawner, &traj).await;
    let worker = bough_plugin_workers::WorkerId::new("w1");

    let step = bough_plugin_worker_spawn::land_in_spawners_chain(
        &ledger,
        &req(&spawner, "wk1"),
        &worker,
        &report(),
    )
    .await
    .expect("the chain accepts the report")
    .expect("and answers with the report's step id");

    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            ..Default::default()
        })
        .await
        .expect("read back");
    let kinds: Vec<&str> = steps.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, vec!["worker/report", "worker/claim"]);

    let report_step = &steps[0];
    assert_eq!(
        report_step.id, step,
        "the returned id names the report step"
    );
    assert_eq!(
        report_step.class,
        Class::Evidence,
        "a report with an external cite is EVIDENCE"
    );
    assert_eq!(
        report_step.cites.as_ref(),
        &vec![cite("step:evidence-1")],
        "only the EXTERNAL cites travel"
    );
    assert_eq!(report_step.body["summary"], "renamed it");
    assert_eq!(report_step.wake.as_str(), "wk1");
}

#[tokio::test]
async fn a_claim_citing_only_the_workers_own_report_lands_as_a_thought() {
    let ctx = ctx();
    let (spawner, traj) = (AgentName::new("sol"), TrajId::new("sol-traj"));
    let ledger = ledger_with_spawner(&ctx, &spawner, &traj).await;
    let worker = bough_plugin_workers::WorkerId::new("w1");

    let r = Report {
        summary: "did it".into(),
        claims: vec![
            ReportClaim {
                text: "the test suite passes".into(),
                cites: vec![cite("step:run-1")],
            },
            ReportClaim {
                text: "I am confident this is correct".into(),
                // Its ONLY citation is the worker's own report.
                cites: vec![cite(&format!("worker:{worker}"))],
            },
        ],
    };
    bough_plugin_worker_spawn::land_in_spawners_chain(&ledger, &req(&spawner, "wk1"), &worker, &r)
        .await
        .expect("lands");

    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj],
            ..Default::default()
        })
        .await
        .expect("read back");
    let claim = steps
        .iter()
        .find(|s| s.kind.as_str() == "worker/claim")
        .expect("the self-cited claim is recorded");
    assert_eq!(
        claim.class,
        Class::Thought,
        "a self-cited claim is a THOUGHT"
    );
    assert_eq!(claim.body["text"], "I am confident this is correct");
    assert!(claim.cites.is_empty());
    // And the cited one did NOT become a second thought.
    assert_eq!(
        steps
            .iter()
            .filter(|s| s.kind.as_str() == "worker/claim")
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------------------------
// ask()
// ---------------------------------------------------------------------------------------------

async fn run_that_asks(
    mode: AskMode,
    reply: Option<String>,
) -> (bough_plugin_workers::WorkerRun, Arc<RecordingAskSink>) {
    let ctx = ctx();
    let h = WorkersHandle::new(Bounds {
        max_in_flight: 4,
        max_depth: 3,
        per_wake_spawn_cap: 4,
    });
    let sink = Arc::new(RecordingAskSink::new(reply));
    h.ask_sink(&ctx, sink.clone()).await.expect("sink installs");
    // Reach the run the way a provider does: start one and hold it while it asks.
    let run = bough_plugin_workers::WorkerRun::for_test(
        bough_plugin_workers::WorkerId::new("w1"),
        AgentName::new("sol"),
        mode,
        sink.clone(),
    );
    (run, sink)
}

#[tokio::test]
async fn ask_appears_as_wake_class_mail_on_the_spawners_lane() {
    let (run, sink) = run_that_asks(AskMode::End, None).await;
    run.ask("which branch?".into()).await.expect("delivered");

    let delivered = sink.delivered.lock().clone();
    assert_eq!(delivered.len(), 1, "exactly one splice");
    let (to, msg, target, wake) = &delivered[0];
    assert_eq!(to.as_str(), "sol", "it goes to the SPAWNER");
    assert_eq!(msg.class, MailClass::Wake, "a question is WAKE-class mail");
    assert_eq!(*target, Target::NextWake);
    assert!(
        *wake,
        "and it asks for a wake: an unseen question is not a question"
    );
    assert_eq!(msg.text, "which branch?");
    assert!(
        matches!(&msg.from, bough_plugin_agents::Sender::Worker(w) if w.as_str() == "w1"),
        "the sender is the worker"
    );
}

#[tokio::test]
async fn ask_in_block_mode_waits_for_the_answer() {
    let (run, _) = run_that_asks(AskMode::Block, Some("the release branch".into())).await;
    assert_eq!(
        run.ask("which branch?".into()).await.expect("answered"),
        AskAnswer::Answered("the release branch".into())
    );
    assert_eq!(run.asked().expect("recorded").question, "which branch?");
}

#[tokio::test]
async fn ask_in_end_mode_ends_the_worker_without_waiting() {
    // The sink WOULD answer; `end` mode never asks it to.
    let (run, _) = run_that_asks(AskMode::End, Some("the release branch".into())).await;
    assert_eq!(
        run.ask("which branch?".into()).await.expect("delivered"),
        AskAnswer::Ended
    );
    assert_eq!(run.asked().expect("recorded").question, "which branch?");
}

/// A blocking worker whose spawner will never answer ends rather than hanging.
#[tokio::test]
async fn a_block_mode_ask_with_no_answer_ends_the_worker() {
    let (run, _) = run_that_asks(AskMode::Block, None).await;
    assert_eq!(
        run.ask("which branch?".into()).await.expect("delivered"),
        AskAnswer::Ended
    );
}

// ---------------------------------------------------------------------------------------------
// the seeded request
// ---------------------------------------------------------------------------------------------

/// The pure half: the boundary block is first in what the spawner seeds.
#[test]
fn the_seeded_task_begins_with_the_boundary_block() {
    let seeded = SpawnProvider::seed_task("rename `foo` to `bar`");
    assert!(seeded.starts_with(WRITE_BOUNDARY));
    assert!(seeded.ends_with("rename `foo` to `bar`"));
}

// The assertion §10 actually wants — the block reaches the ADAPTER, not merely the seed — needs
// `agents`, `agent-loop`, `tools`, `workers` and `worker-spawn` mounted together with a replay
// adapter, which no test inside this crate can reach. It lives in the launcher's test target as
// `crates/bough/tests/worker_spawn.rs::the_boundary_block_is_first_in_the_request_the_adapter_receives`.
