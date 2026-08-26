//! The three §9 render intents, asserted on the lines they produce.

mod common;

use bough_plugin_tools::{RenderIntent, ToolOutcomeKind, ToolResultBody};
use bough_plugin_tools::{ToolCallId, ToolName};
use bough_plugin_tui_render::{tool_body, tool_header, ToolCallView};
use common::{colors, text, theme};

fn result(
    content: &str,
    outcome: ToolOutcomeKind,
    value: Option<serde_json::Value>,
) -> ToolResultBody {
    ToolResultBody {
        call: ToolCallId::new("c1"),
        name: ToolName::new("t"),
        outcome,
        content: content.to_string(),
        value,
        attached: Vec::new(),
        concludes_wake: false,
        step_index: 1,
    }
}

fn view<'a>(
    name: &'a str,
    intent: RenderIntent,
    args: &'a serde_json::Value,
    result: Option<&'a ToolResultBody>,
    width: u16,
    theme: &'a bough_plugin_tui_shell::Theme,
) -> ToolCallView<'a> {
    ToolCallView {
        name,
        intent,
        args,
        result,
        expanded: true,
        width,
        theme,
    }
}

#[test]
fn generic_renders_sorted_key_values_and_the_result_content() {
    let th = theme();
    let args = serde_json::json!({ "zebra": "last", "alpha": "first", "path": "/tmp/x" });
    let r = result("42 matches", ToolOutcomeKind::Ok, None);
    let lines = tool_body(
        &view("grep", RenderIntent::Generic, &args, Some(&r), 60, &th),
        50,
    );
    let rendered: Vec<String> = lines.iter().map(text).collect();
    assert_eq!(rendered[0], "alpha: first");
    assert_eq!(rendered[1], "path: /tmp/x");
    assert_eq!(rendered[2], "zebra: last");
    assert_eq!(rendered[3], "");
    assert_eq!(rendered[4], "42 matches");
}

#[test]
fn terminal_renders_output_monospace_with_the_exit_code() {
    let th = theme();
    let args = serde_json::json!({ "command": "ls -la" });
    let r = result(
        "a.txt\nb.txt",
        ToolOutcomeKind::Ok,
        Some(serde_json::json!({ "exit_code": 0 })),
    );
    let lines = tool_body(
        &view("bash", RenderIntent::Terminal, &args, Some(&r), 40, &th),
        50,
    );
    let rendered: Vec<String> = lines.iter().map(text).collect();
    assert_eq!(rendered, vec!["a.txt", "b.txt", "exit 0"]);
    assert_eq!(colors(lines.last().unwrap()), vec![Some(th.dim)]);

    let bad = result(
        "boom",
        ToolOutcomeKind::Error,
        Some(serde_json::json!({ "exit_code": 2 })),
    );
    let lines = tool_body(
        &view("bash", RenderIntent::Terminal, &args, Some(&bad), 40, &th),
        50,
    );
    assert_eq!(text(lines.last().unwrap()), "exit 2");
    assert_eq!(colors(lines.last().unwrap()), vec![Some(th.error)]);
}

#[test]
fn terminal_strips_ansi_from_output() {
    let th = theme();
    let args = serde_json::json!({ "command": "ls --color" });
    let r = result(
        "\u{1b}[31mred\u{1b}[0m plain\u{1b}]0;title\u{7}\u{1b}[1;32mgreen\u{1b}[m",
        ToolOutcomeKind::Ok,
        Some(serde_json::json!({ "exit_code": 0 })),
    );
    let lines = tool_body(
        &view("bash", RenderIntent::Terminal, &args, Some(&r), 60, &th),
        50,
    );
    let joined: String = lines.iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(!joined.contains('\u{1b}'), "{joined:?} still has escapes");
    assert!(joined.starts_with("red plaingreen"), "{joined:?}");
}

#[test]
fn diff_renders_added_and_removed_lines_with_the_theme_roles() {
    let th = theme();
    // No path ⇒ no syntax ⇒ the body itself carries the role colour.
    let args = serde_json::json!({ "old": "one\ntwo\n", "new": "one\nTWO\n" });
    let lines = tool_body(
        &view("edit_file", RenderIntent::Diff, &args, None, 40, &th),
        50,
    );
    let removed = lines
        .iter()
        .find(|l| text(l).starts_with('-'))
        .expect("a removed line");
    let added = lines
        .iter()
        .find(|l| text(l).starts_with('+'))
        .expect("an added line");
    assert_eq!(text(removed), "-two");
    assert_eq!(text(added), "+TWO");
    assert!(colors(removed).iter().all(|c| *c == Some(th.removed)));
    assert!(colors(added).iter().all(|c| *c == Some(th.added)));
}

