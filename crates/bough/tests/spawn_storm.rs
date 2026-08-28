//! V4's fan-out half (WP-5, §10): a spawn storm is held by the bounds, and every refusal REACHES
//! THE MODEL as a `tool/result` failure rather than a silent no-op.
//!
//! Every case runs against the SHIPPED `bough-base` bounds (`max_in_flight: 8`, `max_depth: 3`,
//! `per_wake_spawn_cap: 4`) — read from the mounted row, never restated here, so a bundle that
//! changes a bound changes what this file asserts instead of quietly disagreeing with it.
//!
//! The worker Provider is this file's own: `worker-spawn` starts a whole nested agent per run,
//! which would make a fifty-deep storm a fifty-deep LLM replay and would prove the transcript
//! rather than the bounds. `WorkersHandle::provider_for` takes the LAST registration, so
//! registering one here shadows the shipped row for the duration of the test and the BOUNDS —
//! which live in the Definition, not in any Provider — are what is left under test.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent, MailClass, Message, MessageId, Sender};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{AgentName, Ledger, StepId, TrajId, WakeId};
use bough_plugin_workers::{
    AskMode, SealSpec, StartWorker, WorkerError, WorkerKind, WorkerOutcome, WorkerProvider,
    WorkerResult, WorkerRun, Workers,
};
use support::{boot_real, fixture, row_ctx};

