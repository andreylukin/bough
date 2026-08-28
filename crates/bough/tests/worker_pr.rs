//! V5's end-to-end half: a DELEGATED WORKER opens a PR autonomously, inside the write boundary.
//!
//! Nothing here is mocked at the seam under test. The tree is the SHIPPED `bundles/bough-base.yml`
//! booted through the launcher's own composition path, with two patches: `agent.loop` swapped for
//! `agent-loop-scripted` (so the "model" is deterministic and offline) and `actions.github`'s
//! `gh_bin` pointed at the recording shim (`scripts/fixtures/gh/gh`), which answers from files and
//! records every argv. The path from the scripted tool call to the `gh pr create` argv is the real
//! one: `tools` → `tool-actions` → `ActionsHandle::execute` → `actions-github` → a spawned binary.
//!
//! DEVIATION from docs/phase-6-plan.md's verification map, which names
//! `plugins/actions-github/tests/worker_pr.rs`: the assertion needs `agents`,
//! `agent-loop-scripted`, `tools`, `workers`, `worker-spawn`, `actions` and `actions-github`
//! MOUNTED TOGETHER, which no plugin crate's own test target can reach. It lives here for the
//! same reason `crates/bough/tests/worker_spawn.rs` does.

use crate::support;

use std::path::PathBuf;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{
    AgentName, Ledger, LedgerHandle, Order, StepId, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_workers::{AskMode, SealSpec, StartWorker, WorkerKind, Workers};
use support::{boot_real, row_ctx};

const PR_URL: &str = "https://github.com/andreyl/widget/pull/41";

/// A throwaway directory holding the shim's fixtures and its argv log.
struct Shim {
    dir: tempfile::TempDir,
}

impl Shim {
    fn new() -> Shim {
        let shim = Shim {
            dir: tempfile::tempdir().expect("a temp dir"),
        };
        // The ONLY planned `gh` call: `pr create` into this repo. The tail of the argv carries the
        // marker, which is derived from a runtime idem key, so the fixture is a PREFIX one.
        std::fs::write(
            shim.dir.path().join(format!(
                "{}.prefix.json",
                bough_plugin_gh_cli::shim::fixture_name(&[
                    "pr",
                    "create",
                    "--repo",
                    "andreyl/widget"
                ])
            )),
            format!("{PR_URL}\n"),
        )
        .expect("a fixture");
        shim
    }

    fn log_path(&self) -> PathBuf {
        self.dir.path().join("argv.log")
    }

    /// Every `gh` invocation the tree made, in order.
    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.log_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn bin(&self) -> String {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fixtures/gh/gh").to_string()
    }

    /// Exported so the SPAWNED `gh` sees them; `Gh::new` passes no env of its own.
    ///
    /// SAFETY: the caller holds the process-wide `trace::test_lock()`.
    fn export(&self) {
        unsafe {
            std::env::set_var("GH_SHIM_DIR", self.dir.path());
            std::env::set_var("GH_SHIM_LOG", self.log_path());
        }
    }
}

/// The `--patch` layer: a scripted loop that calls `open_pr` once, and `gh` pointed at the shim.
fn patch(shim: &Shim) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("worker-pr.yml");
    let doc = serde_json::json!({
        "entries": {
            "agent.loop": {
                "plugin": "agent-loop-scripted",
                "config": {
                    "strict": true,
                    "wakes": [
                        { "steps": [
                            { "chunks": [
                                { "chunk": "tool_call", "id": "call-1", "name": "open_pr",
                                  "input": {
                                      "target": "andreyl/widget",
                                      "payload": {
                                          "head": "bough/rename-foo",
                                          "base": "main",
                                          "title": "rename `foo` to `bar`",
                                          "body": "As asked."
                                      }
                                  }
                                },
                                { "chunk": "end", "stop": "tool_use" }
                            ] },
                            { "chunks": [
                                { "chunk": "text", "text": "opened the PR" },
                                { "chunk": "end", "stop": "end_turn" }
                            ] }
                        ] }
                    ]
                }
            },
            "actions.github": {
                "config": {
                    "gh_bin": shim.bin(),
                    "known_bots": ["dependabot[bot]"],
                    "timeout_ms": 30000
                }
            }
        }
    });
    std::fs::write(
        &path,
        serde_yaml::to_string(&doc).expect("the patch renders"),
    )
    .expect("written");
    (dir, path)
}

