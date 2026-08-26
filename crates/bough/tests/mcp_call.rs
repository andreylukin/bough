//! V7's launcher half: `bough mcp call <server> <tool> <json>` really mounts the configured MCP
//! server, really speaks the protocol to it, prints the tool's result, and exits.
//!
//! This drives the REAL BINARY as a subprocess against the hermetic stdio fixture server, because
//! what is under test is the whole path — CLI parse, synthetic `mcp.call` layer, `mcp-rmcp`
//! mounting the child, the call, stdout, exit code. An in-process `boot()` would skip most of it.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Home {
        let p = std::env::temp_dir().join(format!(
            "bough-mcpcall-{tag}-{}-{}",
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

/// A patch mounting the hermetic python fixture server as `fixture`.
fn server_patch(home: &Home, disabled: bool) -> PathBuf {
    let script = repo_root().join("scripts/fixtures/mcp/fixture-server.py");
    assert!(script.exists(), "the fixture server exists: {script:?}");
    let yaml = format!(
        "entries:\n  mcp.rmcp:\n    config:\n      connect_timeout_ms: 15000\n      call_timeout_ms: 15000\n      servers:\n        - name: fixture\n          disabled: {disabled}\n          transport: {{ kind: stdio, command: python3, args: [\"{}\"] }}\n",
        script.display()
    );
    let p = home.0.join(format!("mcp-{disabled}.yml"));
    std::fs::write(&p, yaml).unwrap();
    p
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn mcp_call(home: &Home, patch: &Path, server: &str, tool: &str, json: &str) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_bough"))
        .env("BOUGH_HOME", &home.0)
        .arg("--root")
        .arg(repo_root())
        .arg("--patch")
        .arg(patch)
        .args(["mcp", "call", server, tool, json])
        .output()
        .expect("the bough binary runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn bough_mcp_call_speaks_to_the_stdio_fixture_and_prints_its_result() {
    let home = Home::new("ok");
    let patch = server_patch(&home, false);
    let run = mcp_call(
        &home,
        &patch,
        "fixture",
        "echo",
        r#"{"text":"hello from the launcher"}"#,
    );
    assert_eq!(
        run.code, 0,
        "a successful call exits 0\nstdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(
        run.stdout.contains("echo: hello from the launcher"),
        "the SERVER's own answer reaches stdout\nstdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
}

#[test]
fn bough_mcp_call_against_a_disabled_server_row_fails_instead_of_answering() {
    let home = Home::new("disabled");
    let patch = server_patch(&home, true);
    let run = mcp_call(&home, &patch, "fixture", "echo", r#"{"text":"hi"}"#);
    assert_ne!(
        run.code, 0,
        "a disabled server row has no tool to call\nstdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(
        !run.stdout.contains("echo: hi"),
        "and nothing was called\nstdout: {}",
        run.stdout
    );
}