#[test]
fn diff_highlights_by_the_paths_extension() {
    let th = theme();
    let rust = serde_json::json!({
        "path": "src/main.rs",
        "old": "fn main() {}\n",
        "new": "fn main() { let x = 1; }\n",
    });
    let unknown = serde_json::json!({
        "path": "notes.zzzz",
        "old": "fn main() {}\n",
        "new": "fn main() { let x = 1; }\n",
    });
    let hl = tool_body(
        &view("edit_file", RenderIntent::Diff, &rust, None, 60, &th),
        50,
    );
    let plain = tool_body(
        &view("edit_file", RenderIntent::Diff, &unknown, None, 60, &th),
        50,
    );
    let hl_added = hl.iter().find(|l| text(l).starts_with('+')).unwrap();
    let plain_added = plain.iter().find(|l| text(l).starts_with('+')).unwrap();
    assert_eq!(text(hl_added), text(plain_added));
    assert!(
        hl_added.spans.len() > plain_added.spans.len(),
        "a known extension must produce more than one styled run: {:?}",
        hl_added.spans
    );
    // The unknown extension is not guessed at: every run keeps the `added` role.
    assert!(colors(plain_added).iter().all(|c| *c == Some(th.added)));
}

#[test]
fn a_diff_intent_with_unmatched_args_falls_back_to_generic() {
    let th = theme();
    let args = serde_json::json!({ "file": "x.rs", "patch": "@@" });
    let lines = tool_body(
        &view("weird_edit", RenderIntent::Diff, &args, None, 60, &th),
        50,
    );
    assert!(!lines.is_empty(), "the fallback is never nothing");
    assert!(
        text(&lines[0]).contains("not recognised"),
        "{:?}",
        text(&lines[0])
    );
    assert_eq!(colors(&lines[0]), vec![Some(th.dim)]);
    let rest: Vec<String> = lines[1..].iter().map(text).collect();
    assert_eq!(rest, vec!["file: x.rs", "patch: @@"]);
}

#[test]
fn a_body_over_max_lines_ends_in_a_fold_marker_not_a_truncation() {
    let th = theme();
    let args = serde_json::json!({ "command": "seq 20" });
    let out: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let r = result(
        &out,
        ToolOutcomeKind::Ok,
        Some(serde_json::json!({ "exit_code": 0 })),
    );
    let full = tool_body(
        &view("bash", RenderIntent::Terminal, &args, Some(&r), 40, &th),
        1000,
    );
    let folded = tool_body(
        &view("bash", RenderIntent::Terminal, &args, Some(&r), 40, &th),
        5,
    );
    assert_eq!(folded.len(), 5);
    let last = text(folded.last().unwrap());
    assert_eq!(last, format!("… {} more", full.len() - 4));
    assert_eq!(colors(folded.last().unwrap()), vec![Some(th.dim)]);
    // The head is intact — a fold is not a truncation of the visible lines.
    for i in 0..4 {
        assert_eq!(text(&folded[i]), text(&full[i]));
    }
}

#[test]
fn the_collapsed_header_is_exactly_one_line_at_every_width() {
    let th = theme();
    let args = serde_json::json!({ "command": "ls -la /very/long/path/that/does/not/fit" });
    let r = result("ok", ToolOutcomeKind::Ok, None);
    for width in 0u16..=120 {
        let v = ToolCallView {
            name: "bash",
            intent: RenderIntent::Terminal,
            args: &args,
            result: Some(&r),
            expanded: false,
            width,
            theme: &th,
        };
        let line = tool_header(&v);
        let w = line.width();
        assert!(
            w <= width as usize,
            "width {width}: header is {w} columns: {:?}",
            text(&line)
        );
        assert!(!text(&line).contains('\n'), "width {width}: header wrapped");
        if width >= 8 {
            assert_eq!(w, width as usize, "width {width}: header must fill the row");
        }
    }
}
