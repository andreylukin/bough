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
        context: true,
        context_refresh_ms: 150,
    })
}

/// One `thought/text` step, as the loop's flush writes it. These tests go through
/// `rows_from_steps` rather than hand-building rows: since P5-D14 the flushes of one step index
/// are joined THERE, so a hand-built row list would be asserting against a shape the projection
/// no longer produces.
fn text_step(n: u64, index: u32, text: &str) -> bough_plugin_ledger::Step {
    bough_plugin_ledger::Step {
        id: StepId::new(format!("s{n}")),
        traj: bough_plugin_ledger::TrajId::new("lane/sol"),
        seq: bough_plugin_ledger::Seq(n),
        at: chrono::Utc::now(),
        wake: WakeId::new("w1"),
        kind: bough_plugin_ledger::StepType::new("thought/text"),
        class: bough_plugin_ledger::Class::Thought,
        body: Arc::new(serde_json::json!({ "text": text, "step_index": index })),
        cites: Arc::new(vec![]),
        refs: Arc::new(std::collections::BTreeSet::new()),
        ignorable: false,
    }
}

fn text_rows(steps: &[bough_plugin_ledger::Step]) -> Vec<Row> {
    bough_plugin_tui_focus::rows_from_steps(steps)
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
    let (lines, _, _) = pane.lines(&state, &live, 80, &Theme::of(ThemeName::Dark));
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
    let out = painted(
        text_rows(&[text_step(1, 0, "Hello "), text_step(2, 0, "world")]),
        "",
    );
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
    let steps: Vec<bough_plugin_ledger::Step> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| text_step(i as u64 + 1, 0, c))
        .collect();
    assert_eq!(
        painted(text_rows(&steps), ""),
        vec!["one two three four five six"]
    );
}

/// A previous step's text is SETTLED and is drawn from the ledger, whole and separately: the
/// concatenation rule applies to the trailing step index only.
#[test]
fn an_earlier_step_index_is_still_drawn_on_its_own() {
    let out = painted(
        text_rows(&[
            text_step(1, 0, "first answer"),
            text_step(2, 1, "second "),
            text_step(3, 1, "answer"),
        ]),
        "",
    );
    assert_eq!(out, vec!["first answer", "second answer"]);
}

/// Mid-stream: the live tail is ahead of the durable concatenation, so the tail is the one thing
/// painted — never the tail AND an earlier flush.
#[test]
fn the_live_tail_supersedes_the_whole_trailing_group() {
    let out = painted(
        text_rows(&[text_step(1, 0, "Hello "), text_step(2, 0, "world")]),
        "Hello world and then some",
    );
    assert_eq!(out, vec!["Hello world and then some"]);
}

/// WP-7 / §2.8 + P5-D14: the field bug at the RENDER end, and the claim card's hit regions.
mod tests {
    use super::*;

    fn joined(text: &str) -> Row {
        Row::Text {
            step: StepId::new("s1"),
            parts: vec![StepId::new("s1"), StepId::new("s2")],
            wake: WakeId::new("w1"),
            index: 0,
            text: text.to_string(),
        }
    }

    /// The field bug: `"I'll run that"` and `" shell command for you."` are two durable steps of
    /// ONE model step, and they must read as one flowing paragraph — wrapped at the pane's width,
    /// never broken at the flush boundary.
    #[test]
    fn a_joined_row_wraps_as_one_paragraph_at_width() {
        let out = painted(vec![joined("I'll run that shell command for you.")], "");
        assert_eq!(
            out,
            vec!["I'll run that shell command for you.".to_string()],
            "one step is one paragraph, not one line per chunk"
        );

        // At a width the paragraph exceeds it wraps on WORDS, and every wrapped line is inside
        // the width — the break is the wrapper's, not the flush timer's.
        let long = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";
        let state = FocusState {
            rows: vec![joined(long)],
            ..Default::default()
        };
        let pane = FocusPane::new(
            cfg(),
            std::sync::Arc::new(Mutex::new(FocusState::default())),
            std::sync::Arc::new(Mutex::new(LiveText::default())),
        );
        let (lines, _, _) = pane.lines(
            &state,
            &LiveText::default(),
            30,
            &bough_plugin_tui_shell::Theme::of(bough_plugin_tui_shell::ThemeName::Dark),
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(text.len() > 1, "a long paragraph wraps: {text:?}");
        assert!(text.iter().all(|l| l.chars().count() <= 30), "{text:?}");
        assert_eq!(
            text.join(" ").split_whitespace().collect::<Vec<_>>(),
            long.split_whitespace().collect::<Vec<_>>(),
            "wrapping reorders and drops nothing"
        );
    }

    /// The joined row is ONE row, so it is painted ONCE: the pre-join renderer had to skip the
    /// earlier flushes by hand, and that hand-skipping is what the join removes.
    #[test]
    fn a_joined_row_draws_exactly_once() {
        let out = painted(vec![joined("Hello world")], "");
        assert_eq!(out, vec!["Hello world".to_string()]);
        assert_eq!(
            out.iter().filter(|l| l.contains("Hello")).count(),
            1,
            "the group is one row and one paragraph: {out:?}"
        );
    }

}
