//! §2.12: supervision, jittered backoff, `is_ready`, and the `bough/actions` notification.
//!
//! Every assertion is on a real OS process speaking real JSON-RPC over stdio. The one thing these
//! tests DO NOT touch is `ctx.mcp`: the client is asserted directly, because "its tools are still
//! registered" is a property of THIS client (`list_tools` answers from the last successful listing
//! while the process is down), and the seam only holds the `Arc` across the restart.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bough_plugin_mcp::{McpClient, McpError};
use bough_plugin_mcp_subprocess::process::{McpProcessConfig, ProcessState, ResidentProcess};
use bough_plugin_mcp_subprocess::{child_entry, validate_row, ProcessRow};
use bough_plugin_runtime_actions::{RuntimeAction, RuntimeLimits};

fn server() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/fixtures/mcp-process/echo-server.py")
        .canonicalize()
        .expect("the fixture server exists")
}

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        max_actions: 16,
        max_spawns: 2,
        max_text_bytes: 8192,
    }
}

fn row(name: &str, env: BTreeMap<String, String>) -> ProcessRow {
    ProcessRow {
        name: name.into(),
        command: "python3".into(),
        args: vec![server().display().to_string()],
        env,
        cwd: None,
        max_restarts: 2,
        min_uptime_ms: 1000,
        restart_delay_ms: 20,
    }
}

fn cfg(row: ProcessRow) -> Arc<McpProcessConfig> {
    Arc::new(McpProcessConfig {
        row,
        limits: limits(),
    })
}

/// Poll until `f` holds or the deadline passes. Supervision is asynchronous by nature; a fixed
/// sleep would make the test a flake generator.
async fn until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

fn no_actions() -> bough_plugin_mcp_subprocess::process::ActionsCallback {
    Arc::new(|_| {})
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_subprocess_plugin_mounts_lists_its_tools_and_answers_a_call() {
    let (p, sup) = ResidentProcess::start(cfg(row("echo", BTreeMap::new())), no_actions()).await;
    assert!(p.is_ready(), "the handshake settled: {:?}", p.state());
    assert!(matches!(p.state(), ProcessState::Up { .. }));

    let tools = p.list_tools().await.expect("lists");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "echo");
    assert_eq!(tools[0].server.as_str(), "echo");

    let out = p
        .call("echo", serde_json::json!({ "text": "hello" }))
        .await
        .expect("the call answers");
    assert_eq!(out.content, "hello");
    assert!(!out.is_error);
    // The cite is minted by the SEAM, never by the transport.
    assert!(out.cites.is_empty());
    sup.abort();
}

#[tokio::test]
async fn a_killed_process_respawns_and_its_tools_are_still_listed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let record = dir.path().join("starts");
    let mut env = BTreeMap::new();
    env.insert(
        "MCP_FIXTURE_RECORD".to_string(),
        record.display().to_string(),
    );
    let (p, sup) = ResidentProcess::start(cfg(row("echo", env)), no_actions()).await;
    assert!(p.is_ready());
    assert_eq!(p.spawn_count(), 1);
    let listed_before = p.list_tools().await.expect("lists").len();

    let ProcessState::Up { pid } = p.state() else {
        panic!("expected Up, got {:?}", p.state())
    };
    std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .expect("kill");

    // While it is down: NOT ready, tools STILL LISTED, a call refused with Unavailable rather than
    // a tool that vanished mid-wake.
    assert!(until(2000, || !p.is_ready()).await, "it noticed the death");
    assert_eq!(
        p.list_tools().await.expect("lists").len(),
        listed_before,
        "the last successful listing survives the restart"
    );
    match p.call("echo", serde_json::json!({})).await {
        Err(McpError::Unavailable(s)) => assert_eq!(s.as_str(), "echo"),
        other => panic!("expected Unavailable while down, got {other:?}"),
    }

    // It comes back within the backoff, and its tools are there.
    assert!(
        until(5000, || p.is_ready()).await,
        "it respawned: {:?}",
        p.state()
    );
    assert!(p.spawn_count() >= 2, "a second OS process was started");
    let after = p.list_tools().await.expect("lists");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].tool, "echo");
    let starts = std::fs::read_to_string(&record).expect("the fixture recorded its starts");
    assert!(starts.lines().count() >= 2, "{starts}");
    sup.abort();
}

