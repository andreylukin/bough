//! The six baseline tools, each on a tempdir: the read/write/edit round-trip, glob and grep,
//! bash exit codes and its timeout, the spill that leaves a locator inline, and §7's containment
//! check refusing a path outside `root`.

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{AgentName, WakeId};
use bough_plugin_tools::{
    FailureClass, PostExecute, ToolCall, ToolCallId, ToolCx, ToolName, ToolOutcome, ToolResult,
    ToolsHandle,
};
use bough_plugin_tools_baseline::{spill, BaselineConfig};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn cfg(root: &TempDir) -> Arc<BaselineConfig> {
    Arc::new(BaselineConfig {
        root: root.path().to_path_buf(),
        bash_timeout_ms: 2_000,
        max_output_bytes: 1_000,
        max_read_bytes: 1_000,
        deny_globs: vec![],
    })
}

async fn registry(ctx: &Context, cfg: Arc<BaselineConfig>) -> ToolsHandle {
    let tools = ToolsHandle::with_limits(4, 10_000);
    for spec in bough_plugin_tools_baseline::specs(cfg) {
        tools.register(ctx, spec).await.unwrap();
    }
    tools
}

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(format!("c-{name}")),
        name: ToolName::new(name),
        args,
        agent: AgentName::new("lane"),
        wake: WakeId::new("w1"),
        step_index: 1,
    }
}

async fn run(
    ctx: &Context,
    tools: &ToolsHandle,
    name: &str,
    args: serde_json::Value,
) -> ToolResult {
    tools
        .execute(ctx, vec![call(name, args)])
        .await
        .pop()
        .expect("one call, one result")
}

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

#[tokio::test]
async fn write_read_and_edit_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx();
    let tools = registry(&ctx, cfg(&dir)).await;

    let w = run(
        &ctx,
        &tools,
        "write_file",
        serde_json::json!({ "path": "sub/notes.txt", "content": "alpha\nbeta\n" }),
    )
    .await;
    assert!(w.ok, "{:?}", w.failure);

    let r = run(
        &ctx,
        &tools,
        "read_file",
        serde_json::json!({ "path": "sub/notes.txt" }),
    )
    .await;
    assert_eq!(r.content, "alpha\nbeta\n");
    assert_eq!(r.cites.len(), 1, "a read cites the file it read (P2-D26)");

    let e = run(
        &ctx,
        &tools,
        "edit_file",
        serde_json::json!({ "path": "sub/notes.txt", "old": "beta", "new": "gamma" }),
    )
    .await;
    assert!(e.ok, "{:?}", e.failure);
    let after = std::fs::read_to_string(dir.path().join("sub/notes.txt")).unwrap();
    assert_eq!(after, "alpha\ngamma\n");

    // A non-unique `old` is refused rather than guessed at.
    std::fs::write(dir.path().join("dup.txt"), "x\nx\n").unwrap();
    let bad = run(
        &ctx,
        &tools,
        "edit_file",
        serde_json::json!({ "path": "dup.txt", "old": "x", "new": "y" }),
    )
    .await;
    assert!(!bad.ok);
    assert!(bad.failure.unwrap().message.contains("2 times"));
}

#[tokio::test]
async fn glob_and_grep_find_what_is_there() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn main() {}\n// needle\n").unwrap();
    std::fs::write(dir.path().join("src/b.rs"), "fn other() {}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "needle in the docs\n").unwrap();
    let ctx = ctx();
    let tools = registry(&ctx, cfg(&dir)).await;

    let g = run(
        &ctx,
        &tools,
        "glob",
        serde_json::json!({ "pattern": "**/*.rs" }),
    )
    .await;
    assert_eq!(g.content, "src/a.rs\nsrc/b.rs");

    let gr = run(
        &ctx,
        &tools,
        "grep",
        serde_json::json!({ "pattern": "needle" }),
    )
    .await;
    let lines: Vec<&str> = gr.content.lines().collect();
    assert_eq!(
        lines,
        vec!["README.md:1:needle in the docs", "src/a.rs:2:// needle"]
    );
    assert_eq!(gr.cites.len(), 2, "grep cites each file it matched in");

    let none = run(
        &ctx,
        &tools,
        "grep",
        serde_json::json!({ "pattern": "zzz" }),
    )
    .await;
    assert_eq!(none.content, "no matches");
}

