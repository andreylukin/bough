//! P3-D12, the composition half: `FocusPane::lines` draws NO STEP TWICE.
//!
//! `stream.rs` covers the `trailing_text` length rule in isolation. What this file covers is the
//! rule the crate's own module comment states and the review found broken: the durable
//! `thought/text` flushes of ONE step index concatenate, so exactly ONE of them may be painted.
//! Every answer that streams for longer than the loop's `text_flush_ms` writes two or more of
//! them, and painting each with its own chunk drew `chunk1` and then `chunk1 + chunk2`.

use std::sync::Arc;

use bough_plugin_ledger::{StepId, WakeId};
use bough_plugin_tui_focus::{FocusConfig, FocusPane, FocusState, LiveText, Row};
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

fn text_row(n: u64, index: u32, text: &str) -> Row {
    Row::Text {
        step: StepId::new(format!("s{n}")),
        wake: WakeId::new("w1"),
        index,
        text: text.to_string(),
    }
}

fn painted(rows: Vec<Row>, live: &str) -> Vec<String> {
    let state = FocusState {
        rows,
        ..Default::default()
    };
    let live = LiveText {
        agent: None,
        text: live.to_string(),
    };
    let pane = FocusPane::new(
        cfg(),
        Arc::new(Mutex::new(FocusState::default())),
        Arc::new(Mutex::new(LiveText::default())),
    );
    let (lines, _) = pane.lines(&state, &live, 80, &Theme::of(ThemeName::Dark));
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// The regression: two flushes of one step index are ONE paragraph on screen, not the first chunk
/// followed by the concatenation of both.
#[test]
fn two_flushes_of_one_step_are_painted_once() {
    let out = painted(vec![text_row(1, 0, "Hello "), text_row(2, 0, "world")], "");
    assert_eq!(
        out,
        vec!["Hello world".to_string()],
        "the flushes of one step index concatenate into ONE painted paragraph"
    );
}

/// Six flushes, the shape a real paragraph from a model takes at a 400ms flush cadence.
#[test]
fn a_long_answer_flushed_many_times_is_painted_once() {
    let chunks = ["one ", "two ", "three ", "four ", "five ", "six"];
    let rows: Vec<Row> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| text_row(i as u64 + 1, 0, c))
        .collect();
    assert_eq!(painted(rows, ""), vec!["one two three four five six"]);
}

/// A previous step's text is SETTLED and is drawn from the ledger, whole and separately: the
/// concatenation rule applies to the trailing step index only.
#[test]
fn an_earlier_step_index_is_still_drawn_on_its_own() {
    let out = painted(
        vec![
            text_row(1, 0, "first answer"),
            text_row(2, 1, "second "),
            text_row(3, 1, "answer"),
        ],
        "",
    );
    assert_eq!(out, vec!["first answer", "second answer"]);
}

/// Mid-stream: the live tail is ahead of the durable concatenation, so the tail is the one thing
/// painted — never the tail AND an earlier flush.
#[test]
fn the_live_tail_supersedes_the_whole_trailing_group() {
    let out = painted(
        vec![text_row(1, 0, "Hello "), text_row(2, 0, "world")],
        "Hello world and then some",
    );
    assert_eq!(out, vec!["Hello world and then some"]);
}
