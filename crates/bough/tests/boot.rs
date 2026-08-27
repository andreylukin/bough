//! V7's launcher half: an enabled row that never activates is a boot FAILURE, reported row by row,
//! with teardown before exit — and SIGINT is `shutdown().await` then exit.
//!
//! These drive the real binary, because the claim is about a process's exit code and its output.
//! "Teardown before exit" is asserted on EVIDENCE, not on the exit code: `hello`'s `unload_marker`
//! config field makes its unwind touch a file, which is the only unload evidence that survives a
//! process boundary. Asserting the exit code alone would hold just as well with
//! `kernel.shutdown()` deleted.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bough"))
}

fn check(home: &Path) -> std::process::Output {
    Command::new(bin())
        .args(["--profile", "tui", "--check", "--no-watch"])
        .env("BOUGH_HOME", home)
        .output()
        .expect("run bough --check")
}

/// A home whose user patch disables the greeting provider, leaving `hello.greeter` PENDING.
fn unbootable_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("bough.patch.yml"),
        "entries:\n  greeting.provider:\n    disabled: true\n",
    )
    .unwrap();
    home
}

/// A bootable home whose `hello.greeter` row writes a marker file when its fiber unwinds.
fn marker_home() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let marker = home.path().join("unwound.marker");
    std::fs::write(
        home.path().join("bough.patch.yml"),
        format!(
            "entries:\n  hello.greeter:\n    config:\n      who: world\n      unload_marker: {}\n",
            marker.display()
        ),
    )
    .unwrap();
    (home, marker)
}

#[test]
fn enabled_row_that_never_activates_fails_boot_after_teardown() {
    let home = unbootable_home();
    let out = check(home.path());
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "an unresolved row must fail boot");
    assert!(
        stderr.contains("hello.greeter"),
        "the failure must name the unresolved row: {stderr}"
    );
    assert!(
        stderr.contains("greeting"),
        "the failure must name the unmet key: {stderr}"
    );
    assert!(
        out.status.code().is_some(),
        "the process must exit normally after teardown, not be killed: {:?}",
        out.status
    );
}

/// The teardown half of the same claim, on evidence: a boot that FAILS must still unwind the rows
/// that did activate before the process exits.
#[test]
fn a_failed_boot_unwinds_the_rows_that_did_activate() {
    let home = tempfile::tempdir().unwrap();
    let marker = home.path().join("unwound.marker");
    let patch = format!(
        "entries:\n\
         \x20 hello.greeter:\n\
         \x20   config:\n\
         \x20     who: world\n\
         \x20     unload_marker: {}\n\
         insert:\n\
         \x20 - entry:\n\
         \x20     id: doomed.row\n\
         \x20     plugin: hello\n\
         \x20     config: {{ who: nobody }}\n\
         \x20     inject: [nothing-provides-this]\n",
        marker.display()
    );
    std::fs::write(home.path().join("bough.patch.yml"), &patch).unwrap();

    let out = check(home.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`doomed.row` can never activate, so boot must fail: {stderr}"
    );
    assert!(
        stderr.contains("doomed.row"),
        "the failure must name the unresolved row: {stderr}"
    );
    assert!(
        marker.is_file(),
        "the rows that DID activate must be unwound before the process exits: {stderr}"
    );
}

#[test]
fn boot_failure_exit_code_is_one() {
    let home = unbootable_home();
    assert_eq!(check(home.path()).status.code(), Some(1));
}

#[test]
fn sigint_tears_down_before_exit() {
    let (home, marker) = marker_home();
    // No `--check`: the process boots a good tree and then waits for SIGINT.
    let mut child = Command::new(bin())
        // A generous teardown deadline: the claim under test is that SIGINT unwinds the rows, not
        // that unwinding fits in the product's default 2s budget. With the default, this test
        // failed whenever the rest of the file's child processes loaded the machine enough to push
        // teardown past the deadline — `bounded` then abandons the unwind, and the marker never
        // appears (phase ux1 §2.4, B8).
        .args(["--profile", "tui", "--no-watch", "--shutdown-ms", "30000"])
        .env("BOUGH_HOME", home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bough");

    // Wait for the tree to be UP before signalling, rather than sleeping a guessed interval: the
    // `ledger` row creates `$BOUGH_HOME/ledger.db` when it activates, so its appearance is a real
    // readiness signal. A fixed sleep raced the process's own start-up whenever the machine was
    // loaded by the rest of the suite.
    let ready = home.path().join("ledger.db");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.is_file() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "a good tree must keep running until it is signalled"
        );
        assert!(Instant::now() < deadline, "the tree never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The ledger row is up; give the rest of the tree a moment to quiesce.
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        child.try_wait().unwrap().is_none(),
        "a good tree must keep running until it is signalled"
    );
    assert!(
        !marker.is_file(),
        "nothing may have unwound while the tree is up"
    );

    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "SIGINT must tear down and exit, not hang"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(
        status.code(),
        Some(0),
        "SIGINT is shutdown().await then a clean exit"
    );
    assert!(
        marker.is_file(),
        "the exit code alone is not teardown: `hello`'s unwind must have run before exit"
    );
}
