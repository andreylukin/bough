//! WP-7 / phase codemode §5: the program row. One `run` call, its `program/*` steps and its
//! `tool/result` are ONE row — the pane's "no step is rendered twice" rule, extended to the
//! surface code mode shows. These drive the pure projection and the pure render: no ledger, no
//! terminal, no model.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::{Class, Seq, Step, StepId, StepType, TrajId, WakeId};
use bough_plugin_llm::ToolCallId;
use bough_plugin_tui_focus::program::{ProgramView, RUN_TOOL};
use bough_plugin_tui_focus::{program_lines, rows_from_steps, Expanded, Row};
use bough_plugin_tui_shell::{Theme, ThemeName};

fn step(n: u64, kind: &str, body: serde_json::Value) -> Step {
    Step {
        id: StepId::new(format!("s{n}")),
        traj: TrajId::new("lane/sol"),
        seq: Seq(n),
        at: chrono::Utc::now(),
        wake: WakeId::new("w1"),
        kind: StepType::new(kind),
        class: Class::Thought,
        body: Arc::new(body),
        cites: Arc::new(vec![]),
        refs: Arc::new(BTreeSet::new()),
        ignorable: false,
    }
}

const SOURCE: &str = "const out = await bash('ls', ['fs:list']);\nconsole.log(out);";

/// The `run` call, `n` inner calls with their results, one console chunk and the closing result.
fn program_steps(n: u32) -> Vec<Step> {
    let mut steps = vec![step(
        1,
        "tool/call",
        serde_json::json!({
            "call": "c1", "name": RUN_TOOL,
            "args": { "program": SOURCE }, "render": "generic", "step_index": 0
        }),
    )];
    let mut seq = 2u64;
    for i in 0..n {
        steps.push(step(
            seq,
            "program/call",
            serde_json::json!({
                "program": "c1", "index": i, "call": format!("c1.{i}"),
                "name": "bash", "args": { "cmd": "ls" }, "render": "terminal",
                "tags": ["fs:list"], "step_index": 0
            }),
        ));
        seq += 1;
        steps.push(step(
            seq,
            "program/result",
            serde_json::json!({
                "program": "c1", "index": i, "call": format!("c1.{i}"),
                "name": "bash", "outcome": "ok", "content": "a\nb",
                "step_index": 0, "ms": 12
            }),
        ));
        seq += 1;
    }
    steps.push(step(
        seq,
        "program/console",
        serde_json::json!({ "program": "c1", "chunk": 0, "text": "hello from the program\n" }),
    ));
    seq += 1;
    steps.push(step(
        seq,
        "tool/result",
        serde_json::json!({
            "call": "c1", "name": RUN_TOOL, "outcome": "ok",
            "content": "hello from the program\n",
            "value": { "ms": 1200, "ops": 4200 }, "step_index": 0
        }),
    ));
    steps
}

fn theme() -> Theme {
    Theme::of(ThemeName::Dark)
}

fn view<'a>(row: &'a Row, expanded: &'a Expanded, width: u16, theme: &'a Theme) -> ProgramView<'a> {
    let Row::Program {
        call,
        source,
        console,
        subs,
        result,
        error,
        ms,
        ..
    } = row
    else {
        panic!("expected a Program row, got {row:?}");
    };
    ProgramView {
        call,
        source,
        console,
        subs,
        result: result.as_ref(),
        error: error.as_ref(),
        ms: *ms,
        expanded,
        width,
        theme,
        max_tool_lines: 20,
    }
}