/// The steps of every trajectory, seq-ascending.
async fn all_steps(ledger: &LedgerHandle) -> Vec<bough_plugin_ledger::Step> {
    ledger
        .0
        .steps(&StepQuery {
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the journal reads")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delegated_worker_opens_a_pr_and_the_journal_shows_intent_before_done() {
    let _guard = trace::test_lock();
    let shim = Shim::new();
    shim.export();
    let (_patch_dir, patch_path) = patch(&shim);
    let (kernel, _dir) = boot_real("headless", &[patch_path]).await;

    let ctx = row_ctx(&kernel, "tool.spawn_worker");
    let exec_ctx = row_ctx(&kernel, "exec");
    let agents = exec_ctx.get::<Agents>().expect("the agents key is bound");
    let ledger = exec_ctx.get::<Ledger>().expect("the ledger key is bound");
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

    workers
        .start(
            &ctx,
            StartWorker {
                kind: WorkerKind::Spawn,
                spawner: name.clone(),
                spawner_id: spawner.id().clone(),
                wake: WakeId::new("wake:v5"),
                step: StepId::new("step:v5"),
                depth: 1,
                task: "open a PR that renames `foo` to `bar`".to_string(),
                seal: SealSpec::report(),
                tools: None,
                ask_mode: AskMode::Block,
                at: chrono::Utc::now(),
            },
        )
        .await
        .expect("the worker starts");

    // The worker runs on its own fibers; wait for the act to land rather than for a fixed time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        kernel.quiesce().await;
        let steps = all_steps(&ledger).await;
        if steps.iter().any(|s| s.kind.as_str() == "action/done") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no `action/done` within 20s; the journal was {:?}",
            steps.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>()
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let steps = all_steps(&ledger).await;
    let intent = steps
        .iter()
        .find(|s| s.kind == StepType::new("action/intent"))
        .expect("an `action/intent` step");
    let done = steps
        .iter()
        .find(|s| s.kind == StepType::new("action/done"))
        .expect("an `action/done` step");
    assert!(
        intent.seq < done.seq,
        "intent must precede done: {:?} vs {:?}",
        intent.seq,
        done.seq
    );
    assert_eq!(
        intent.traj, done.traj,
        "both steps belong to the WORKER's trajectory"
    );
    assert_ne!(
        intent.traj,
        TrajId::new("lane/sol"),
        "the act was the delegated worker's, not the spawner's"
    );

    // The world was actually touched, exactly once, and only through `gh pr create`.
    let argv = shim.argv();
    assert_eq!(argv.len(), 1, "exactly one `gh` call: {argv:?}");
    let call = &argv[0];
    assert!(
        call.starts_with("pr create --repo andreyl/widget"),
        "the write is a PR create against the target: {call}"
    );

    // The idem key's marker is IN THE PR BODY the shim saw — recomputed from the journalled key,
    // never read back out of the same struct that wrote it.
    let rows = ledger
        .0
        .actions(&Default::default())
        .await
        .expect("the action rows read");
    assert_eq!(rows.len(), 1, "exactly one action row: {rows:?}");
    let idem = rows[0].idem_key.as_str().to_string();
    let marker = format!("bough-action:{}", &idem[..16]);
    assert!(
        call.contains(&format!("<!-- {marker} -->")),
        "the PR body carries the marker derived from the journalled idem key `{idem}`: {call}"
    );

    // And the artifact the journal recorded is the PR the shim answered with.
    assert_eq!(
        done.body.pointer("/artifact").and_then(|v| v.as_str()),
        Some(PR_URL),
        "the `action/done` step records the PR url: {}",
        done.body
    );
    assert_eq!(
        done.body.pointer("/status").and_then(|v| v.as_str()),
        Some("done"),
        "the action concluded successfully: {}",
        done.body
    );
    let row = &rows[0];
    assert_eq!(
        row.result
            .as_ref()
            .and_then(|r| r.pointer("/marker"))
            .and_then(|v| v.as_str()),
        Some(marker.as_str()),
        "the action row's result carries the same marker the world got: {:?}",
        row.result
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}
