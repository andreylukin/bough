//! V3: the standing write-boundary block is injected on BOTH paths from ONE source, asserted on
//! the requests the ADAPTER actually received — not on a registry, not on a rendered projection
//! in isolation.
//!
//! The tree is the shipped `profiles/` + `bundles/` with `llm-replay` swapped in for the
//! Anthropic adapter, so `boundary-instructions` mounts exactly as Andrey ships it. Two paths are
//! exercised in one boot:
//!
//! 1. a resident agent's wake, and
//! 2. a worker spawned through the `workers` seam.
//!
//! `worker-fork` does not exist on this branch, so the fork arm is not claimed; the test asserts
//! that absence explicitly so the day a fork provider lands, this file is the thing that fails.

mod support;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent};
use bough_plugin_boundary_instructions::BOUNDARY_BLOCK;
use bough_plugin_hello::trace;
use bough_plugin_ledger::{AgentName, StepId, TrajId, WakeId};
use bough_plugin_llm::{LlmContentBlock, LlmMessage, LlmRequest};
use bough_plugin_worker_spawn::boundary::WRITE_BOUNDARY;
use bough_plugin_workers::{AskMode, SealSpec, StartWorker, WorkerKind, Workers};
use support::{boot_real, fixture, row_ctx};

const TASK: &str = "count the mangoes in the orchard";

fn text_of(m: &LlmMessage) -> Option<&str> {
    m.content.iter().find_map(|b| match b {
        LlmContentBlock::Text { text } => Some(text.as_str()),
        _ => None,
    })
}

fn all_text(r: &LlmRequest) -> String {
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

/// The exact bytes of the boundary as they sit in ONE request's system prefix, sliced back out of
/// the request rather than compared with `contains` alone: the slice is what the two paths are
/// then compared to each other by.
fn boundary_slice_of(r: &LlmRequest) -> String {
    let system = r
        .system
        .as_deref()
        .expect("the loop puts the projection in the stable system prefix");
    let at = system.find(BOUNDARY_BLOCK).unwrap_or_else(|| {
        panic!("the boundary block is not in the system prefix the adapter received:\n{system}")
    });
    system[at..at + BOUNDARY_BLOCK.len()].to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_boundary_block_reaches_the_adapter_on_both_paths_with_identical_bytes() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;

    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let agents = row_ctx(&kernel, "exec")
        .get::<Agents>()
        .expect("the agents key is bound");
    let workers = ctx.get::<Workers>().expect("the workers key is bound");

    let name = AgentName::new("sol");
    let traj = TrajId::new("lane/sol");
    // WAKE-class seed mail, so the resident actually runs a wake and a request is genuinely sent.
    let seed = bough_plugin_agents::Message {
        id: bough_plugin_agents::MessageId::new("m-v3"),
        from: bough_plugin_agents::Sender::Andrey,
        class: bough_plugin_agents::MailClass::Wake,
        text: "PAPAYA-THE-RESIDENTS-OWN-WAKE".to_string(),
        subject: "a question".to_string(),
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
        .expect("the resident finishes its first wake");

    let _ = workers
        .start(
            &ctx,
            StartWorker {
                kind: WorkerKind::Spawn,
                spawner: name.clone(),
                spawner_id: spawner.id().clone(),
                wake: WakeId::new("wake:v3"),
                step: StepId::new("step:v3"),
                depth: 1,
                task: TASK.to_string(),
                seal: SealSpec::report(),
                tools: None,
                ask_mode: AskMode::Block,
                at: chrono::Utc::now(),
            },
        )
        .await;

    let sent = bough_plugin_agent_loop::invariant::seen();
    assert!(
        !sent.is_empty(),
        "vacuity guard: no request reached an adapter at all"
    );

    // --- path 1: the resident's own wake -------------------------------------------------
    let resident = sent
        .iter()
        .map(|s| &s.request)
        .find(|r| all_text(r).contains("PAPAYA-THE-RESIDENTS-OWN-WAKE"))
        .expect("the resident's wake reached an adapter as a request");
    let from_resident = boundary_slice_of(resident);

    // --- path 2: the spawned worker ------------------------------------------------------
    let worker = sent
        .iter()
        .map(|s| &s.request)
        .find(|r| {
            r.messages
                .iter()
                .filter_map(text_of)
                .any(|t| t.contains(TASK))
        })
        .expect("the spawned worker's task reached an adapter as a request");
    let from_worker = boundary_slice_of(worker);

    // ONE source: both slices are the const, and each other, byte for byte.
    assert_eq!(
        from_resident, BOUNDARY_BLOCK,
        "the resident's request does not carry the const verbatim"
    );
    assert_eq!(
        from_worker, BOUNDARY_BLOCK,
        "the worker's request does not carry the const verbatim"
    );
    assert_eq!(
        from_resident, from_worker,
        "the two paths carry DIFFERENT boundary text"
    );

    // And the spawner's prepended block is in the worker's request too, ahead of the task: the
    // second injection path §10 owns. It is a second, worker-framed statement of the same
    // refusals until the merge folds it onto BOUNDARY_BLOCK (P6-D3) -- so this is a `contains`,
    // and the byte-identity claim above is about the ONE const both paths read.
    let body = worker
        .messages
        .iter()
        .filter_map(text_of)
        .find(|t| t.contains(TASK))
        .expect("the seeded task is a message");
    assert!(
        body.find(WRITE_BOUNDARY) < body.find(TASK),
        "the spawner's block precedes the task it bounds: {body:?}"
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}

/// The fork arm of V3, stated rather than silently skipped: no fork provider exists on this
/// branch, so there is no third path to assert on. When one lands, this fails and the test above
/// grows an arm.
#[test]
fn no_fork_path_exists_to_assert_on_yet() {
    assert!(
        !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/worker-fork")
            .exists(),
        "a worker-fork row now exists: V3's third arm must be added to the test above"
    );
}
