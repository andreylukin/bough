//! §10's standing rule, checked where it actually matters: the SPAWNER's write-boundary block is
//! the first thing in the request the adapter receives — not merely the first thing in the string
//! the provider seeded.
//!
//! This lives in the launcher's test target because the assertion needs a whole tree: `agents`,
//! `agent-loop`, `tools`, `workers` and `worker-spawn` mounted together, with `llm-replay` in the
//! adapter's place so it stays hermetic. It replaces the `#[ignore]`d placeholder in
//! `plugins/worker-spawn/tests/roundtrip.rs`, which could not reach a mounted tree from inside
//! its own crate.

mod support;

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
