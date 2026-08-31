//! phase ux1 §2.10 (B5): "where am I" has ONE answer, decided once at activation.
//!
//! The audit's finding 5 — a file the agent was asked to write "in the current directory" landed
//! somewhere else — was not a daemon and not an env var. `bundles/bough-base.yml` sets
//! `tools-baseline.root: "."`, and `fs::contain` canonicalised that relative root on EVERY CALL,
//! against whatever the process cwd was at that moment. These tests pin both halves of the fix:
//! the root is resolved once against a GIVEN cwd, and a call with a relative path lands under it
//! ON DISK, not merely in a string.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{AgentName, WakeId};
use bough_plugin_tools::{ToolCall, ToolCallId, ToolName, ToolsHandle};
use bough_plugin_tools_baseline::fs::pin_root;
use bough_plugin_tools_baseline::BaselineConfig;

/// The mutex the `set_current_dir` test holds. The process cwd is global state; two tests changing
/// it in parallel would prove nothing about either.
static CHDIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn cfg(root: PathBuf) -> Arc<BaselineConfig> {
    Arc::new(BaselineConfig {
        bash_tags_min: 3,
        bash_tags_max: 5,
        root,
        bash_timeout_ms: 2_000,
        max_output_bytes: 10_000,
        max_read_bytes: 10_000,
        deny_globs: vec![],
    })
}

#[test]
fn pin_root_resolves_a_dot_against_the_given_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let pinned = pin_root(Path::new("."), dir.path()).unwrap();
    assert!(pinned.is_absolute(), "the pinned root is absolute");
    assert_eq!(
        pinned,
        dir.path().canonicalize().unwrap(),
        "`.` means the cwd it was resolved against, canonically"
    );
}

#[test]
fn pin_root_takes_an_absolute_root_as_given() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let pinned = pin_root(other.path(), dir.path()).unwrap();
    assert_eq!(pinned, other.path().canonicalize().unwrap());
}

#[test]
fn a_root_that_does_not_exist_is_a_load_failure_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let err = pin_root(Path::new("nope/at/all"), dir.path()).unwrap_err();
    assert!(err.contains("nope/at/all"), "{err}");
    assert!(err.contains("unreadable"), "{err}");
}

#[test]
fn pin_root_is_immune_to_a_later_chdir() {
    let _guard = CHDIR.lock().unwrap_or_else(|e| e.into_inner());
    let launch = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    let pinned = pin_root(Path::new("."), launch.path()).unwrap();

    let before = std::env::current_dir().unwrap();
    std::env::set_current_dir(elsewhere.path()).unwrap();
    let again = pin_root(Path::new("."), launch.path()).unwrap();
    std::env::set_current_dir(before).unwrap();

    assert_eq!(
        pinned, again,
        "the pinned root is a function of the cwd it was GIVEN, never of the process's current one"
    );
    assert_ne!(pinned, elsewhere.path().canonicalize().unwrap());
}

/// The disk assertion V10 names: a relative path in a tool call lands under the pinned root, and
/// the launch directory is where the file actually is — even though the process has since moved.
// The lock is deliberately held across the awaits: it is what serialises the process-wide cwd
// against the other chdir test, and an async mutex would not make that any less true.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn a_relative_path_lands_under_the_pinned_root() {
    let _guard = CHDIR.lock().unwrap_or_else(|e| e.into_inner());
    let launch = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    // Boot: the row pins the root against the cwd it started in.
    let pinned = pin_root(Path::new("."), launch.path()).unwrap();

    let ctx = Context::root(KernelCore::new());
    let tools = ToolsHandle::with_limits(4, 10_000);
    for spec in bough_plugin_tools_baseline::specs(cfg(pinned.clone())) {
        tools.register(&ctx, spec).await.unwrap();
    }

    // Anything at all moves the process afterwards: a launcher, a test, a plugin.
    let before = std::env::current_dir().unwrap();
    std::env::set_current_dir(elsewhere.path()).unwrap();

    let results = tools
        .execute(
            &ctx,
            vec![ToolCall {
                id: ToolCallId::new("c1"),
                name: ToolName::new("write_file"),
                args: serde_json::json!({ "path": "notes.txt", "content": "here" }),
                agent: AgentName::new("sol"),
                wake: WakeId::new("w1"),
                step_index: 0,
            }],
        )
        .await;

    std::env::set_current_dir(before).unwrap();

    assert!(
        results[0].ok,
        "the write must succeed: {:?}",
        results[0].failure
    );
    // ON DISK, in the launch directory — not in the directory the process wandered into.
    assert_eq!(
        std::fs::read_to_string(pinned.join("notes.txt")).unwrap(),
        "here"
    );
    assert!(!elsewhere.path().join("notes.txt").exists());
}

/// B5's brand actually enforces its own doc comment now. A relative or non-canonical path cannot
/// construct a `WorkspaceRoot` at all, so the invariant rests on the TYPE and not on the single
/// call site in this crate.
#[test]
fn a_workspace_root_refuses_a_relative_or_uncanonical_path() {
    use bough_plugin_tools::WorkspaceRoot;
    let err = WorkspaceRoot::new(std::path::PathBuf::from("notes"))
        .expect_err("a relative root is not a root");
    assert!(err.contains("ABSOLUTE"), "{err}");
    let err = WorkspaceRoot::new(std::path::PathBuf::from("/tmp/../etc"))
        .expect_err("`..` is not canonical");
    assert!(err.contains("CANONICAL"), "{err}");
    assert!(WorkspaceRoot::new(std::path::PathBuf::from("/tmp")).is_ok());
}
