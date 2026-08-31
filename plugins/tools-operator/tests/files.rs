//! The three file verbs on a real tempdir, driven through the `tools` pipeline exactly as the
//! model would reach them: `view` records what it rendered, `patch` refuses anything it was not
//! shown, `write` echoes a tag the next `patch` can chain onto, and a path outside the workspace
//! is `Denied`.

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{AgentName, WakeId};
use bough_plugin_tools::{
    FailureClass, ToolCall, ToolCallId, ToolName, ToolResult, ToolsHandle, WorkspaceRoot,
};
use bough_plugin_tools_operator::files::{tag_of, SeenFiles};
use bough_plugin_tools_operator::OperatorConfig;
use tempfile::TempDir;

fn cfg() -> Arc<OperatorConfig> {
    Arc::new(OperatorConfig {
        max_view_bytes: 1_000_000,
        max_files_per_patch: 8,
        bg_log_dir: PathBuf::from("/tmp"),
        bg_max: 4,
        bg_poll_ms: 100,
        ledger_page: 50,
        schedule_max_horizon_days: 30,
        schedule_tick_ms: 1_000,
        sh_max_legs: 8,
        sh_timeout_ms: 120_000,
        sh_tags_min: 3,
        sh_tags_max: 5,
    })
}

/// The root a row actually holds is PINNED: absolute and canonical. `TempDir::path` is neither on
/// macOS, where `/var` is a symlink to `/private/var`.
fn root(dir: &TempDir) -> WorkspaceRoot {
    WorkspaceRoot::new(dir.path().canonicalize().unwrap()).unwrap()
}

struct Fx {
    dir: TempDir,
    ctx: Context,
    tools: ToolsHandle,
}

impl Fx {
    async fn new(files: &[(&str, &str)]) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        for (p, text) in files {
            let full = dir.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        let ctx = Context::root(KernelCore::new());
        let tools = ToolsHandle::with_limits(4, 10_000);
        let seen = Arc::new(SeenFiles::default());
        for spec in bough_plugin_tools_operator::files::specs(cfg(), root(&dir), seen) {
            tools.register(&ctx, spec).await.unwrap();
        }
        Fx { dir, ctx, tools }
    }

    async fn run(&self, agent: &str, name: &str, args: serde_json::Value) -> ToolResult {
        self.tools
            .execute(
                &self.ctx,
                vec![ToolCall {
                    id: ToolCallId::new(format!("c-{name}")),
                    name: ToolName::new(name),
                    args,
                    agent: AgentName::new(agent),
                    wake: WakeId::new("w1"),
                    step_index: 1,
                }],
            )
            .await
            .pop()
            .expect("one call, one result")
    }

    async fn view(&self, path: &str) -> ToolResult {
        self.run("lane", "view", serde_json::json!({ "path": path }))
            .await
    }

    async fn patch(&self, input: &str) -> ToolResult {
        self.run("lane", "patch", serde_json::json!({ "patch": input }))
            .await
    }

    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(p)).unwrap()
    }

    fn put(&self, p: &str, text: &str) {
        std::fs::write(self.dir.path().join(p), text).unwrap();
    }
}

fn doc(lines: &[&str]) -> String {
    format!("{}\n", lines.join("\n"))
}

// ---------------------------------------------------------------------------
// view
// ---------------------------------------------------------------------------

#[tokio::test]
async fn view_returns_the_anchor_and_numbered_lines() {
    let text = doc(&["alpha", "beta"]);
    let fx = Fx::new(&[("a.rs", &text)]).await;
    let r = fx.view("a.rs").await;
    assert!(r.ok, "{:?}", r.failure);
    assert_eq!(
        r.content,
        format!("[a.rs#{}]\n1:alpha\n2:beta", tag_of(&text))
    );
    // A view is EVIDENCE: it cites the file it rendered.
    assert_eq!(r.cites.len(), 1);
}

