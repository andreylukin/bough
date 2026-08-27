//! B6: the roving row focus. A keyboard user can reach a tool row, see which one they are on, and
//! open it — none of which was possible before this phase, at any width, with any key.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::{Class, Seq, Step, StepId, StepType, TrajId, WakeId};
use bough_plugin_tui_focus::rowfocus::{focus_marker, RowFocus};
use bough_plugin_tui_focus::{
    reveal, rows_from_steps, FocusConfig, FocusPane, FocusState, LiveText, Scroll,
};
use bough_plugin_tui_shell::{Theme, ThemeName};
use parking_lot::Mutex;

fn cfg() -> Arc<FocusConfig> {
    Arc::new(FocusConfig {
        max_rows: 100,
        max_tool_lines: 50,
        page_lines: 10,
        expand_new_tools: false,
        show_reasoning: true,
    })
}

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

fn text(n: u64, t: &str) -> Step {
    step(
        n,
        "thought/text",
        serde_json::json!({ "text": t, "step_index": n as u32 }),
    )
}

fn call(n: u64, id: &str) -> Step {
    step(
        n,
        "tool/call",
        serde_json::json!({
            "call": id, "name": "write_file", "args": { "path": "notes.txt" },
            "render": "diff", "step_index": n as u32
        }),
    )
}

// ---------------------------------------------------------------------------
// The pure state machine
// ---------------------------------------------------------------------------

#[test]
fn a_move_from_none_enters_at_the_last_row_whichever_way_it_was_pressed() {
    // A keyboard user arriving from the composer is at the BOTTOM of the conversation: that is
    // where they were reading, and it is where the newest tool call is.
    assert_eq!(RowFocus::default().moved(1, 5).index, Some(4));
    assert_eq!(RowFocus::default().moved(-1, 5).index, Some(4));
    assert_eq!(RowFocus::default().moved(1, 1).index, Some(0));
}

#[test]
fn it_clamps_at_both_ends_and_never_wraps() {
    let at_top = RowFocus { index: Some(0) };
    assert_eq!(at_top.clone().moved(-1, 5).index, Some(0), "the top holds");
    assert_eq!(at_top.moved(-99, 5).index, Some(0));

    let at_bottom = RowFocus { index: Some(4) };
    assert_eq!(
        at_bottom.clone().moved(1, 5).index,
        Some(4),
        "and so does the bottom: wrapping to the top of a conversation is disorienting"
    );
    assert_eq!(at_bottom.moved(99, 5).index, Some(4));

    assert_eq!(RowFocus { index: Some(2) }.moved(1, 5).index, Some(3));
    assert_eq!(RowFocus { index: Some(2) }.moved(-1, 5).index, Some(1));
}

#[test]
fn an_empty_transcript_has_nothing_to_focus() {
    assert_eq!(RowFocus::default().moved(1, 0).index, None);
    assert_eq!(RowFocus { index: Some(3) }.moved(-1, 0).index, None);
}

#[test]
fn a_stale_index_is_clamped_rather_than_carried() {
    // Rows are recomputed from the ledger and paged in from underneath; an index past the end
    // must land somewhere real on the next press, not panic and not stay past the end.
    assert_eq!(RowFocus { index: Some(50) }.moved(1, 3).index, Some(2));
    assert_eq!(RowFocus { index: Some(50) }.moved(-1, 3).index, Some(1));
}

#[test]
fn on_step_puts_the_keyboard_where_a_search_hit_landed() {
    let rows = rows_from_steps(&[text(1, "one"), call(2, "c2"), text(3, "three")]);
    assert_eq!(
        RowFocus::on_step(&rows, &StepId::new("s2")).index,
        Some(1),
        "the row a FocusRequest names"
    );
    assert_eq!(RowFocus::on_step(&rows, &StepId::new("nope")).index, None);
}