#[tokio::test]
async fn a_process_that_dies_faster_than_min_uptime_is_quarantined_and_its_sibling_is_untouched() {
    let mut dying = BTreeMap::new();
    dying.insert("MCP_FIXTURE_DIE_MS".to_string(), "40".to_string());
    let mut r = row("dying", dying);
    r.min_uptime_ms = 1500;
    r.max_restarts = 2;
    r.restart_delay_ms = 10;
    let (bad, bad_sup) = ResidentProcess::start(cfg(r), no_actions()).await;
    let (good, good_sup) =
        ResidentProcess::start(cfg(row("healthy", BTreeMap::new())), no_actions()).await;

    assert!(
        until(8000, || matches!(
            bad.state(),
            ProcessState::Quarantined { .. }
        ))
        .await,
        "a crash loop must quarantine, not spin: {:?}",
        bad.state()
    );
    let ProcessState::Quarantined { reason } = bad.state() else {
        unreachable!()
    };
    assert!(reason.contains("died within 1500ms"), "{reason}");
    // max_restarts respawns, plus the original start.
    assert!(
        bad.spawn_count() <= 4,
        "it stopped restarting: {} spawns",
        bad.spawn_count()
    );
    assert!(!bad.is_ready());

    // The sibling never noticed.
    assert!(good.is_ready(), "{:?}", good.state());
    assert_eq!(good.spawn_count(), 1);
    assert_eq!(good.list_tools().await.expect("lists").len(), 1);
    bad_sup.abort();
    good_sup.abort();
}

#[tokio::test]
async fn a_bough_actions_notification_reaches_the_boundary() {
    let seen: Arc<parking_lot::Mutex<Vec<RuntimeAction>>> =
        Arc::new(parking_lot::Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let cb: bough_plugin_mcp_subprocess::process::ActionsCallback =
        Arc::new(move |actions| sink.lock().extend(actions));

    let mut env = BTreeMap::new();
    env.insert("MCP_FIXTURE_ACTIONS".to_string(), "1".to_string());
    let (p, sup) = ResidentProcess::start(cfg(row("talky", env)), cb).await;

    assert!(
        until(3000, || !seen.lock().is_empty()).await,
        "the notification reached the callback"
    );
    assert_eq!(
        seen.lock().clone(),
        vec![RuntimeAction::Hint {
            agent: "sol".into(),
            text: "a resident process said so".into()
        }]
    );
    assert!(p.is_ready());
    sup.abort();
}

#[tokio::test]
async fn a_command_that_does_not_exist_is_reported_and_never_ready() {
    let mut r = row("missing", BTreeMap::new());
    r.command = "/nonexistent/definitely-not-here".into();
    r.min_uptime_ms = 1000;
    r.max_restarts = 1;
    r.restart_delay_ms = 10;
    let (p, sup) = ResidentProcess::start(cfg(r), no_actions()).await;
    assert!(!p.is_ready());
    assert!(
        until(5000, || matches!(
            p.state(),
            ProcessState::Quarantined { .. }
        ))
        .await,
        "a command that cannot start quarantines rather than spinning: {:?}",
        p.state()
    );
    sup.abort();
}

// ---------------------------------------------------------------------------
// pure config
// ---------------------------------------------------------------------------

#[test]
fn validate_refuses_a_row_that_could_never_be_supervised() {
    for (mutate, why) in [
        (
            Box::new(|r: &mut ProcessRow| r.command = String::new())
                as Box<dyn Fn(&mut ProcessRow)>,
            "an empty command",
        ),
        (
            Box::new(|r: &mut ProcessRow| r.name = String::new()),
            "no name",
        ),
        (
            Box::new(|r: &mut ProcessRow| r.max_restarts = 0),
            "no restarts",
        ),
        (
            Box::new(|r: &mut ProcessRow| r.min_uptime_ms = 0),
            "no min uptime",
        ),
        (
            Box::new(|r: &mut ProcessRow| r.restart_delay_ms = 0),
            "no backoff",
        ),
    ] {
        let mut r = row("echo", BTreeMap::new());
        mutate(&mut r);
        assert!(validate_row(&r).is_err(), "{why} must be refused");
    }
    assert!(validate_row(&row("echo", BTreeMap::new())).is_ok());
}

#[test]
fn one_child_entry_per_process_named_for_the_server() {
    let e = child_entry("mcp.subprocess", &row("echo", BTreeMap::new()), &limits());
    assert_eq!(e.id.as_str(), "mcp.subprocess.echo");
    assert_eq!(e.plugin.as_deref(), Some("mcp-process"));
    let parsed: McpProcessConfig = serde_yaml::from_value(e.config).expect("round-trips");
    assert_eq!(parsed.row.name, "echo");
}
