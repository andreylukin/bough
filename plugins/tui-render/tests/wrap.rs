//! Wrapping is grapheme-aware and column-correct: a CJK cell is two columns, a combining mark is
//! zero, and neither may be split or overflow.

use crate::common;

use bough_plugin_tui_render::wrap;
use unicode_width::UnicodeWidthStr;

#[test]
fn ascii_wraps_on_word_boundaries() {
    assert_eq!(
        wrap("the quick brown fox", 10),
        vec!["the quick", "brown fox"]
    );
}

#[test]
fn a_word_longer_than_the_line_is_hard_broken_not_dropped() {
    let got = wrap("aaaaaaaaaaaa", 5);
    assert_eq!(got, vec!["aaaaa", "aaaaa", "aa"]);
    assert_eq!(got.concat(), "aaaaaaaaaaaa");
}

#[test]
fn a_cjk_cell_counts_as_two_columns() {
    let got = wrap("日本語テキスト", 6);
    for l in &got {
        assert!(UnicodeWidthStr::width(l.as_str()) <= 6, "{l:?}");
    }
    assert_eq!(got, vec!["日本語", "テキス", "ト"]);
}

#[test]
fn a_grapheme_cluster_is_never_split() {
    // Family emoji: several code points, one cluster.
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    let got = wrap(&format!("{family}{family}"), 3);
    assert!(
        got.iter().all(|l| l.contains(family) || l.is_empty()),
        "{got:?}"
    );
    assert_eq!(got.concat().matches(family).count(), 2);
}

#[test]
fn a_combining_mark_costs_no_column() {
    let got = wrap("e\u{301}e\u{301}e\u{301}", 3);
    assert_eq!(got.len(), 1, "{got:?}");
}

#[test]
fn hard_newlines_are_honoured_and_empty_lines_survive() {
    assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
    assert_eq!(wrap("a\r\nb", 10), vec!["a", "b"]);
}

#[test]
fn a_zero_width_never_loops_forever() {
    assert_eq!(wrap("ab", 0), vec!["a", "b"]);
}

#[test]
fn markdownish_styles_bold_and_code_without_leaking_the_markers() {
    let th = common::theme();
    let lines = bough_plugin_tui_render::markdownish("a **b** and `c`", 40, &th);
    let rendered: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(rendered, "a b and c");
    let styles: Vec<_> = lines[0].spans.iter().map(|s| s.style.fg).collect();
    // Code is its own role on its own ground (visual audit F5), no longer the accent.
    assert!(
        styles.contains(&Some(th.code)),
        "the code span has the code role"
    );
    assert!(
        !styles.contains(&Some(th.accent)),
        "a code span is not a speaker or a heading"
    );
}

#[test]
fn markdownish_highlights_a_fenced_block() {
    let th = common::theme();
    let lines = bough_plugin_tui_render::markdownish("```rs\nfn main() {}\n```", 40, &th);
    let rendered: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();
    assert!(rendered.contains("fn main() {}"), "{rendered:?}");
    assert!(
        lines.iter().any(|l| l.spans.len() > 1),
        "a fenced rust block is highlighted into runs"
    );
}
