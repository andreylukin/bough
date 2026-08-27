//! §10's standing rule, checked where it actually matters: the SPAWNER's write-boundary block is
//! the first thing in the request the adapter receives — not merely the first thing in the string
//! the provider seeded.
//!
//! This lives in the launcher's test target because the assertion needs a whole tree: `agents`,
//! `agent-loop`, `tools`, `workers` and `worker-spawn` mounted together, with `llm-replay` in the
//! adapter's place so it stays hermetic. It replaces the `#[ignore]`d placeholder in
//! `plugins/worker-spawn/tests/roundtrip.rs`, which could not reach a mounted tree from inside
//! its own crate.

use crate::support;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{AgentName, StepId, TrajId, WakeId};
use bough_plugin_llm::{LlmContentBlock, LlmMessage};
use bough_plugin_worker_spawn::boundary::WRITE_BOUNDARY;
use bough_plugin_workers::{AskMode, SealSpec, StartWorker, WorkerKind, Workers};
use support::{boot_real, fixture, row_ctx};

#[tokio::test]
async fn the_boundary_block_is_first_in_the_request_the_adapter_receives() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;

    // A row may only `get` what it injected, so each key is taken from a row that declared it:
    // `exec` for `agents`, `tool.spawn_worker` for `workers`.
    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let agents = row_ctx(&kernel, "exec")
        .get::<Agents>()
        .expect("the agents key is bound");
    let workers = ctx.get::<Workers>().expect("the workers key is bound");

    let name = AgentName::new("sol");
    let traj = TrajId::new("lane/sol");
    let (spawner, disposer) = agents
        .create(CreateAgent {
            name: name.clone(),
            traj: traj.clone(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the spawner is created");

    let started = workers
        .start(
            &ctx,
            StartWorker {
                kind: WorkerKind::Spawn,
                spawner: name.clone(),
                spawner_id: spawner.id().clone(),
                wake: WakeId::new("wake:spawn-test"),
                step: StepId::new("step:spawn-test"),
                depth: 1,
                task: "rename `foo` to `bar`".to_string(),
                seal: SealSpec::report(),
                tools: None,
                ask_mode: AskMode::Block,
                at: chrono::Utc::now(),
            },
        )
        .await;
    // The replay transcript answers with prose and never calls `report`, so HOW the run ends is
    // not this test's subject; that a request was sent, and what was first in it, is.
    let _ = started;

    fn text_of(m: &LlmMessage) -> Option<&str> {
        m.content.iter().find_map(|b| match b {
            LlmContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }

    let sent = bough_plugin_agent_loop::invariant::seen();
    let text = sent
        .iter()
        .flat_map(|s| s.request.messages.iter())
        .filter_map(text_of)
        .find(|t| t.contains("rename `foo` to `bar`"))
        .expect("the worker\'s task reached an adapter as a request");
    // The loop wraps delivered mail in its own envelope line (`[mail from …] <subject>`); what
    // §10 rules on is the MAIL'S OWN body, and the boundary is the first thing in it — before the
    // task, not appended after it.
    let body = text
        .split_once('\n')
        .map(|(_envelope, rest)| rest)
        .expect("delivered mail carries an envelope line and a body");
    assert!(
        body.starts_with(WRITE_BOUNDARY),
        "the write-boundary block is first in what the adapter was handed, not merely in the \
         seed: {body:?}"
    );
    assert!(
        body.find(WRITE_BOUNDARY) < body.find("rename `foo` to `bar`"),
        "the boundary precedes the task it bounds: {body:?}"
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}

/// §10's other half of the same rule: the worker's context is TASK-ONLY. The spawner is given a
/// history first — a distinctive message it actually answers, so the string is provably in the
/// spawner's own request — and then a worker is spawned. What the adapter is handed for the
/// worker must be the seeded task and nothing else: no projection of the spawner's transcript.
///
/// The vacuity guard is the first assertion: if the secret never reached a request at all, the
/// "the worker never saw it" half would pass for the wrong reason.
#[tokio::test(flavor = "multi_thread")]
async fn the_worker_context_is_task_only() {
    const SECRET: &str = "PINEAPPLE-ONLY-THE-SPAWNER-KNOWS-THIS";
    const TASK: &str = "count the mangoes in the orchard";

    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;

    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let agents = row_ctx(&kernel, "exec")
        .get::<Agents>()
        .expect("the agents key is bound");
    let workers = ctx.get::<Workers>().expect("the workers key is bound");

    let name = AgentName::new("sol");
    let traj = TrajId::new("lane/sol");
    // The spawner is seeded with the secret as WAKE-class mail, so it runs a wake over it and the
    // string is genuinely part of its transcript rather than merely sitting in an inbox.
    let seed = bough_plugin_agents::Message {
        id: bough_plugin_agents::MessageId::new("m-secret"),
        from: bough_plugin_agents::Sender::Andrey,
        class: bough_plugin_agents::MailClass::Wake,
        text: SECRET.to_string(),
        subject: "a secret".to_string(),
        cites: Vec::new(),
        refs: Default::default(),
        mail_seq: None,
        at: chrono::Utc::now(),
    };
    let (spawner, disposer) = agents
        .create(CreateAgent {
            name: name.clone(),
            traj: traj.clone(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: vec![(seed, bough_plugin_agents::Target::NextWake)],
            at: chrono::Utc::now(),
        })
        .await
        .expect("the spawner is created");
    tokio::time::timeout(std::time::Duration::from_secs(30), spawner.when_idle())
        .await
        .expect("the spawner finishes its first wake");

    let _ = workers
        .start(
            &ctx,
            StartWorker {
                kind: WorkerKind::Spawn,
                spawner: name.clone(),
                spawner_id: spawner.id().clone(),
                wake: WakeId::new("wake:task-only"),
                step: StepId::new("step:task-only"),
                depth: 1,
                task: TASK.to_string(),
                seal: SealSpec::report(),
                tools: None,
                ask_mode: AskMode::Block,
                at: chrono::Utc::now(),
            },
        )
        .await;

    fn all_text(r: &bough_plugin_llm::LlmRequest) -> String {
        let mut s = r.system.clone().unwrap_or_default();
        for m in &r.messages {
            for b in &m.content {
                if let LlmContentBlock::Text { text } = b {
                    s.push('\n');
                    s.push_str(text);
                }
            }
        }
        s
    }

    let sent = bough_plugin_agent_loop::invariant::seen();
    assert!(
        sent.iter().any(|s| all_text(&s.request).contains(SECRET)),
        "vacuity guard: the spawner's own request never carried the secret, so the rest of this \
         test would prove nothing"
    );

    let worker_reqs: Vec<_> = sent
        .iter()
        .filter(|s| all_text(&s.request).contains(TASK))
        .collect();
    assert!(
        !worker_reqs.is_empty(),
        "the worker's task reached an adapter as a request"
    );
    for s in &worker_reqs {
        let text = all_text(&s.request);
        assert!(
            !text.contains(SECRET),
            "the worker was shown the spawner's history: {text:?}"
        );
        assert_eq!(
            s.request.messages.len(),
            1,
            "a task-only context is exactly the seeded task, no projected transcript: {:?}",
            s.request.messages
        );
    }

    // And the durable half: the worker's steps live on its OWN trajectory, never the spawner's.
    let ledger = row_ctx(&kernel, "worker.spawn")
        .get::<bough_plugin_ledger::Ledger>()
        .expect("the ledger key is bound");
    let on_spawners_lane = ledger
        .0
        .steps(&bough_plugin_ledger::StepQuery {
            trajs: vec![traj.clone()],
            ..Default::default()
        })
        .await
        .expect("read back");
    assert!(
        on_spawners_lane
            .iter()
            .all(|s| !serde_json::to_string(&s.body)
                .unwrap_or_default()
                .contains(TASK)),
        "the worker's task never appears on the spawner's own trajectory"
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}

/// §10's `ask()` through the PRODUCTION sink, not the recording one: `AgentsAskSink` delivers a
/// worker's question to the real spawner's real inbox, and every inbox mutation is a durable
/// `inbox/spliced` step keyed by the message id (§2). The recording-sink cases in
/// `plugins/worker-spawn/tests/roundtrip.rs` pin the CALL SHAPE; this one pins that the mail
/// actually lands on the spawner's lane and is durable.
#[tokio::test(flavor = "multi_thread")]
async fn a_workers_question_lands_on_the_spawners_lane_as_a_durable_wake_class_splice() {
    const QUESTION: &str = "which branch should I cut the DURIAN release from?";

    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;

    let agents = row_ctx(&kernel, "exec")
        .get::<Agents>()
        .expect("the agents key is bound");
    let name = AgentName::new("sol");
    let traj = TrajId::new("lane/sol");
    let (spawner, disposer) = agents
        .create(CreateAgent {
            name: name.clone(),
            traj: traj.clone(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the spawner is created");

    let sink = bough_plugin_worker_spawn::AgentsAskSink::new(agents.as_ref().clone());
    let msg = bough_plugin_agents::Message {
        id: bough_plugin_agents::MessageId::new("m-ask-1"),
        from: bough_plugin_agents::Sender::Worker(bough_plugin_workers::WorkerId::new("w-ask")),
        class: bough_plugin_agents::MailClass::Wake,
        text: QUESTION.to_string(),
        subject: "a question".to_string(),
        cites: Vec::new(),
        refs: Default::default(),
        mail_seq: None,
        at: chrono::Utc::now(),
    };
    let delivered =
        <bough_plugin_worker_spawn::AgentsAskSink as bough_plugin_workers::AskSink>::deliver(
            &sink,
            &name,
            msg,
            bough_plugin_agents::Target::NextWake,
            true,
        )
        .await
        .expect("the production sink reaches the spawner");
    assert_eq!(delivered.to_string(), "m-ask-1");

    // The wake flag was set, so the question does not sit unseen: the spawner runs.
    tokio::time::timeout(std::time::Duration::from_secs(30), spawner.when_idle())
        .await
        .expect("the question woke the spawner");

    let ledger = row_ctx(&kernel, "worker.spawn")
        .get::<bough_plugin_ledger::Ledger>()
        .expect("the ledger key is bound");
    let steps = ledger
        .0
        .steps(&bough_plugin_ledger::StepQuery {
            trajs: vec![traj.clone()],
            ..Default::default()
        })
        .await
        .expect("read back");
    let splice = steps
        .iter()
        .filter(|s| s.kind.as_str() == "inbox/spliced")
        .find(|s| s.body.get("message").and_then(|v| v.as_str()) == Some("m-ask-1"))
        .expect("the question is a durable inbox/spliced step keyed by its message id");
    let body = serde_json::to_string(&splice.body).expect("serialisable");
    assert!(body.contains("insert"), "it is an INSERT splice: {body}");
    assert!(
        body.contains(QUESTION),
        "the durable envelope carries the question itself: {body}"
    );
    assert!(
        body.contains("wake"),
        "the splice records the wake flag: {body}"
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}
