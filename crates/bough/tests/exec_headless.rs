//! V9's second half (§17 Phase 2): `bough exec` runs ONE task through the ordinary loop and exits.
//!
//! These drive the REAL BINARY as a subprocess, because half of what is being asserted is the
//! process boundary: what reached stdout, what the exit code was, and whether the tree was torn
//! down before the process left. A test that called `boot()` in-process could check none of it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A throwaway `$BOUGH_HOME`. Removed on drop.
struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Home {
        let p = std::env::temp_dir().join(format!(
            "bough-exec-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Home(p)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// What one `bough exec` run produced.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn exec(home: &Home, task: &str, patches: &[PathBuf]) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bough"));
    cmd.env("BOUGH_HOME", &home.0)
        .arg("--root")
        .arg(repo_root());
    for p in patches {
        cmd.arg("--patch").arg(p);
    }
    cmd.arg("exec").arg(task);
    let out = cmd.output().expect("the bough binary runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn exec_runs_one_task_end_to_end_with_llm_replay() {
    let home = Home::new("replay");
    let run = exec(&home, "what is two plus two", &[fixture("exec-replay.yml")]);
    assert_eq!(
        run.code, 0,
        "exec must exit 0 on a completed task\nstdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(
        run.stdout.contains("four"),
        "the recorded answer must reach stdout\nstdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
}

#[test]
fn exec_exits_with_the_ledger_intact() {
    let home = Home::new("ledger");
    let run = exec(&home, "what is two plus two", &[fixture("exec-replay.yml")]);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let db = home.0.join("ledger.db");
    assert!(db.is_file(), "the task's chain must be on disk at {db:?}");
    assert!(
        db.metadata().unwrap().len() > 0,
        "an empty ledger file is not an intact one"
    );
}

#[test]
fn exec_tears_down_before_exit() {
    let home = Home::new("teardown");
    let run = exec(&home, "what is two plus two", &[fixture("exec-replay.yml")]);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    // A sqlite connection that was CLOSED leaves no write-ahead log behind. A process that asked
    // to exit without unloading the tree would leave `ledger.db-wal` sitting next to the db.
    let wal = home.0.join("ledger.db-wal");
    assert!(
        !wal.exists(),
        "a leftover {wal:?} means the process left before the tree was unloaded"
    );
    assert!(
        !run.stderr.contains("did not reach a quiescent state"),
        "stderr: {}",
        run.stderr
    );
}

#[test]
fn an_empty_task_is_not_a_task_and_the_row_still_activates() {
    // `--profile headless` with no `exec` subcommand: the row mounts, does nothing, and `--check`
    // asserts every enabled row activated.
    let home = Home::new("idle");
    let out = Command::new(env!("CARGO_BIN_EXE_bough"))
        .env("BOUGH_HOME", &home.0)
        .arg("--root")
        .arg(repo_root())
        .arg("--profile")
        .arg("headless")
        .arg("--check")
        .arg("--no-watch")
        .output()
        .expect("the bough binary runs");
    assert!(
        out.status.success(),
        "the headless profile must boot with no task\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "live: needs BOUGH_LIVE=1 and ANTHROPIC_API_KEY (make live)"]
fn exec_runs_one_task_live_with_haiku() {
    if std::env::var("BOUGH_LIVE").as_deref() != Ok("1") {
        eprintln!("BOUGH_LIVE is not 1; skipping");
        return;
    }
    let home = Home::new("live");
    // No replay patch: the shipped `llm-anthropic` row answers, under the model `model-policy`
    // picks for an answer wake — `sol`, which is claude-haiku-4-5-20251001 in `bough-base`.
    let run = exec(
        &home,
        "Reply with exactly the word: pong. Nothing else.",
        &[],
    );
    assert_eq!(
        run.code, 0,
        "stdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(
        run.stdout.to_lowercase().contains("pong"),
        "the live answer must reach stdout\nstdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
}

// ---------------------------------------------------------------------------------------------
// V9's first half, through the BOOT PATH. `plugins/agent-loop/tests/repair.rs` calls `repair::run`
// directly; this plants the tail a crash leaves in the REAL sqlite ledger `bough exec` will open,
// then runs the binary and reads the file back. Nothing here calls repair: the only thing that can
// close the orphan is `agent-loop`'s `repair_on_boot` step inside the process.
mod repair_at_boot {
    use super::*;
    use bough_kernel::{Context, KernelCore};
    use bough_plugin_ledger::{
        AgentName, Append, Class, LedgerStore, NewRollup, RollupKind, RollupQuery, Seq, StepQuery,
        StepType, TrajId, WakeId,
    };
    use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};
    use std::sync::Arc;

    fn traj() -> TrajId {
        TrajId::new("lane/crashed")
    }

    fn open(home: &Home) -> Arc<SqliteStore> {
        let store = SqliteStore::open(
            &SqliteConfig {
                path: home.0.join("ledger.db"),
                busy_timeout_ms: 5000,
            },
            Context::root(KernelCore::new()),
        )
        .expect("the db opens");
        for def in bough_plugin_tools::vocabulary::step_types() {
            let _ = store.register_step_type(def);
        }
        store
    }

    /// A wake that opened, called a tool, and never came back — plus a sealed rollup to watch.
    async fn plant(home: &Home) -> (WakeId, serde_json::Value) {
        let store = open(home);
        store
            .put_agent(bough_plugin_ledger::AgentRow {
                name: AgentName::new("crashed"),
                traj: traj(),
                routing_refs: Default::default(),
                wake_classes: Default::default(),
                model_override: None,
                tick_floor: None,
                digest_rollup: None,
            })
            .await
            .expect("the row lands");
        let wake = WakeId::new("crashed-wake");
        for (kind, body) in [
            ("wake/start", serde_json::json!({ "urgency": "immediate" })),
            ("step/start", serde_json::json!({ "index": 0 })),
            (
                "tool/call",
                serde_json::json!({ "call": "c1", "name": "bash", "args": {},
                                    "render": "terminal", "step_index": 0 }),
            ),
        ] {
            store
                .append(Append {
                    traj: traj(),
                    wake: wake.clone(),
                    kind: StepType::new(kind),
                    class: Class::Thought,
                    body,
                    cites: vec![],
                    at: chrono::Utc::now(),
                    id: None,
                })
                .await
                .expect("the step lands");
        }
        let sealed = store
            .seal_rollup(NewRollup {
                id: None,
                traj: traj(),
                kind: RollupKind::Tier,
                tier: 0,
                from_seq: Seq(1),
                to_seq: Seq(3),
                src_trajs: vec![traj()],
                body: serde_json::json!({ "text": "a sealed segment" }),
                notable_refs: Default::default(),
                prompt_ver: "p1".into(),
                sealed_at: chrono::Utc::now(),
            })
            .await
            .expect("the rollup seals");
        store.retire();
        (wake, sealed.body)
    }

    #[test]
    fn booting_exec_closes_an_orphaned_wake_and_leaves_rollups_alone() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let home = Home::new("repair");
        let (wake, rollup_body) = rt.block_on(plant(&home));

        let run = exec(&home, "what is two plus two", &[fixture("exec-replay.yml")]);
        assert_eq!(run.code, 0, "stderr: {}", run.stderr);

        rt.block_on(async {
            let store = open(&home);
            let ends = store
                .steps(&StepQuery {
                    trajs: vec![traj()],
                    kinds: vec![StepType::new("wake/end")],
                    ..Default::default()
                })
                .await
                .expect("a read");
            assert_eq!(ends.len(), 1, "the orphan is closed exactly once");
            assert_eq!(ends[0].wake, wake);
            assert_eq!(
                ends[0].body["reason"], "interrupted",
                "boot repair closes it with the reason no live loop emits"
            );

            let results = store
                .steps(&StepQuery {
                    trajs: vec![traj()],
                    kinds: vec![StepType::new("tool/result")],
                    ..Default::default()
                })
                .await
                .expect("a read");
            assert_eq!(results.len(), 1, "the unanswered call got a result");
            assert_eq!(results[0].body["call"], "c1");
            assert_eq!(results[0].body["outcome"], "unknown");
            assert!(
                results[0].seq < ends[0].seq,
                "and it landed before the wake closed"
            );

            let rollups = store
                .rollups(&RollupQuery {
                    trajs: vec![traj()],
                    include_superseded: true,
                    ..Default::default()
                })
                .await
                .expect("a read");
            assert_eq!(rollups.len(), 1, "no rollup was added or removed");
            assert_eq!(rollups[0].body, rollup_body, "and none was rewritten");
            assert!(rollups[0].superseded_by.is_none());
            store.retire();
        });
    }
}
