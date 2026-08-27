//! phase ux1 §2.10 (minor 29): the about-line is ONE clean sentence, and the audit's own line is
//! the fixture. `read mail `say hi`; Hi; ! 👋 ; **` was three fragments joined with `;` and a
//! dangling bold marker, repeated on the screen once per agent.

use bough_plugin_residents::one_sentence;

/// The exact string the audit screenshotted.
const AUDIT: &str = "read mail `say hi`; Hi; ! 👋 ; **";

#[test]
fn one_sentence_strips_markers_and_never_splices() {
    let s = one_sentence(AUDIT, 80);
    assert_eq!(s, "read mail say hi");
    assert!(!s.contains('*'), "no dangling emphasis: {s}");
    assert!(!s.contains('`'), "no code markers: {s}");
    assert!(!s.contains(';'), "one sentence, never a splice: {s}");
}

#[test]
fn an_emoji_survives_the_strip() {
    assert_eq!(one_sentence("shipped it 👋", 80), "shipped it 👋");
}

#[test]
fn a_sentence_keeps_its_terminator_and_drops_what_follows() {
    assert_eq!(
        one_sentence("Read the plan. Then I broke the build.", 80),
        "Read the plan."
    );
}

#[test]
fn whitespace_and_newlines_collapse_to_one_row() {
    assert_eq!(
        one_sentence("ran   `make gates`\n  and it\twas green", 80),
        "ran make gates and it was green"
    );
}

#[test]
fn the_clip_lands_on_a_word_boundary() {
    let s = one_sentence("alpha beta gamma delta epsilon", 20);
    assert!(s.chars().count() <= 20, "{s}");
    assert!(s.ends_with('…'), "{s}");
    assert!(!s.contains("epsil"), "never mid-word: {s}");
    assert_eq!(s, "alpha beta gamma…");
}

#[test]
fn a_line_that_reduces_to_nothing_is_empty_not_invented() {
    assert_eq!(one_sentence("** ; ;", 80), "");
    assert_eq!(one_sentence("", 80), "");
}
