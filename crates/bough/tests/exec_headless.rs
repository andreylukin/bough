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
