//! WP-6 §2.7: the index is over what the CONVERSATION SHOWS. Every test here is over a pure
//! function; the fixture is a trajectory of real `Step`s put through the focus pane's own
//! projection, so what is asserted is what the transcript renders.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::{AgentName, Class, Seq, Step, StepId, StepType, TrajId, WakeId};
use bough_plugin_tui_focus::rows::{rows_from_steps, Row};
use bough_plugin_tui_search::index::{
    counter, entries, lines, search, step_selection, strip_markdown,
};
use bough_plugin_tui_search::{hit_id, step_of_hit};
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

fn sol() -> AgentName {
    AgentName::new("sol")
}

/// The trajectory the audit screenshotted: envelope steps interleaved with the prose. The two
/// `thought/text` steps share `(wake, step_index)`, so the projection JOINS them — the term the
/// search test looks for spans the seam.
fn fixture() -> Vec<Step> {
    vec![
        step(1, "step/start", serde_json::json!({ "index": 0 })),
        step(
            2,
            "request/header",
            serde_json::json!({ "as_of": 53, "budget": 96000, "model": "haiku" }),
        ),
        step(
            3,
            "thought/text",
            serde_json::json!({ "text": "I'll run that shell com", "step_index": 0 }),
        ),
        step(
            4,
            "thought/text",
            serde_json::json!({ "text": "mand for you.", "step_index": 0 }),
        ),
        step(
            5,
            "tool/call",
            serde_json::json!({
                "call": "c1",
                "name": "write_file",
                "args": { "path": "notes.md", "text": "the mechanism" },
                "step_index": 1
            }),
        ),
        step(6, "step/end", serde_json::json!({ "index": 1 })),
    ]
}

#[test]
fn envelope_steps_produce_no_entry_and_no_json_reaches_the_index() {
    let rows = rows_from_steps(&fixture());
    let idx = entries(&sol(), &rows);
    let all: String = idx
        .iter()
        .map(|e| format!("{} {}\n", e.speaker, e.text))
        .collect();
    assert!(
        !all.contains("request/header"),
        "the reconstruction anchor is not conversation: {all}"
    );
    assert!(!all.contains("step/start"), "{all}");
    assert!(!all.contains("as_of"), "{all}");
    assert!(
        !all.contains('{') && !all.contains('}'),
        "no JSON braces may reach a hit: {all}"
    );
    // Exactly the two rows a reader sees: the joined answer, and the tool call.
    assert_eq!(idx.len(), 2, "{idx:#?}");
    assert_eq!(idx[0].speaker, "sol");
    assert_eq!(idx[1].speaker, "write_file");
    assert!(idx[1].text.contains("notes.md"), "{:?}", idx[1].text);
}

#[test]
fn a_term_spanning_a_joined_multi_part_row_is_found() {
    let rows = rows_from_steps(&fixture());
    // The join is what makes this possible: the word straddles two `thought/text` steps.
    assert!(matches!(&rows[0], Row::Text { parts, .. } if parts.len() == 2));
    let idx = entries(&sol(), &rows);
    let hits = search(&idx, "shell command", 20);
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].step, StepId::new("s3"), "the row's FIRST step");
    assert_eq!(
        &hits[0].snippet[hits[0].at.clone()],
        "shell command",
        "the range names the match"
    );
}

#[test]
fn the_search_is_case_insensitive_substring_not_stemming() {
    let idx = vec![bough_plugin_tui_search::index::Entry {
        step: StepId::new("s1"),
        agent: sol(),
        speaker: "sol".into(),
        text: "a 5-step MECHANISM for three things".into(),
    }];
    assert_eq!(search(&idx, "mechanism", 10).len(), 1);
    // FTS stemming matched "5-step mechanism" for the query `THREE`; substring does not.
    assert!(search(&idx, "walking", 10).is_empty());
    assert_eq!(search(&idx, "THREE", 10).len(), 1);
}

#[test]
fn a_snippet_is_a_window_around_the_match_with_ellipses() {
    let long = "x".repeat(200) + "needle" + &"y".repeat(200);
    let idx = vec![bough_plugin_tui_search::index::Entry {
        step: StepId::new("s1"),
        agent: sol(),
        speaker: "sol".into(),
        text: long,
    }];
    let hits = search(&idx, "needle", 10);
    assert_eq!(hits.len(), 1);
    let h = &hits[0];
    assert!(
        h.snippet.starts_with('…') && h.snippet.ends_with('…'),
        "{}",
        h.snippet
    );
    assert_eq!(&h.snippet[h.at.clone()], "needle");
    assert!(h.snippet.chars().count() < 40, "{}", h.snippet);
}

#[test]
fn the_counter_reads_n_of_n_and_wraps_under_n_and_shift_n() {
    assert_eq!(counter(0, 3), "1 of 3");
    assert_eq!(counter(2, 3), "3 of 3");
    assert_eq!(counter(0, 0), "no matches");
    // `n` wraps forward past the end, `N` wraps back past the start.
    assert_eq!(step_selection(2, 3, true), 0);
    assert_eq!(step_selection(0, 3, false), 2);
    assert_eq!(step_selection(0, 3, true), 1);
    assert_eq!(step_selection(0, 0, true), 0);
}

#[test]
fn lines_highlight_exactly_the_match_bytes() {
    let idx = vec![bough_plugin_tui_search::index::Entry {
        step: StepId::new("s7"),
        agent: sol(),
        speaker: "sol".into(),
        text: "the swap gate is the phase gate".into(),
    }];
    let hits = search(&idx, "gate", 6);
    assert_eq!(hits.len(), 2, "every occurrence is its own hit");
    let theme = Theme::of(ThemeName::Dark);
    let out = lines(&hits, 0, "gate", 120, &theme);
    // Line 0 is the FIELD: a label and chrome, never a bare dim string.
    let head: String = out[0]
        .0
        .spans
        .iter()
        .map(|s| s.content.to_string())
        .collect();
    assert!(head.starts_with("search "), "{head}");
    assert!(head.contains("[gate"), "the query sits in a box: {head}");
    assert!(head.contains("1 of 2"), "{head}");

    let highlighted: String = out[1]
        .0
        .spans
        .iter()
        .filter(|s| s.style.bg == Some(theme.sel_bg))
        .map(|s| s.content.to_string())
        .collect();
    assert_eq!(
        highlighted, "gate",
        "sel_bg sits on the match and nothing else"
    );
}

#[test]
fn a_hit_id_round_trips_to_its_step() {
    let step = StepId::new("s42");
    let id = hit_id(&step);
    assert_eq!(step_of_hit(&id), Some(step));
    assert_eq!(
        step_of_hit(&bough_plugin_tui_shell::HitId::new("claim:accept")),
        None,
        "another pane's hit is not ours"
    );
}

#[test]
fn markdown_markers_are_stripped_so_the_reader_finds_what_they_see() {
    assert_eq!(strip_markdown("## The **swap** gate"), "The swap gate");
    assert_eq!(strip_markdown("- one\n- two"), "one two");
    assert_eq!(strip_markdown("`code` span"), "code span");
}