#[tokio::test]
async fn bash_reports_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx();
    let tools = registry(&ctx, cfg(&dir)).await;

    let ok = run(
        &ctx,
        &tools,
        "bash",
        serde_json::json!({ "command": "echo hi" }),
    )
    .await;
    assert!(ok.content.starts_with("hi\n"), "{:?}", ok.content);
    assert!(ok.content.contains("[exit status: 0]"));

    let bad = run(
        &ctx,
        &tools,
        "bash",
        serde_json::json!({ "command": "exit 3" }),
    )
    .await;
    assert!(bad.content.contains("[exit status: 3]"));
    assert_eq!(bad.value, Some(serde_json::json!({ "exit_code": 3 })));
    assert!(
        bad.cites.is_empty(),
        "a shell result is not evidence (P2-D26)"
    );
}

#[tokio::test]
async fn bash_times_out_at_its_configured_bound() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = (*cfg(&dir)).clone();
    c.bash_timeout_ms = 150;
    let ctx = ctx();
    let tools = registry(&ctx, Arc::new(c)).await;

    let out = run(
        &ctx,
        &tools,
        "bash",
        serde_json::json!({ "command": "sleep 5" }),
    )
    .await;
    assert!(!out.ok);
    assert_eq!(out.failure.as_ref().unwrap().kind, FailureClass::Timeout);
    assert!(out.failure.unwrap().message.contains("150ms"));
}

#[tokio::test]
async fn a_path_outside_the_root_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx();
    let tools = registry(&ctx, cfg(&dir)).await;

    for (tool, args) in [
        ("read_file", serde_json::json!({ "path": "../escape.txt" })),
        (
            "write_file",
            serde_json::json!({ "path": "/etc/bough-should-not-write", "content": "no" }),
        ),
    ] {
        let out = run(&ctx, &tools, tool, args).await;
        assert!(!out.ok, "{tool} must refuse");
        assert_eq!(out.failure.as_ref().unwrap().kind, FailureClass::Denied);
        assert!(
            out.failure
                .unwrap()
                .message
                .contains("outside the tool root"),
            "{tool} names the root in its refusal"
        );
    }
    assert!(!PathBuf::from("/etc/bough-should-not-write").exists());
}

#[tokio::test]
async fn a_denied_glob_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
    let mut c = (*cfg(&dir)).clone();
    c.deny_globs = vec!["*.env".into(), ".env".into()];
    let ctx = ctx();
    let tools = registry(&ctx, Arc::new(c)).await;

    let out = run(
        &ctx,
        &tools,
        "read_file",
        serde_json::json!({ "path": ".env" }),
    )
    .await;
    assert_eq!(out.failure.as_ref().unwrap().kind, FailureClass::Denied);
    assert!(out.failure.unwrap().message.contains("denied glob"));
}

#[tokio::test]
async fn oversized_output_spills_and_leaves_a_locator_inline() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = (*cfg(&dir)).clone();
    c.max_output_bytes = 200;
    let cfg = Arc::new(c);
    let ctx = ctx();
    let tools = registry(&ctx, cfg.clone()).await;

    // The row's own listener, registered exactly as `apply` does.
    let max = cfg.max_output_bytes;
    ctx.on_waterfall::<bough_plugin_tools::ToolsPostExecute, _, _>(
        move |mut post: PostExecute, next| async move {
            spill::spill_if_oversized(max, &mut post);
            next.run(post).await
        },
    )
    .await
    .unwrap();

    let out = run(
        &ctx,
        &tools,
        "bash",
        serde_json::json!({ "command": "for i in $(seq 1 400); do echo 0123456789; done" }),
    )
    .await;

    assert!(
        out.content.len() < 400,
        "the model sees a BOUNDED result, not the whole output"
    );
    let marker = out
        .content
        .lines()
        .last()
        .expect("a locator line")
        .to_string();
    assert!(marker.contains("[output spilled:"), "{marker}");
    let path = marker
        .rsplit("full output at ")
        .next()
        .unwrap()
        .trim_end_matches(']')
        .to_string();
    let spilled = std::fs::read_to_string(&path).expect("the locator names a real file");
    assert!(
        spilled.lines().count() > 400,
        "the spill file holds the WHOLE output"
    );
    assert!(spilled.contains("[exit status: 0]"));
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_tool_honours_its_cancellation_signal() {
    // Dispatch happens through the seam in every other case; here the tool is driven directly so
    // the cancellation path is observed without a wrapper.
    let dir = tempfile::tempdir().unwrap();
    let bash = bough_plugin_tools_baseline::Bash(cfg(&dir));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let out: Result<ToolOutcome, _> = bough_plugin_tools::Tool::call(
        &bash,
        Arc::new(call("bash", serde_json::json!({ "command": "sleep 5" }))),
        ToolCx {
            ctx: ctx(),
            cancel,
            deadline: None,
            initiator: None,
        },
    )
    .await;
    assert_eq!(out.unwrap_err().kind, FailureClass::Cancelled);
}
