//! §17 Phase 2: every invariant this phase introduced reports CLEAN over a scripted session, and a
//! planted violation of each of the three new ones is reported THROUGH THE RUNNER.
//!
//! The plant goes into the invariant's own recorded stream rather than through a fixture flag,
//! because these three checks are about relations the production code is written not to break:
//! there is no config that makes the loop send a side-channel message. Planting at the stream is
//! the honest way to prove the check fires — the pure evaluator is unit-tested in its own crate,
//! and what this file adds is that the RUNNER surfaces it.

mod support;

use bough_kernel::{FiberUid, Kernel};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{StepType, WakeId};
use support::{boot_real, fixture, row_ctx};

async fn scripted_tree() -> (std::sync::Arc<Kernel>, support::TempDir) {
    boot_real(
        "headless",
        &[fixture("loop-scripted.yml"), fixture("llm-replay.yml")],
    )
    .await
}

/// The fiber a row is running on, which is the key every recorded stream is partitioned by.
fn fiber_of(kernel: &Kernel, row: &str) -> FiberUid {
    row_ctx(kernel, row).fiber_uid()
}

fn violation_names(kernel: &Kernel) -> Vec<&'static str> {
    kernel
        .violations()
        .into_iter()
        .map(|v| v.invariant)
        .collect()
}

#[tokio::test]
async fn every_invariant_reports_clean_over_a_scripted_session() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = scripted_tree().await;

    kernel.run_invariants().await;
    assert!(
        kernel.violations().is_empty(),
        "a clean tree must violate nothing: {:#?}",
        kernel.violations()
    );

    // The gate means nothing unless the runner is actually carrying this phase's specs. Naming
    // them is what stops a silently-empty spec set from reading as success.
    for row in [
        "ledger",
        "agents",
        "agent.loop",
        "tools",
        "workers",
        "actions",
    ] {
        assert!(
            support::maybe_row(&kernel, row).is_some(),
            "row `{row}` must be live for its invariant to be collected"
        );
    }

    kernel.shutdown().await;
}