#[test]
fn is_on_answers_for_exactly_one_row() {
    let f = RowFocus { index: Some(2) };
    assert!(f.is_on(2));
    assert!(!f.is_on(1) && !f.is_on(3));
    assert!(!RowFocus::default().is_on(0), "`None` draws nothing");
}

#[test]
fn the_marker_is_a_glyph_and_not_only_a_colour() {
    // Audit delight 3, inverted: colour alone is not an indicator for the users who most need one.
    assert_eq!(focus_marker(), '\u{258c}');
}

// ---------------------------------------------------------------------------
// Bringing the focused row into view
// ---------------------------------------------------------------------------

#[test]
fn reveal_scrolls_only_when_the_row_is_off_screen() {
    // 100 lines, a 10-line window, anchored at 40: 40..=49 are visible.
    let at_40 = Scroll::anchored_on(40);
    assert_eq!(
        reveal(at_40, 45, 100, 10),
        at_40,
        "already visible: no move"
    );
    assert_eq!(
        reveal(at_40, 39, 100, 10).top(100, 10),
        39,
        "above: to the top"
    );
    assert_eq!(
        reveal(at_40, 55, 100, 10).top(100, 10),
        46,
        "below: the row becomes the last visible line"
    );
    assert_eq!(
        reveal(Scroll::Follow, 99, 100, 10),
        Scroll::Follow,
        "the tail is already showing the newest row: Follow survives"
    );
}

// ---------------------------------------------------------------------------
// The pane
// ---------------------------------------------------------------------------

/// The focused row paints a marker AND a fill, on the pane's own lines.
#[test]
fn the_focused_row_paints_a_marker_in_the_gutter() {
    let theme = Theme::of(ThemeName::Dark);
    let steps = vec![text(1, "one"), text(2, "two"), text(3, "three")];
    let state = FocusState {
        row_focus: RowFocus { index: Some(1) },
        ..Default::default()
    };
    let mut state = state;
    state.set_steps(steps);
    state.row_focus = RowFocus { index: Some(1) };

    let pane = FocusPane::new(
        cfg(),
        Arc::new(Mutex::new(FocusState::default())),
        Arc::new(Mutex::new(LiveText::default())),
    );
    let (lines, _, _, row_lines) = pane.lines_with_rows(&state, &LiveText::default(), 60, &theme);

    let start = row_lines[1] as usize;
    let rendered: String = lines[start]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        rendered.starts_with(&focus_marker().to_string()),
        "the focused row leads with the marker: {rendered:?}"
    );
    assert_eq!(
        lines[start].style.bg,
        Some(theme.sel_bg),
        "and carries the selection fill"
    );

    let other: String = lines[row_lines[0] as usize]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        !other.starts_with(&focus_marker().to_string()),
        "and no other row does: {other:?}"
    );
}

#[test]
fn row_lines_line_up_with_the_rows_they_belong_to() {
    let theme = Theme::of(ThemeName::Dark);
    let mut state = FocusState::default();
    state.set_steps(vec![text(1, "one"), call(2, "c2"), text(3, "three")]);

    let pane = FocusPane::new(
        cfg(),
        Arc::new(Mutex::new(FocusState::default())),
        Arc::new(Mutex::new(LiveText::default())),
    );
    let (lines, headers, _, row_lines) =
        pane.lines_with_rows(&state, &LiveText::default(), 60, &theme);

    assert_eq!(row_lines.len(), state.rows.len(), "one start per row");
    assert!(
        row_lines.windows(2).all(|w| w[0] <= w[1]),
        "and they only go forward: {row_lines:?}"
    );
    assert!(*row_lines.last().unwrap() < lines.len() as u16);
    // The tool row's own header IS the line the row starts at — which is what makes the click
    // hit-test's origin the row's origin (M26).
    assert_eq!(headers.len(), 1);
    assert_eq!(
        headers[0].1, row_lines[1],
        "the header is the row's first line"
    );
}