#[tokio::test]
async fn a_crlf_file_views_with_the_same_tag_as_its_lf_twin() {
    let fx = Fx::new(&[("crlf.rs", "alpha\r\nbeta\r\n"), ("lf.rs", "alpha\nbeta\n")]).await;
    let a = fx.view("crlf.rs").await;
    let b = fx.view("lf.rs").await;
    let tag = |c: &str| {
        c.lines()
            .next()
            .unwrap()
            .rsplit('#')
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(tag(&a.content), tag(&b.content));
}

#[tokio::test]
async fn an_empty_file_says_how_to_start_it() {
    let fx = Fx::new(&[("empty.rs", "")]).await;
    let r = fx.view("empty.rs").await;
    assert!(r.content.contains("INS.HEAD"), "{}", r.content);
}

#[tokio::test]
async fn a_missing_file_is_not_found_and_a_directory_is_named_as_one() {
    let fx = Fx::new(&[("a.rs", "x\n")]).await;
    let missing = fx.view("nope.rs").await;
    assert_eq!(
        missing.failure.as_ref().map(|f| f.kind),
        Some(FailureClass::NotFound),
        "{missing:?}"
    );
    std::fs::create_dir_all(fx.dir.path().join("sub")).unwrap();
    let dir = fx.view("sub").await;
    assert!(!dir.ok);
    assert!(dir.failure.unwrap().message.contains("directory"));
}

#[tokio::test]
async fn a_path_outside_the_workspace_is_denied() {
    let fx = Fx::new(&[("a.rs", "x\n")]).await;
    for r in [
        fx.view("../escape.rs").await,
        fx.view("/etc/hosts").await,
        fx.run(
            "lane",
            "write",
            serde_json::json!({ "path": "../escape.rs", "content": "x" }),
        )
        .await,
    ] {
        assert_eq!(
            r.failure.as_ref().map(|f| f.kind),
            Some(FailureClass::Denied),
            "{r:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// patch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_viewed_file_patches_in_viewed_coordinates_and_echoes_the_new_tag() {
    let fx = Fx::new(&[("a.rs", &doc(&["a", "b", "c"]))]).await;
    fx.view("a.rs").await;
    let r = fx.patch("[a.rs#]\nINS.PRE 1:\n+top\n\nSWAP 3:\n+C\n").await;
    assert!(r.ok, "{:?}", r.failure);
    let after = doc(&["top", "a", "b", "C"]);
    assert_eq!(fx.read("a.rs"), after);
    assert!(
        r.content.contains(&format!("[a.rs#{}]", tag_of(&after))),
        "{}",
        r.content
    );
    assert!(r.content.contains("2 operations"), "{}", r.content);
}

#[tokio::test]
async fn an_explicit_tag_from_the_view_is_accepted() {
    let text = doc(&["a"]);
    let fx = Fx::new(&[("a.rs", &text)]).await;
    fx.view("a.rs").await;
    let r = fx
        .patch(&format!("[a.rs#{}]\nSWAP 1:\n+A\n", tag_of(&text)))
        .await;
    assert!(r.ok, "{:?}", r.failure);
    assert_eq!(fx.read("a.rs"), doc(&["A"]));
}

#[tokio::test]
async fn a_stale_tag_is_refused_and_nothing_is_written() {
    let text = doc(&["a"]);
    let fx = Fx::new(&[("a.rs", &text)]).await;
    fx.view("a.rs").await;
    let wrong = if tag_of(&text) == "0000" {
        "FFFF"
    } else {
        "0000"
    };
    let r = fx.patch(&format!("[a.rs#{wrong}]\nSWAP 1:\n+A\n")).await;
    assert!(!r.ok);
    assert!(r.failure.unwrap().message.contains("stale tag"));
    assert_eq!(fx.read("a.rs"), text, "the file must be untouched");
}

#[tokio::test]
async fn a_file_this_agent_never_viewed_is_refused() {
    let text = doc(&["a"]);
    let fx = Fx::new(&[("a.rs", &text)]).await;
    let r = fx.patch("[a.rs#]\nSWAP 1:\n+A\n").await;
    assert!(!r.ok);
    let m = r.failure.unwrap().message;
    assert!(m.contains("no viewed version"), "{m}");
    assert_eq!(fx.read("a.rs"), text);
}

#[tokio::test]
async fn one_agents_view_does_not_license_anothers_patch() {
    let text = doc(&["a"]);
    let fx = Fx::new(&[("a.rs", &text)]).await;
    fx.run("lane", "view", serde_json::json!({ "path": "a.rs" }))
        .await;
    let r = fx
        .run(
            "other",
            "patch",
            serde_json::json!({ "patch": "[a.rs#]\nSWAP 1:\n+A\n" }),
        )
        .await;
    assert!(!r.ok, "a subagent must view a file itself before patching");
    assert_eq!(fx.read("a.rs"), text);
}

#[tokio::test]
async fn an_untouched_range_rebases_onto_a_file_that_moved_since_the_view() {
    let fx = Fx::new(&[("a.rs", &doc(&["a", "b", "c"]))]).await;
    fx.view("a.rs").await;
    // Someone else prepended two lines — nowhere near the patched range.
    fx.put("a.rs", &doc(&["header", "header2", "a", "b", "c"]));
    let r = fx.patch("[a.rs#]\nSWAP 3:\n+C\n").await;
    assert!(r.ok, "{:?}", r.failure);
    assert_eq!(
        fx.read("a.rs"),
        doc(&["header", "header2", "a", "b", "C"]),
        "both edits must land"
    );
}

#[tokio::test]
async fn a_touched_range_conflicts_and_names_the_line_range() {
    let fx = Fx::new(&[("a.rs", &doc(&["a", "b", "c"]))]).await;
    fx.view("a.rs").await;
    let moved = doc(&["a", "B!", "c"]);
    fx.put("a.rs", &moved);
    let r = fx.patch("[a.rs#]\nSWAP 2:\n+B\n").await;
    assert!(!r.ok);
    let m = r.failure.unwrap().message;
    assert!(m.contains("lines 2.=2"), "{m}");
    assert_eq!(fx.read("a.rs"), moved, "nothing may be written");
}

#[tokio::test]
async fn a_multi_file_patch_is_all_or_nothing() {
    let a = doc(&["a1", "a2"]);
    let b = doc(&["b1", "b2"]);
    let fx = Fx::new(&[("a.rs", &a), ("b.rs", &b)]).await;
    fx.view("a.rs").await;
    fx.view("b.rs").await;
    // b.rs moves under the exact line the patch replaces; a.rs's section is impeccable.
    let b_moved = doc(&["b1", "B2!"]);
    fx.put("b.rs", &b_moved);

    let r = fx
        .patch("[a.rs#]\nSWAP 1:\n+A1\n\n[b.rs#]\nSWAP 2:\n+B2\n")
        .await;
    assert!(!r.ok, "the conflict in b.rs must refuse the whole patch");
    assert_eq!(fx.read("a.rs"), a, "a.rs must be byte-identical");
    assert_eq!(fx.read("b.rs"), b_moved);
}

#[tokio::test]
async fn a_patch_chains_onto_the_tag_the_previous_patch_echoed() {
    let fx = Fx::new(&[("a.rs", &doc(&["a", "b"]))]).await;
    fx.view("a.rs").await;
    let first = fx.patch("[a.rs#]\nSWAP 1:\n+A\n").await;
    assert!(first.ok, "{:?}", first.failure);
    // No second view: the echoed tag is live.
    let second = fx.patch("[a.rs#]\nSWAP 2:\n+B\n").await;
    assert!(second.ok, "{:?}", second.failure);
    assert_eq!(fx.read("a.rs"), doc(&["A", "B"]));
}

#[tokio::test]
async fn two_spellings_of_one_path_in_one_patch_are_refused() {
    let text = doc(&["a", "b"]);
    let fx = Fx::new(&[("a.rs", &text)]).await;
    fx.view("a.rs").await;
    let r = fx
        .patch("[a.rs#]\nSWAP 1:\n+A\n\n[./a.rs#]\nSWAP 2:\n+B\n")
        .await;
    assert!(!r.ok);
    assert!(r.failure.unwrap().message.contains("same file"));
    assert_eq!(fx.read("a.rs"), text);
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_creates_a_file_and_its_tag_is_accepted_without_a_re_view() {
    let fx = Fx::new(&[]).await;
    let content = doc(&["one", "two"]);
    let w = fx
        .run(
            "lane",
            "write",
            serde_json::json!({ "path": "sub/new.rs", "content": content }),
        )
        .await;
    assert!(w.ok, "{:?}", w.failure);
    assert!(
        w.content
            .contains(&format!("[sub/new.rs#{}]", tag_of(&content))),
        "{}",
        w.content
    );
    assert_eq!(fx.read("sub/new.rs"), content);

    let r = fx
        .patch(&format!(
            "[sub/new.rs#{}]\nSWAP 2:\n+TWO\n",
            tag_of(&content)
        ))
        .await;
    assert!(r.ok, "{:?}", r.failure);
    assert_eq!(fx.read("sub/new.rs"), doc(&["one", "TWO"]));
}

#[tokio::test]
async fn write_replaces_a_file_wholesale() {
    let fx = Fx::new(&[("a.rs", &doc(&["old"]))]).await;
    let w = fx
        .run(
            "lane",
            "write",
            serde_json::json!({ "path": "a.rs", "content": "new\n" }),
        )
        .await;
    assert!(w.ok, "{:?}", w.failure);
    assert_eq!(fx.read("a.rs"), "new\n");
    assert!(w.content.contains("1 line"), "{}", w.content);
}