/// A Provider that starts and finishes at once. It exists so the bounds are the only thing that
/// can refuse a start.
struct InstantProvider {
    /// How long a run occupies its in-flight slot. `0` for the counting cases; non-zero for the
    /// storm, where an overlap has to be possible for the bound to mean anything.
    hold: Duration,
    started: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl WorkerProvider for InstantProvider {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Spawn, WorkerKind::Fork]
    }

    async fn start(
        &self,
        _req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        if !self.hold.is_zero() {
            tokio::time::sleep(self.hold).await;
        }
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

/// One spawner agent, created through the real `agents` seam.
async fn spawner(
    kernel: &bough_kernel::Kernel,
    name: &str,
) -> (
    bough_plugin_agents::Agent,
    bough_plugin_agents::AgentDisposer,
) {
    let agents = row_ctx(kernel, "exec")
        .get::<Agents>()
        .expect("the agents key is bound");
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new(name),
            traj: TrajId::new(format!("lane/{name}")),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the spawner is created");
    (agent, disposer)
}

fn request(
    spawner: &str,
    spawner_id: &bough_plugin_agents::AgentId,
    wake: &str,
    depth: u8,
) -> StartWorker {
    StartWorker {
        kind: WorkerKind::Spawn,
        spawner: AgentName::new(spawner),
        spawner_id: spawner_id.clone(),
        wake: WakeId::new(wake),
        step: StepId::new(format!("{wake}#0")),
        depth,
        task: "count something small".to_string(),
        seal: SealSpec::report(),
        tools: None,
        ask_mode: AskMode::Block,
        at: chrono::Utc::now(),
    }
}

/// The bound a refusal names, or `None` if it was not a bound refusal at all.
fn bound_of(e: &WorkerError) -> Option<&'static str> {
    match e {
        WorkerError::BoundsExceeded { bound, .. } => Some(bound),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fifty_spawns_in_one_wake_stop_at_the_per_wake_cap() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;
    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let workers = ctx.get::<Workers>().expect("the workers key is bound");
    let cap = workers.bounds().per_wake_spawn_cap;

    let started = Arc::new(AtomicUsize::new(0));
    workers
        .provider(
            &ctx,
            Arc::new(InstantProvider {
                hold: Duration::ZERO,
                started: started.clone(),
            }),
        )
        .await
        .expect("a test Provider registers like any other");

    let (agent, disposer) = spawner(&kernel, "sol").await;

    let mut refused: Vec<&'static str> = Vec::new();
    let mut ok = 0usize;
    for _ in 0..50 {
        match workers
            .start(&ctx, request("sol", agent.id(), "wake:storm", 1))
            .await
        {
            Ok(_) => ok += 1,
            Err(e) => refused.push(bound_of(&e).unwrap_or_else(|| {
                panic!("a storm must be refused by a BOUND, not by anything else: {e}")
            })),
        }
    }

    assert_eq!(ok, cap, "exactly `per_wake_spawn_cap` starts are admitted");
    assert_eq!(
        started.load(Ordering::SeqCst),
        cap,
        "and exactly that many reached a Provider: a refusal never runs"
    );
    assert_eq!(refused.len(), 50 - cap, "every other request is refused");
    assert!(
        refused.iter().all(|b| *b == "per_wake_spawn_cap"),
        "and each refusal names the per-wake cap: {refused:?}"
    );
    assert_eq!(
        workers.spawned_in_wake(&WakeId::new("wake:storm")),
        cap,
        "the wake's counter stopped at the cap rather than counting refusals"
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn in_flight_never_exceeds_max_in_flight_under_a_three_agent_storm() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;
    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let workers = ctx.get::<Workers>().expect("the workers key is bound");
    let bounds = workers.bounds();

    let started = Arc::new(AtomicUsize::new(0));
    workers
        .provider(
            &ctx,
            Arc::new(InstantProvider {
                // Long enough that starts genuinely overlap: a bound nobody ever approaches is
                // not proven by anything.
                hold: Duration::from_millis(60),
                started: started.clone(),
            }),
        )
        .await
        .expect("a test Provider registers");

    let mut spawners = Vec::new();
    for name in ["sol", "terra", "luna"] {
        spawners.push((name, spawner(&kernel, name).await));
    }

    // The sampler reads the SAME counter the bound is enforced against, on its own task, for the
    // whole storm. A peak it never sees is a peak the assertion cannot claim, which is why the
    // observed maximum is asserted as a floor too.
    let peak = Arc::new(AtomicUsize::new(0));
    let sampling = Arc::new(tokio::sync::Notify::new());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let workers = workers.clone();
        let peak = peak.clone();
        let stop = stop.clone();
        let sampling = sampling.clone();
        tokio::spawn(async move {
            sampling.notify_one();
            while !stop.load(Ordering::SeqCst) {
                let now = workers.in_flight();
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    };
    sampling.notified().await;

    let mut tasks = Vec::new();
    for (name, (agent, _)) in &spawners {
        for w in 0..3 {
            for _ in 0..bounds.per_wake_spawn_cap {
                let workers = workers.clone();
                let ctx = ctx.clone();
                let req = request(name, agent.id(), &format!("wake:{name}-{w}"), 1);
                tasks.push(tokio::spawn(async move {
                    workers.start(&ctx, req).await.map_err(|e| e.to_string())
                }));
            }
        }
    }
    let mut refused_in_flight = 0usize;
    for t in tasks {
        if let Err(e) = t.await.expect("no task panicked") {
            if e.contains("max_in_flight") {
                refused_in_flight += 1;
            }
        }
    }
    stop.store(true, Ordering::SeqCst);
    let _ = sampler.await;

    let observed = peak.load(Ordering::SeqCst);
    assert!(
        observed <= bounds.max_in_flight,
        "the storm ran {observed} workers at once; max_in_flight is {}",
        bounds.max_in_flight
    );
    assert!(
        observed > 1 || refused_in_flight > 0,
        "vacuity guard: the storm never overlapped and never hit the bound, so the ceiling was \
         not exercised (peak {observed})"
    );

    for (_, (_, disposer)) in spawners {
        disposer.dispose().await;
    }
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_depth_four_spawn_is_refused() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;
    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let workers = ctx.get::<Workers>().expect("the workers key is bound");
    let started = Arc::new(AtomicUsize::new(0));
    workers
        .provider(
            &ctx,
            Arc::new(InstantProvider {
                hold: Duration::ZERO,
                started: started.clone(),
            }),
        )
        .await
        .expect("a test Provider registers");
    let (agent, disposer) = spawner(&kernel, "sol").await;

    let depth = workers.bounds().max_depth + 1;
    let err = workers
        .start(&ctx, request("sol", agent.id(), "wake:deep", depth))
        .await
        .expect_err("a spawn past `max_depth` is refused");
    assert_eq!(
        bound_of(&err),
        Some("max_depth"),
        "the refusal names the depth bound: {err}"
    );
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "depth is checked BEFORE any Provider runs, so nothing was started"
    );
    // And the generation below it is admitted, so the refusal is a bound and not a blanket no.
    workers
        .start(&ctx, request("sol", agent.id(), "wake:deep-ok", depth - 1))
        .await
        .expect("the last permitted generation still spawns");

    disposer.dispose().await;
    kernel.shutdown().await;
}

/// The transcript for the refusal case: one wake in which the model asks for five workers at
/// once, then answers. Written to a file because a `--patch` layer is a FILE (§0.5), and inline
/// because it is one test's fixture and nothing else's.
const FIVE_SPAWNS: &str = r#"
entries:
  # A GAP this test found, patched here rather than in a crate this work package may not edit:
  # `spawn_worker` runs on the LOOP's context (`ToolCx.ctx`), and `agent-loop` does not declare
  # `workers` in its static inject — so on the SHIPPED tree every spawn_worker call comes back
  # `workers seam unavailable: … read service `workers` without declaring it in inject`. The row
  # patch below adds the key the way §0.3 allows an entry to (entry ∪ plugin-static); the fix
  # belongs in `bough-base` or in `agent-loop::inject`, and is written up as hook H-C5 in
  # docs/track-c-merge-notes.md.
  agent.loop:
    inject:
      optional: [workers]
  llm.anthropic:
    plugin: llm-replay
    config:
      strict: false
      models: "*"
      rounds:
        - chunks:
            - { type: tool_call, id: "s1", name: "spawn_worker", input: { task: "one" } }
            - { type: tool_call, id: "s2", name: "spawn_worker", input: { task: "two" } }
            - { type: tool_call, id: "s3", name: "spawn_worker", input: { task: "three" } }
            - { type: tool_call, id: "s4", name: "spawn_worker", input: { task: "four" } }
            - { type: tool_call, id: "s5", name: "spawn_worker", input: { task: "five" } }
            - { type: end, stop: tool_use }
        - chunks:
            - { type: text, text: "four of five ran" }
            - { type: end, stop: end_turn }
"#;

#[tokio::test(flavor = "multi_thread")]
async fn every_refusal_reaches_the_model_as_a_tool_result_failure() {
    let _guard = trace::test_lock();
    let patch = std::env::temp_dir().join(format!("bough-storm-{}.yml", std::process::id()));
    std::fs::write(&patch, FIVE_SPAWNS).expect("the patch layer is writable");

    let (kernel, _dir) = boot_real("headless", std::slice::from_ref(&patch)).await;
    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let workers = ctx.get::<Workers>().expect("the workers key is bound");
    let cap = workers.bounds().per_wake_spawn_cap;
    let started = Arc::new(AtomicUsize::new(0));
    workers
        .provider(
            &ctx,
            Arc::new(InstantProvider {
                hold: Duration::ZERO,
                started: started.clone(),
            }),
        )
        .await
        .expect("a test Provider registers");

    let (agent, disposer) = spawner(&kernel, "sol").await;
    agent
        .followup(Message {
            id: MessageId::new("m-storm"),
            from: Sender::Andrey,
            class: MailClass::Wake,
            text: "spawn five workers".to_string(),
            subject: "five".to_string(),
            cites: Vec::new(),
            refs: Default::default(),
            mail_seq: None,
            at: chrono::Utc::now(),
        })
        .await
        .expect("mail lands");
    tokio::time::timeout(Duration::from_secs(30), agent.when_idle())
        .await
        .expect("the wake finishes");

    assert_eq!(
        started.load(Ordering::SeqCst),
        cap,
        "the model asked for five and the cap admitted `per_wake_spawn_cap`"
    );

    // The DURABLE half: the refusal is a `tool/result` step whose outcome is a failure naming the
    // bound — not a missing step and not a silent success.
    let ledger = row_ctx(&kernel, "exec")
        .get::<Ledger>()
        .expect("the ledger key is bound");
    let steps = ledger
        .0
        .steps(&bough_plugin_ledger::StepQuery {
            trajs: vec![TrajId::new("lane/sol")],
            kinds: vec![bough_plugin_ledger::StepType::new("tool/result")],
            ..Default::default()
        })
        .await
        .expect("the chain reads back");
    assert_eq!(
        steps.len(),
        5,
        "five calls, five results: a refusal is answered, never dropped"
    );
    let failures: Vec<&bough_plugin_ledger::Step> =
        steps.iter().filter(|s| s.body["outcome"] != "ok").collect();
    assert_eq!(
        failures.len(),
        5 - cap,
        "exactly the refused calls carry a failing outcome: {:#?}",
        steps.iter().map(|s| &s.body).collect::<Vec<_>>()
    );
    for f in &failures {
        let body = serde_json::to_string(&f.body).expect("serialisable");
        assert!(
            body.contains("per_wake_spawn_cap"),
            "the model is told WHICH bound refused it: {body}"
        );
    }

    // And the model-visible half: the failure text is in the request the next step sent.
    let sent = bough_plugin_agent_loop::invariant::seen();
    assert!(
        sent.iter().any(|s| {
            serde_json::to_string(&s.request.messages)
                .unwrap_or_default()
                .contains("per_wake_spawn_cap")
        }),
        "the refusal reached the MODEL, not only the ledger"
    );

    disposer.dispose().await;
    kernel.shutdown().await;
    let _ = std::fs::remove_file(&patch);
}
