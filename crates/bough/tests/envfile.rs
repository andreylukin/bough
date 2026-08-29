//! Invariant under test: `$BOUGH_HOME/env` is loaded at LAUNCH, before compose — a key written
//! there reaches the compose-time `!!expr env_or(...)` snapshot — and the process environment
//! WINS over the file. This is the boot-level half; the parsing rules are `envfile.rs`'s own
//! unit tests.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bough"))
}

fn home_with_env_canary() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("env"),
        "# keys for this machine\nBOUGH_TEST_ENVFILE_CANARY=from-env-file\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("bough.patch.yml"),
        "entries:\n  hello.greeter:\n    config:\n      who: !!expr 'env_or(\"BOUGH_TEST_ENVFILE_CANARY\", \"missing\")'\n      log_level: info\n",
    )
    .unwrap();
    home
}

fn dump(home: &Path, extra_env: Option<(&str, &str)>) -> String {
    let mut c = Command::new(bin());
    c.args(["--profile", "headless", "--dump-config", "--no-watch"])
        .env("BOUGH_HOME", home)
        .env_remove("BOUGH_TEST_ENVFILE_CANARY");
    if let Some((k, v)) = extra_env {
        c.env(k, v);
    }
    let out = c.output().expect("run bough --dump-config");
    assert!(
        out.status.success(),
        "--dump-config failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_key_in_the_env_file_reaches_the_compose_time_snapshot() {
    let home = home_with_env_canary();
    let text = dump(home.path(), None);
    assert!(
        text.contains("from-env-file"),
        "the env file's value never reached `env_or`:\n{text}"
    );
}

#[test]
fn the_process_environment_wins_over_the_file() {
    let home = home_with_env_canary();
    let text = dump(
        home.path(),
        Some(("BOUGH_TEST_ENVFILE_CANARY", "from-process")),
    );
    assert!(
        text.contains("from-process") && !text.contains("from-env-file"),
        "a shell export must never be overwritten by the file:\n{text}"
    );
}