#[tokio::test]
async fn a_planted_status_repeat_is_reported() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = scripted_tree().await;

    use bough_plugin_agents::invariant::{record, Obs};
    use bough_plugin_agents::{AgentId, Status};
    let fiber = fiber_of(&kernel, "agents");
    let agent = AgentId::new("planted");
    record(Obs::Status {
        fiber,
        agent: agent.clone(),
        from: Status::Idle,
        to: Status::Running,
    });
    record(Obs::Status {
        fiber,
        agent,
        from: Status::Running,
        to: Status::Running,
    });

    kernel.run_invariants().await;
    assert!(
        violation_names(&kernel).contains(&"agent_status_never_repeats_and_disposal_is_terminal"),
        "the planted status repeat was not reported: {:#?}",
        kernel.violations()
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn a_planted_side_channel_is_reported() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = scripted_tree().await;

    // A request that reached the adapter for a wake the ledger knows nothing about: exactly what
    // "model-visible ⟺ ledgered" forbids (V4).
    use bough_plugin_agent_loop::invariant::{record, SentRequest};
    use bough_plugin_llm::{CallConfig, LlmRequest};
    record(SentRequest {
        fiber: fiber_of(&kernel, "agent.loop"),
        wake: WakeId::new("wake-that-left-no-steps"),
        step_index: 0,
        request: LlmRequest {
            model: "claude-haiku-4-5-20251001".into(),
            system: Some("a system prefix nobody ledgered".into()),
            system_volatile: None,
            messages: Vec::new(),
            tools: Vec::new(),
            call: CallConfig {
                model: "claude-haiku-4-5-20251001".into(),
                max_tokens: 8192,
                effort: None,
                tool_choice_none: false,
                meta: Default::default(),
            },
        },
    });

    kernel.run_invariants().await;
    assert!(
        violation_names(&kernel).contains(&"every_request_reconstructs_from_the_ledger"),
        "the planted side channel was not reported: {:#?}",
        kernel.violations()
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn a_planted_unpaired_tool_result_is_reported() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = scripted_tree().await;

    use bough_plugin_tools::invariant::{record, Obs};
    record(Obs {
        fiber: fiber_of(&kernel, "tools"),
        wake: WakeId::new("w-planted"),
        kind: StepType::new("tool/result"),
        call: "call-with-no-call-step".into(),
        step_index: 0,
    });

    kernel.run_invariants().await;
    assert!(
        violation_names(&kernel).contains(&"tool_calls_and_results_pair_within_a_step"),
        "the planted unpaired result was not reported: {:#?}",
        kernel.violations()
    );

    kernel.shutdown().await;
}

// ---------------------------------------------------------------------------
// P2-D24: the measurement that decides Phase 0's open item 1
// ---------------------------------------------------------------------------

/// Inbox receipt → the wake's first `request/header`, measured over the live loop with the model
/// replayed (P2-D24).
///
/// Phase 0 left the fiber lifecycle a 20ms/5ms/1ms poll loop. A wake is not a fiber transition, so
/// the expectation is that the poll does not show up here at all; if this says otherwise, the
/// notify rewrite lands and P2-D24 is rewritten with the number.
///
/// STATUS (integration): it produces a number — p50 ~17ms, high ~520ms on the build machine, over
/// `llm-replay`. Read it honestly: the sample is receipt → the agent is idle again, i.e. the WHOLE
/// wake including the replayed round, not receipt → the first header alone (the assertion below
/// only proves a header was appended). At that scale the kernel's 20ms fiber poll is not the
/// dominant term and Phase 0's open item 1 does not become urgent here; a tighter figure needs a
/// probe on the header append itself, which is the measurement to build before rewriting the poll.
#[tokio::test]
#[ignore = "bench: needs BOUGH_BENCH=1 (make bench); see the STATUS note above"]
async fn wake_latency_from_receipt_to_first_request() {
    if std::env::var("BOUGH_BENCH").as_deref() != Ok("1") {
        eprintln!("BOUGH_BENCH is not 1; skipping");
        return;
    }
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;

    use bough_plugin_agents::{
        AgentKind, Agents, CreateAgent, MailClass, Message, MessageId, Sender,
    };
    let ctx = row_ctx(&kernel, "exec");
    let agents = ctx.get::<Agents>().unwrap();
    let ledger = ctx.get::<bough_plugin_ledger::Ledger>().unwrap();
    let traj = bough_plugin_ledger::TrajId::new("lane/sol");
    // The observable "the loop is about to call the adapter" is the `request/header` step: §5
    // appends it BEFORE the call, so counting them measures the harness and nothing downstream.
    let headers = || {
        let ledger = ledger.clone();
        let traj = traj.clone();
        async move {
            ledger
                .0
                .steps(&bough_plugin_ledger::query::StepQuery {
                    trajs: vec![traj],
                    ..Default::default()
                })
                .await
                .unwrap()
                .iter()
                .filter(|s| s.kind.as_str() == "request/header")
                .count()
        }
    };
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: bough_plugin_ledger::AgentName::new("sol"),
            traj: traj.clone(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .unwrap();

    // One iteration per recorded round in `llm-replay.yml`: a run that outlives its transcript
    // would measure the failure path instead.
    const N: usize = 4;
    let mut samples = Vec::with_capacity(N);
    for i in 0..N {
        let before = headers().await;
        let t0 = std::time::Instant::now();
        agent
            .followup(Message {
                id: MessageId::new(format!("bench-{i}")),
                from: Sender::Andrey,
                class: MailClass::Wake,
                text: format!("bench {i}"),
                subject: "bench".into(),
                cites: Vec::new(),
                refs: Default::default(),
                mail_seq: None,
                at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), agent.when_idle())
            .await
            .expect("the agent goes idle between samples");
        assert!(
            headers().await > before,
            "the wake must have sent at least one request"
        );
        samples.push(t0.elapsed());
    }
    samples.sort();
    println!(
        "wake latency (receipt → first request/header): p50 {:?}, high {:?}",
        samples[N / 2],
        samples[N - 1]
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}
