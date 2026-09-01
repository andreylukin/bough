//! V3 (WP-5, §17 Phase 8, §7): `kill -9` during a wake, against the real headless binary with a
//! pending `action/intent` row and a recording `gh` shim. On restart the wake is closed
//! `interrupted`, nothing durable is lost, and the intent is reconciled — never re-executed.
//!
//! Two variants matter: killed BEFORE the outward call, and killed AFTER it. The second is the one
//! that would re-execute if reconciliation guessed instead of looking the marker up in the world.
//! `actions-shim`'s two configured delays are exactly those two windows.
//!
//! Nothing here sleeps to wait for the process: the kill is armed by POLLING the sqlite ledger
//! (variant A) or the shim's own call log (variant B), so the kill lands inside the window rather
//! than near it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bough_kernel::{Context, KernelCore};
use bough_plugin_actions::ActionsHandle;
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{LedgerHandle, LedgerStore, Step, StepType, TrajId, WakeId};
use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};

/// How long a poll may wait before the test calls it a failure. A window that never opens is the
/// bug this file exists to catch, so it is reported, never waited out.
///
/// Generous on purpose: five of these cases each boot a DEBUG build of the whole tree at once, and
/// on a loaded machine that is seconds, not milliseconds. A 60s deadline went red under a parallel
/// cargo run while passing in 4s on its own — a deadline that fires under load is a flake, not a
/// gate.
const DEADLINE: Duration = Duration::from_secs(240);

/// The lane `bough exec` runs on (`bundles/bough-headless.yml`).
fn traj() -> TrajId {
    TrajId::new("lane/trunk")
}