fn text_of(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// The fold: one program, one row, and no step left over as an orphan.
#[test]
fn a_program_with_four_sub_calls_folds_into_one_row() {
    let steps = program_steps(4);
    let rows = rows_from_steps(&steps);
    assert_eq!(rows.len(), 1, "one program = one row: {rows:?}");
    let Row::Program {
        call,
        source,
        console,
        subs,
        result,
        ms,
        parts,
        ..
    } = &rows[0]
    else {
        panic!("expected a Program row, got {:?}", rows[0]);
    };
    assert_eq!(*call, ToolCallId::new("c1"));
    assert_eq!(source, SOURCE);
    assert_eq!(console, "hello from the program\n");
    assert_eq!(subs.len(), 4);
    assert_eq!(subs[2].call, ToolCallId::new("c1.2"));
    assert_eq!(subs[2].name, "bash");
    assert_eq!(
        subs[2]
            .result
            .as_ref()
            .expect("the sub result folded in")
            .content,
        "a\nb"
    );
    assert_eq!(
        result.as_ref().expect("the run result folded in").outcome,
        {
            use bough_plugin_tools::ToolOutcomeKind;
            ToolOutcomeKind::Ok
        }
    );
    // The duration comes off the closing result's value; the collapsed line shows it.
    assert_eq!(*ms, 1200);
    // ZERO ORPHANS: every step in the input is folded into this one row, exactly once.
    let folded: BTreeSet<StepId> = parts.iter().cloned().collect();
    assert_eq!(folded.len(), parts.len(), "a step folded twice: {parts:?}");
    let all: BTreeSet<StepId> = steps.iter().map(|s| s.id.clone()).collect();
    assert_eq!(folded, all, "every step belongs to the program row");
}

/// A program that called nothing is still one row: the source and the console are the point.
#[test]
fn a_program_with_no_sub_calls_folds_into_one_row() {
    let rows = rows_from_steps(&program_steps(0));
    assert_eq!(rows.len(), 1, "{rows:?}");
    let Row::Program { subs, console, .. } = &rows[0] else {
        panic!("expected a Program row, got {:?}", rows[0]);
    };
    assert!(subs.is_empty());
    assert_eq!(console, "hello from the program\n");
}

/// A `program/error` is the one terminal error a program can end with, and it renders as the
/// TYPED line — the tag the model was given, not a bare "error".
#[test]
fn a_program_error_renders_the_typed_error_line() {
    let mut steps = vec![step(
        1,
        "tool/call",
        serde_json::json!({
            "call": "c1", "name": RUN_TOOL,
            "args": { "program": SOURCE }, "render": "generic", "step_index": 0
        }),
    )];
    steps.push(step(
        2,
        "program/error",
        serde_json::json!({
            "program": "c1", "ops": 900, "ms": 5000,
            "error": { "kind": "time_exceeded", "ms": 5000 }
        }),
    ));
    let rows = rows_from_steps(&steps);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let Row::Program { error, ops, ms, .. } = &rows[0] else {
        panic!("expected a Program row, got {:?}", rows[0]);
    };
    assert_eq!(
        error.as_ref().expect("the error folded in").kind,
        "time_exceeded"
    );
    assert_eq!(*ops, 900);
    assert_eq!(*ms, 5000);

    let theme = theme();
    let mut expanded = Expanded::new();
    expanded.insert(&ToolCallId::new("c1"));
    let (lines, _) = program_lines(&view(&rows[0], &expanded, 60, &theme));
    let text = text_of(&lines);
    assert!(
        text.iter().any(|l| l.contains("time_exceeded")),
        "the typed error line is missing: {text:?}"
    );
}

/// TOTAL. An unknown `program/*` kind, a sub-step whose program paged out, and a body missing its
/// declared fields all render as `Other` — and none of them panics.
#[test]
fn unknown_and_orphaned_sub_steps_render_as_other() {
    let rows = rows_from_steps(&[
        step(
            1,
            "tool/call",
            serde_json::json!({
                "call": "c1", "name": RUN_TOOL,
                "args": { "program": SOURCE }, "render": "generic", "step_index": 0
            }),
        ),
        // A kind this binary does not own. The step-type map is merge-extensible (§3).
        step(
            2,
            "program/heartbeat",
            serde_json::json!({ "program": "c1", "beat": 1 }),
        ),
        // A `program/call` whose program is not in this window.
        step(
            3,
            "program/call",
            serde_json::json!({
                "program": "cZZ", "index": 0, "call": "cZZ.0",
                "name": "bash", "args": {}, "render": "generic", "step_index": 0
            }),
        ),
        // A `program/console` with no text.
        step(4, "program/console", serde_json::json!({ "program": "c1" })),
    ]);
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert!(matches!(rows[0], Row::Program { .. }));
    for row in &rows[1..] {
        assert!(matches!(row, Row::Other { .. }), "{row:?}");
    }
    // The program row folded nothing but its own call.
    let Row::Program { subs, console, .. } = &rows[0] else {
        unreachable!()
    };
    assert!(subs.is_empty());
    assert!(console.is_empty());
}

/// The collapsed row is ONE line and NEVER WIDER than the pane at any width — the guarantee
/// `tool_header` gives every other row, which is what stops a program row from jittering against
/// its neighbours on a narrow terminal.
///
/// MERGE (ux-visual pass A, visual audit F7): the header no longer PADS to the pane's width. The
/// outcome glyph used to be flush against the far edge, fifty columns from the tool it belonged
/// to; it now sits right after the arguments. The program row is built through `tool_header`, so
/// it inherits that convention rather than restating it — the claim here is the one that still
/// holds for every tool row.
#[test]
fn the_collapsed_line_is_one_line_of_stable_width() {
    let rows = rows_from_steps(&program_steps(4));
    let theme = theme();
    let expanded = Expanded::new();
    for width in [10u16, 24, 40, 80, 200] {
        let (lines, headers) = program_lines(&view(&rows[0], &expanded, width, &theme));
        assert_eq!(lines.len(), 1, "collapsed is one line at width {width}");
        assert_eq!(
            headers.len(),
            1,
            "only the program's own header is clickable"
        );
        assert_eq!(headers[0].1, 0);
        assert!(
            lines[0].width() <= width as usize,
            "width {width}: the collapsed line overflows the pane: {:?}",
            text_of(&lines)
        );
    }
    // At a comfortable width the gist NAMES the calls (the TUI brief, D2), then the duration.
    let (lines, _) = program_lines(&view(&rows[0], &expanded, 60, &theme));
    let text = text_of(&lines).remove(0);
    assert!(text.contains("program"), "{text:?}");
    assert!(text.contains("bash ls, bash ls"), "{text:?}");
    assert!(!text.contains("4 calls"), "{text:?}");
    assert!(text.contains("1.2s"), "{text:?}");
    // Narrower: the calls that do not fit are grouped by verb; narrower still, the bare count.
    let (lines, _) = program_lines(&view(&rows[0], &expanded, 30, &theme));
    let text = text_of(&lines).remove(0);
    assert!(
        text.contains("4 bashs") || text.contains("4 calls"),
        "{text:?}"
    );
    let (lines, _) = program_lines(&view(&rows[0], &expanded, 22, &theme));
    let text = text_of(&lines).remove(0);
    assert!(text.contains("4 calls"), "{text:?}");
}

/// Expanded: the source block, the console beneath it, and one nested row per sub-call carrying
/// the ✓/✗ mark every tool row carries.
#[test]
fn expanded_shows_the_source_then_the_console_then_the_nested_rows() {
    let rows = rows_from_steps(&program_steps(4));
    let theme = theme();
    let mut expanded = Expanded::new();
    expanded.insert(&ToolCallId::new("c1"));
    let (lines, headers) = program_lines(&view(&rows[0], &expanded, 60, &theme));
    let text = text_of(&lines);

    let source_at = text
        .iter()
        .position(|l| l.contains("await bash"))
        .unwrap_or_else(|| panic!("the JS source block is missing: {text:?}"));
    let console_at = text
        .iter()
        .position(|l| l.contains("hello from the program"))
        .expect("the console output");
    assert!(
        source_at < console_at,
        "the console output sits UNDER the source: {text:?}"
    );

    // One header for the program plus one per sub-call, and the sub headers are all below the
    // console.
    assert_eq!(headers.len(), 5, "{headers:?}");
    for (call, line) in headers.iter().skip(1) {
        assert!(call.to_string().starts_with("c1."), "{call}");
        assert!(*line as usize > console_at, "sub rows are last: {text:?}");
    }
    let marks = text.iter().filter(|l| l.contains('\u{2713}')).count();
    assert!(marks >= 4, "every sub-call carries its ✓: {text:?}");
}

/// Closing a program closes what was nested inside it: an inner call is `{program}.{n}`, so
/// reopening the row shows the one line it promises rather than whatever had been open before.
#[test]
fn collapsing_a_program_collapses_its_sub_rows() {
    let program = ToolCallId::new("c1");
    let mut expanded = Expanded::new();
    assert!(expanded.toggle(&program));
    expanded.insert(&ToolCallId::new("c1.2"));
    assert!(expanded.is_expanded(&ToolCallId::new("c1.2")));
    assert!(!expanded.toggle(&program), "the second press closes it");
    assert!(expanded.is_empty(), "the sub-rows closed with it");

    // A plain tool call has no nested ids, so this is a no-op for every other row.
    let plain = ToolCallId::new("c9");
    let other = ToolCallId::new("c90");
    expanded.insert(&plain);
    expanded.insert(&other);
    expanded.toggle(&plain);
    assert!(expanded.is_expanded(&other), "a prefix is not a parent");
}
