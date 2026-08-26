//! V7's launcher half: an enabled row that never activates is a boot FAILURE, reported row by row,
//! with teardown before exit — and SIGINT is `shutdown().await` then exit.
//!
//! These drive the real binary, because the claim is about a process's exit code and its output.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bough"))
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

fn check(home: &Path) -> std::process::Output {
    Command::new(bin())
        .args(["--profile", "tui", "--check", "--no-watch"])
        .env("BOUGH_HOME", home)
        .output()
        .expect("run bough --check")
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
    // Teardown before exit: the process exits under its own power (no hang, no abort), which is
    // only true if `shutdown().await` returned before `main` did.
    assert!(
        out.status.code().is_some(),
        "the process must exit normally after teardown, not be killed: {:?}",
        out.status
    );
}

#[test]
fn boot_failure_exit_code_is_one() {
    let home = unbootable_home();
    assert_eq!(check(home.path()).status.code(), Some(1));
}

#[test]
fn sigint_tears_down_before_exit() {
    let home = tempfile::tempdir().unwrap();
    // No `--check`: the process boots a good tree and then waits for SIGINT.
    let mut child = Command::new(bin())
        .args(["--profile", "tui", "--no-watch"])
        .env("BOUGH_HOME", home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bough");

    // Give it time to compose, mount and quiesce before signalling.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "a good tree must keep running until it is signalled"
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
}