/// A throwaway `$BOUGH_HOME` that SURVIVES the crash: both processes open the same one.
struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Home {
        // A per-Home counter, not just the clock: several cases run the SAME variant (the same
        // `tag`) concurrently in one test binary, and two of them entering this function inside
        // one clock tick would share a directory — whereupon the second `remove_dir_all` deletes
        // the first's home out from under a running child.
        static NTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "bough-crash-{tag}-{}-{nth}-{}",
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
    fn gh_log(&self) -> PathBuf {
        self.0.join("gh-calls.log")
    }
    fn patch(&self) -> PathBuf {
        self.0.join("crash.patch.yml")
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

/// Render `fixtures/crash.patch.yml` for this run: the shim's absolute path and the two delays.
///
/// The template carries placeholders rather than a path because the shim's location is per-run;
/// `actions-shim` takes the binary from CONFIG and never hardcodes `gh`, which is what makes a
/// recording shim reachable without a PATH trick.
fn write_patch(home: &Home, before_ms: u64, after_ms: u64) -> PathBuf {
    let template = std::fs::read_to_string(fixture("crash.patch.yml")).expect("the template");
    let rendered = template
        .replace("@GH@", fixture("gh-shim.sh").to_str().expect("utf-8 path"))
        .replace("@BEFORE@", &before_ms.to_string())
        .replace("@AFTER@", &after_ms.to_string());
    let path = home.patch();
    std::fs::write(&path, rendered).expect("the rendered layer is writable");
    path
}

/// Start `bough exec` against `home`, with the shim's call log named in the environment.
fn spawn_exec(home: &Home, task: &str, patch: &PathBuf) -> Child {
    Command::new(env!("CARGO_BIN_EXE_bough"))
        .env("BOUGH_HOME", &home.0)
        .env("BOUGH_TEST_GH_LOG", home.gh_log())
        .arg("--root")
        .arg(repo_root())
        .arg("--patch")
        .arg(repo_root().join("bundles/bough-typed.yml"))
        .arg("--patch")
        .arg(patch)
        .arg("exec")
        .arg(task)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the bough binary runs")
}

/// Open the run's ledger from THIS process. A second connection to the same file: the crashed
/// process held a write-ahead log, and reading it back is half of what "nothing was lost" means.
fn open(home: &Home) -> Arc<SqliteStore> {
    let store = SqliteStore::open(
        &SqliteConfig {
            path: home.0.join("ledger.db"),
            busy_timeout_ms: 5000,
        },
        Context::root(KernelCore::new()),
    )
    .expect("the db opens");
    // Every vocabulary the headless tree writes with. A store that has not been told a step type
    // refuses to READ it (`UnknownStepTypeOnRead`), so a reader outside the process has to
    // declare the same set the tree did — which is exactly the §3 rule that a step type is owned
    // by the row that defines it, seen from the other side.
    for def in bough::vocabulary::all() {
        // MERGE: the hand-written list of thirteen crates is gone. `bough::vocabulary::all()` is
        // the launcher's own union — `docs/track-c-merge-notes.md` asked for exactly this — and
        // `bough::vocabulary::tests::every_plugin_that_writes_steps_is_in_the_list` greps the tree
        // so it cannot go stale again. It did, at this merge: the code-mode `program/*` types and
        // `tools-operator`'s `schedule/*` were missing, and a store that has not been told a step
        // type reads the chain as EMPTY rather than as an error.
        let _ = store.register_step_type(def);
    }
    store
}

/// Every step on the exec lane, oldest first.
async fn steps_of(store: &Arc<SqliteStore>) -> Vec<Step> {
    store
        .steps(&StepQuery {
            trajs: vec![traj()],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the chain reads back")
}

/// Poll `cond` over the live ledger until it holds, and return the chain at that instant.
///
/// The returned snapshot is the pre-kill truth the post-restart assertions are compared against:
/// taken from the SAME read that armed the kill, so there is no window between "the test decided
/// to kill" and "the test recorded what existed".
async fn until(home: &Home, what: &str, cond: impl Fn(&[Step]) -> bool) -> Vec<Step> {
    let start = Instant::now();
    loop {
        // Reopened per poll: a connection held open across the crash sees a stale snapshot.
        let store = open(home);
        let steps = steps_of(&store).await;
        let hit = cond(&steps);
        store.retire();
        if hit {
            return steps;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "waited {DEADLINE:?} for {what} and it never happened"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll the shim's own call log until it has a line.
async fn until_gh_called(home: &Home) -> Vec<String> {
    let start = Instant::now();
    loop {
        let lines = gh_calls(home);
        if !lines.is_empty() {
            return lines;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "waited {DEADLINE:?} for the gh shim to be invoked and it never was"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Every line the recording shim wrote.
fn gh_calls(home: &Home) -> Vec<String> {
    std::fs::read_to_string(home.gh_log())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// `SIGKILL`, then reap. Not `Child::kill`, which sends SIGKILL too but is worth spelling out:
/// the point of this file is that NOTHING ran on the way out — no teardown, no flush, no
/// `action/done`.
fn kill_9(child: &mut Child) {
    // SAFETY: the pid is this process's own child and has not been reaped.
    unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
    let status = child.wait().expect("the killed child is reaped");
    assert!(
        status.code().is_none(),
        "the child exited normally instead of being killed: {status:?}"
    );
}

/// The wake that repair closed, and the `wake/end` step that closed it.
fn interrupted_wake(steps: &[Step]) -> (WakeId, &Step) {
    let end = steps
        .iter()
        .filter(|s| s.kind.as_str() == "wake/end")
        .find(|s| s.body["reason"] == "interrupted")
        .unwrap_or_else(|| {
            let chain: Vec<String> = steps
                .iter()
                .map(|s| match s.kind.as_str() {
                    "wake/end" => format!("wake/end({})", s.body["reason"]),
                    k => k.to_string(),
                })
                .collect();
            panic!(
                "boot repair closed the orphaned wake with the one reason no live loop emits; \
                 the chain after the restart was {chain:?}"
            )
        });
    (end.wake.clone(), end)
}

/// What one crash-and-restart produced.
struct Crash {
    home: Home,
    /// The chain as it stood at the instant the kill was armed.
    before: Vec<Step>,
    /// The chain after the restart.
    after: Vec<Step>,
}

/// Run the whole V3 sequence for one variant.
///
/// `before_ms` / `after_ms` place the kill window; `arm` decides WHEN to pull the trigger.
async fn crash_and_restart(
    tag: &str,
    before_ms: u64,
    after_ms: u64,
    arm: impl AsyncFn(&Home) -> Vec<Step>,
) -> Crash {
    let home = Home::new(tag);
    let patch = write_patch(&home, before_ms, after_ms);
    let mut child = spawn_exec(&home, "open a pr for this", &patch);

    let before = arm(&home).await;
    kill_9(&mut child);

    // The RESTART: the same `$BOUGH_HOME`, a different task. Nothing in it asks for an action, so
    // anything the shim records after this point was recorded by reconciliation — which must
    // record nothing.
    let restart = write_patch(&home, 0, 0);
    let out = Command::new(env!("CARGO_BIN_EXE_bough"))
        .env("BOUGH_HOME", &home.0)
        .env("BOUGH_TEST_GH_LOG", home.gh_log())
        .arg("--root")
        .arg(repo_root())
        .arg("--patch")
        .arg(&restart)
        .arg("exec")
        .arg("say hello")
        .output()
        .expect("the bough binary runs again");
    assert!(
        out.status.success(),
        "the restart must boot over the crashed home\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let store = open(&home);
    let after = steps_of(&store).await;
    store.retire();
    Crash {
        home,
        before,
        after,
    }
}

/// Killed in the window BEFORE the outward call: an intent row exists and the world was not
/// touched.
async fn killed_before_the_call() -> Crash {
    crash_and_restart("before", 5000, 0, async |home| {
        until(home, "an `action/intent` row", |steps| {
            steps.iter().any(|s| s.kind.as_str() == "action/intent")
        })
        .await
    })
    .await
}

/// Killed in the window AFTER the outward call: the world HAS been touched and the journal does
/// not know it yet. This is the variant a guessing reconciliation would re-execute.
async fn killed_after_the_call() -> Crash {
    crash_and_restart("after", 0, 5000, async |home| {
        until_gh_called(home).await;
        // The chain as it stands the moment the act is known to have happened.
        let store = open(home);
        let steps = steps_of(&store).await;
        store.retire();
        steps
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_wake_reopens_closed_as_interrupted() {
    let c = killed_before_the_call().await;
    let (wake, end) = interrupted_wake(&c.after);
    assert!(
        c.before
            .iter()
            .any(|s| s.wake == wake && s.kind.as_str() == "wake/start"),
        "the wake repair closed is the one that was open when the process died"
    );
    assert!(
        !c.before
            .iter()
            .any(|s| s.wake == wake && s.kind.as_str() == "wake/end"),
        "vacuity guard: that wake was still OPEN at the kill"
    );
    assert_eq!(
        c.after
            .iter()
            .filter(|s| s.wake == wake && s.kind.as_str() == "wake/end")
            .count(),
        1,
        "closed exactly once"
    );
    assert_eq!(end.body["reason"], "interrupted");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_in_flight_thought_is_missing_after_the_restart() {
    let c = killed_before_the_call().await;
    let (wake, _) = interrupted_wake(&c.after);

    // Nothing durable was LOST: every step that existed at the kill is still there, at the same
    // seq, with the same kind and the same body.
    for old in &c.before {
        let found = c
            .after
            .iter()
            .find(|s| s.seq == old.seq)
            .unwrap_or_else(|| panic!("step {:?} ({}) was lost in the crash", old.seq, old.kind));
        assert_eq!(found.kind, old.kind, "step {:?} changed kind", old.seq);
        assert_eq!(found.body, old.body, "step {:?} was rewritten", old.seq);
    }

    // And nothing was INVENTED: the only steps repair added to the orphaned wake are the two it
    // owes — a result for each unanswered call, and the close. The in-flight thought is what is
    // missing, and it is missing because it was never durable (§5: text flushes as a step).
    let added: Vec<&str> = c
        .after
        .iter()
        .filter(|s| s.wake == wake && !c.before.iter().any(|o| o.seq == s.seq))
        .map(|s| s.kind.as_str())
        .collect();
    assert!(
        added
            .iter()
            .all(|k| *k == "tool/result" || *k == "wake/end"),
        "boot repair added something it does not owe: {added:?}"
    );
    assert!(
        !c.after
            .iter()
            .any(|s| s.wake == wake && s.kind.as_str() == "thought/text"),
        "the in-flight thought is the ONLY thing lost, and it is lost: it was never durable"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_unanswered_tool_call_gets_an_unknown_result() {
    let c = killed_before_the_call().await;
    let (wake, end) = interrupted_wake(&c.after);

    let calls: Vec<&Step> = c
        .after
        .iter()
        .filter(|s| s.wake == wake && s.kind.as_str() == "tool/call")
        .collect();
    assert!(
        !calls.is_empty(),
        "vacuity guard: the crashed wake had a tool call in flight"
    );
    for call in calls {
        let id = &call.body["call"];
        let result = c
            .after
            .iter()
            .filter(|s| s.wake == wake && s.kind.as_str() == "tool/result")
            .find(|s| &s.body["call"] == id)
            .unwrap_or_else(|| panic!("call {id} was never answered"));
        assert_eq!(
            result.body["outcome"], "unknown",
            "an unanswered call is answered `unknown`, never invented as ok or error"
        );
        assert!(
            result.seq < end.seq,
            "and the answer lands before the wake closes"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pending_intent_is_listed_after_the_restart_and_never_re_executed() {
    let c = killed_before_the_call().await;

    // `ActionsHandle::pending()` over the crashed journal: the seam's OWN reconciliation entry
    // point, not a query written for the test.
    let store = open(&c.home);
    let actions = ActionsHandle::new(LedgerHandle(store.clone() as Arc<dyn LedgerStore>));
    let pending = actions.pending().await.expect("the journal reads back");
    store.retire();

    assert_eq!(
        pending.len(),
        1,
        "the intent-without-done row is LISTED: {pending:#?}"
    );
    assert_eq!(pending[0].kind, bough_plugin_actions::ActionKind::OpenPr);
    assert_eq!(pending[0].target, "andrey/bough");
    assert_eq!(
        pending[0].marker,
        bough_plugin_actions::marker_for(&pending[0].idem_key),
        "the marker is DERIVED from the journal, so reconciliation can look the world up"
    );

    // Killed BEFORE the call, so nothing was ever done to the world — and the restart did not do
    // it either. Reconciliation LISTS; it never acts.
    assert!(
        gh_calls(&c.home).is_empty(),
        "the outward call never happened and reconciliation did not perform it: {:?}",
        gh_calls(&c.home)
    );
    assert!(
        !c.after
            .iter()
            .any(|s| s.kind == StepType::new("action/done")),
        "no `action/done` was invented for an act that never happened"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kill_after_the_outward_call_still_yields_exactly_one_gh_invocation() {
    let c = killed_after_the_call().await;

    let calls = gh_calls(&c.home);
    assert_eq!(
        calls.len(),
        1,
        "the world was touched exactly once, across the crash AND the restart: {calls:?}"
    );

    let store = open(&c.home);
    let actions = ActionsHandle::new(LedgerHandle(store.clone() as Arc<dyn LedgerStore>));
    let pending = actions.pending().await.expect("the journal reads back");
    store.retire();
    assert_eq!(
        pending.len(),
        1,
        "the act is unreconciled: an intent with no done, which is exactly what a crash between \
         them leaves"
    );
    // The marker the journal can recompute is the marker the shim was handed — which is what
    // makes reconciliation a LOOKUP against the world rather than a guess (§7).
    let marker = bough_plugin_actions::marker_for(&pending[0].idem_key);
    assert!(
        calls[0].contains(&marker),
        "the one invocation carries the journal's own marker ({marker}): {}",
        calls[0]
    );
    assert_eq!(
        calls.iter().filter(|l| l.contains(&marker)).count(),
        1,
        "exactly one line for that idem key"
    );

    // And the wake is closed the same way the other variant's is.
    let (_, end) = interrupted_wake(&c.after);
    assert_eq!(end.body["reason"], "interrupted");
}
