//! §17 Phase 2's live check for the spawn roundtrip: a REAL haiku worker is given a file in a
//! tempdir, told to edit it, and reports back — and the FILE'S CONTENT on disk, not the worker's
//! prose, is what proves it happened.
//!
//! `#[ignore]` + `BOUGH_LIVE=1` (AGENTS.md): the suite stays hermetic without a key. `make live`
//! sources `~/.bough/env` and runs it.
//!
//! It lives in the launcher's test target because it needs the whole headless composition;
//! `worker-spawn` cannot mount a tree from inside its own crate.

mod support;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{AgentName, StepId, TrajId, WakeId};
use bough_plugin_workers::{AskMode, SealSpec, StartWorker, WorkerKind, WorkerOutcome, Workers};
use support::{boot_real, row_ctx};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "BOUGH_LIVE=1: a real claude-haiku-4-5 round"]
async fn a_real_worker_edits_a_file_and_its_content_proves_it() {
    if std::env::var("BOUGH_LIVE").ok().as_deref() != Some("1") {
        eprintln!("BOUGH_LIVE is not 1: skipping");
        return;
    }
    let _guard = trace::test_lock();

    let work = support::TempDir::new("worker-live");
    let file = work.path().join("greeting.txt");
    std::fs::write(&file, "hello world\n").expect("seed the file");

    // The worker may only write inside the task's tree: `tools.baseline.root` IS that boundary,
    // so the patch is the security-relevant half of this test as much as the model call is.
    let patch = work.path().join("live.patch.yml");
    std::fs::write(
        &patch,
        format!(
            // `config` is REPLACED wholesale (§0.5), so every field is restated.
            "entries:\n  tools.baseline:\n    config:\n      root: {}\n      \
             bash_timeout_ms: 120000\n      max_output_bytes: 20000\n      \
             max_read_bytes: 400000\n      deny_globs: []\n",
            work.path().display()
        ),
    )
    .expect("the patch is writable");

    let (kernel, _dir) = boot_real("headless", std::slice::from_ref(&patch)).await;
    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let agents = row_ctx(&kernel, "exec")
        .get::<Agents>()
        .expect("the agents key is bound");
    let workers = ctx.get::<Workers>().expect("the workers key is bound");

    let name = AgentName::new("sol");
    let (spawner, disposer) = agents
        .create(CreateAgent {
            name: name.clone(),
            traj: TrajId::new("lane/sol"),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the spawner is created");

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        workers.start(
            &ctx,
            StartWorker {
                kind: WorkerKind::Spawn,
                spawner: name.clone(),
                spawner_id: spawner.id().clone(),
                wake: WakeId::new("wake:live-spawn"),
                step: StepId::new("step:live-spawn"),
                depth: 1,
                task: "In greeting.txt, replace the word `world` with `bough`. Then call the \
                       `report` tool once."
                    .to_string(),
                seal: SealSpec::report(),
                tools: None,
                ask_mode: AskMode::Block,
                at: chrono::Utc::now(),
            },
        ),
    )
    .await
    .expect("a live worker finishes inside three minutes")
    .expect("the run starts");

    let after = std::fs::read_to_string(&file).expect("the file is still there");
    assert!(
        after.contains("bough") && !after.contains("world"),
        "the FILE is the proof, never the worker's summary: {after:?} (outcome {:?})",
        result.outcome
    );
    assert_eq!(
        result.outcome,
        WorkerOutcome::Done,
        "a worker that did the task and called `report` is Done"
    );
    assert!(
        result.report.is_some(),
        "a Done run carries a report that validated against the seal"
    );
    assert!(
        result.report_step.is_some(),
        "the report lands in the SPAWNER's chain as one `worker/report` step"
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}
