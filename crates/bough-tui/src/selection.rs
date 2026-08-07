//! Drag selection over the transcript viewport: screen cells in, column spans
//! and plain text out (port of `src/tui/selection.ts`).
//!
//! THE INVARIANT THIS HOLDS: **selection is arithmetic on display columns,
//! never on string indices.** Every transcript row can carry SGR colour, an
//! OSC 8 hyperlink or a wide CJK glyph, so `text[i]` and "the cell under the
//! mouse" are different things — slicing by index would highlight the wrong run
//! and copy escape bytes into the user's clipboard. [`crate::ansi`] does the
//! column arithmetic and the stripping, and everything here is a pure function
//! of `(selection, row, text)` with no terminal, no clock and no renderer.
//!
//! SECOND — **a selection is normalized, never assumed forward.** Dragging up
//! and left is as ordinary as dragging down and right, and every export here
//! reads through `ordered()`.
//!
//! THIRD — **the release cell is inside the selection.** A terminal includes
//! the cell you let go on; excluding it makes a one-character selection
//! impossible to express and a careful drag always one short.
//!
//! The highlighted span deliberately DROPS its own colours: selection reads as
//! one solid inverse band, as it does in a real terminal, rather than as
//! inverse-video syntax highlighting.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::ansi::{slice_ansi, strip_ansi};

/// "to end of line" — the port of the TS `Infinity` sentinel in a span's `to`.
pub const EOL: usize = usize::MAX;

/// A 1-based terminal cell, as a mouse report gives it. `y` is signed because
/// the copy rule looks one row PAST each end of the drag, which at the top of
/// the screen is row 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i64,
    pub y: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub focus: Point,
}

/// A 0-based display-column range `[from, to)`; `to == EOL` means end of line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: usize,
    pub to: usize,
}

/// Anchor/focus in reading order (top-left first).
fn ordered(sel: &Selection) -> (Point, Point) {
    let (a, b) = (sel.anchor, sel.focus);
    if a.y < b.y || (a.y == b.y && a.x <= b.x) {
        (a, b)
    } else {
        (b, a)
    }
}

/// The inclusive screen-row range the selection covers, normalized.
pub fn sel_rows(sel: &Selection) -> (i64, i64) {
    let (a, b) = ordered(sel);
    (a.y, b.y)
}

/// Does the selection cover more than a single cell? A click is not a drag.
pub fn is_empty_selection(sel: &Selection) -> bool {
    sel.anchor.x == sel.focus.x && sel.anchor.y == sel.focus.y
}

/// The selected column span on screen row `y`, or `None` when the row falls
/// outside the selection. Interior rows take the whole line; the end rows clip
/// to the drag's cells, the release cell included.
pub fn row_span(sel: &Selection, y: i64) -> Option<Span> {
    let (a, b) = ordered(sel);
    if y < a.y || y > b.y {
        return None;
    }
    let from = if y == a.y {
        (a.x - 1).max(0) as usize
    } else {
        0
    };
    let to = if y == b.y { b.x.max(0) as usize } else { EOL };
    (from < to).then_some(Span { from, to })
}

fn slice_to(text: &str, from: usize, to: usize) -> String {
    if to == EOL {
        slice_ansi(text, from, usize::MAX)
    } else {
        slice_ansi(text, from, to)
    }
}

/// `text` (which may carry SGR codes) with display columns `[from, to)` in
/// inverse video, the highlighted run stripped of its own colours.
pub fn highlight_span(text: &str, from: usize, to: usize) -> String {
    let before = slice_ansi(text, 0, from);
    let mid = strip_ansi(&slice_to(text, from, to));
    // Nothing under the span (a drag past end-of-line): leave the row alone
    // rather than emit a zero-width inverse pair that some terminals render as
    // a blip.
    if mid.is_empty() {
        return text.to_string();
    }
    let after = if to == EOL {
        String::new()
    } else {
        slice_ansi(text, to, usize::MAX)
    };
    format!("{before}\u{1b}[7m{mid}\u{1b}[27m{after}")
}

/// The plain text of display columns `[from, to)`, trailing whitespace dropped.
pub fn extract_span(text: &str, from: usize, to: usize) -> String {
    strip_ansi(&slice_to(text, from, to)).trim_end().to_string()
}

/// What a row can offer a copy: what is painted, and the raw source behind it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyRow {
    /// The styled row as it appears on screen.
    pub text: String,
    /// The unwrapped source this row was laid out from, when the caller knows
    /// it (`VLine::src`).
    pub src: Option<String>,
}

impl CopyRow {
    pub fn painted(text: impl Into<String>) -> CopyRow {
        CopyRow {
            text: text.into(),
            src: None,
        }
    }
    pub fn with_src(text: impl Into<String>, src: impl Into<String>) -> CopyRow {
        CopyRow {
            text: text.into(),
            src: Some(src.into()),
        }
    }
}

/// Leading block chrome: the `│` gutter and the `╭`/`╰` fence a raised block is
/// drawn in. None of it was in the source, and all of it is worse than useless
/// in a paste — `│ const x = 1` does not run.
static CHROME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)[│╭╰]\s?").unwrap());

/// The panel's RIGHT border, with the padding that reaches it. The left one was
/// stripped from the start and this one was not, so every row copied out of a
/// panel ended in a stray `│` — most visibly on the mcp tab's authorization
/// URL, which is the one thing there anybody copies.
static RIGHT_BORDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*[│╮╯]\s*$").unwrap());

/// A fence row: `╭ ts` opening a block or `╰` closing it. Both are chrome for
/// the whole of their width — the opener's label included. It names the block's
/// language to a READER; pasted into a file it is a stray word on a line of its
/// own, which is worse than not saying it.
static FENCE_ONLY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[╭╰]").unwrap());

fn strip_chrome(line: &str) -> String {
    CHROME.replace(line, "$1").into_owned()
}

fn strip_right_border(line: &str) -> String {
    RIGHT_BORDER.replace(line, "").into_owned()
}

/// A painted row reduced to its content: no border either side, no padding.
///
/// `offset` is how many columns the strip removed from the LEFT, so a caller
/// holding a mouse column can translate it into this string. Without it a click
/// in a panel would hit-test one or two characters off, which on a URL is the
/// difference between opening it and opening nothing.
pub fn row_content(text: &str) -> (String, usize) {
    let plain = strip_right_border(&strip_ansi(text));
    let body = strip_chrome(&plain);
    let offset = plain.chars().count().saturating_sub(body.chars().count());
    (body.trim_end().to_string(), offset)
}

/// Does this span cover everything the row actually SHOWS?
///
/// Both edges are measured against the content, not the cells: the left one
/// against what [`row_content`] strips (a drag from column 0 and one from just
/// after the `│` gutter both hold the whole line), the right one against the
/// painted width with its trailing padding gone (a drag that ran past the end
/// of a short row holds it, and the bottom row of a drag never reports `EOL`).
fn covers_row(text: &str, span: Span) -> bool {
    let (_, offset) = row_content(text);
    if span.from > offset {
        return false;
    }
    if span.to == EOL {
        return true;
    }
    span.to
        >= strip_right_border(&strip_ansi(text))
            .trim_end()
            .chars()
            .count()
}

/// A source line as it should reach the clipboard.
///
/// `src` is the line the row was LAID OUT from, which is not the same as the
/// line the user wrote: a code block is syntax-highlighted before it is
/// wrapped, so the source still carries SGR. Pasting that puts escape bytes in
/// the buffer — the exact failure [`extract_span`] exists to avoid on the other
/// path.
///
/// NOTHING TO CONTRIBUTE vs A BLANK LINE: a source that was all fence yields
/// `None` and is skipped; a source that was genuinely empty yields `Some("")`
/// and pastes as the blank line it is.
fn clean_source(src: &str) -> Option<String> {
    let plain = strip_ansi(src);
    let mut out: Vec<String> = Vec::new();
    let mut dropped_chrome = false;
    for line in plain.split('\n') {
        // A fence is chrome down to its last cell and pastes as a stray glyph
        // or a phantom blank line. A genuinely EMPTY source line is not the
        // same thing and survives, which is why this tests for the fence rather
        // than for emptiness.
        if FENCE_ONLY.is_match(line) {
            dropped_chrome = true;
            continue;
        }
        out.push(strip_chrome(line).trim_end().to_string());
    }
    while out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    if out.is_empty() {
        return if dropped_chrome {
            None
        } else {
            Some(String::new())
        };
    }
    Some(out.join("\n"))
}

/// The selection as text worth pasting.
///
/// DIFFERENT FROM [`selected_text`], deliberately. That one answers "what is on
/// those cells", which is right for a single-row drag and wrong the moment a
/// selection crosses a wrap: the window's line breaks are not the text's.
///
/// So a MULTI-ROW selection is answered from the source instead — but ONLY WHEN
/// THE DRAG COVERS THAT WHOLE SOURCE. Every wrapped row carries the whole
/// logical line as its source, so answering any two-row drag inside a paragraph
/// from the source pasted the ENTIRE paragraph; selecting a phrase and getting
/// the message back is not a copy, it is a different feature. A source is
/// therefore substituted only when every one of its rows was spanned edge to
/// edge and no row of it was left outside the drag (checked by looking one row
/// past each end for the same source).
pub fn selected_copy(sel: &Selection, row_at: impl Fn(i64) -> Option<CopyRow>) -> String {
    let (top, bottom) = sel_rows(sel);
    if top == bottom {
        let (Some(span), Some(row)) = (row_span(sel, top), row_at(top)) else {
            return String::new();
        };
        let cells = extract_span(&row.text, span.from, span.to);
        return strip_chrome(&strip_right_border(&cells));
    }
    // The sources this drag does NOT hold in full. Two ways to fail: a row of
    // the source that the drag clipped, or a row of it that the drag never
    // reached — which is why the rows one past each end are consulted.
    let mut clipped: HashSet<String> = HashSet::new();
    let mut edge = |inside: i64, outside: i64| {
        if let Some(src) = row_at(inside).and_then(|r| r.src) {
            if row_at(outside).and_then(|r| r.src).as_deref() == Some(src.as_str()) {
                clipped.insert(src);
            }
        }
    };
    edge(top, top - 1);
    edge(bottom, bottom + 1);
    for y in top..=bottom {
        if let (Some(span), Some(row)) = (row_span(sel, y), row_at(y)) {
            if let Some(src) = row.src {
                if !covers_row(&row.text, span) {
                    clipped.insert(src);
                }
            }
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut last_source: Option<String> = None;
    for y in top..=bottom {
        let Some(span) = row_span(sel, y) else {
            continue;
        };
        let Some(row) = row_at(y) else {
            // A gap the selection crossed — padding above a short transcript.
            // It pastes as a blank line, because a selection that spans a gap
            // should keep it.
            out.push(String::new());
            last_source = None;
            continue;
        };
        if let Some(src) = row.src.as_ref().filter(|s| !clipped.contains(s.as_str())) {
            // One source, however many rows it was wrapped across.
            if last_source.as_deref() != Some(src.as_str()) {
                if let Some(clean) = clean_source(src) {
                    out.push(clean);
                }
            }
            last_source = Some(src.clone());
            continue;
        }
        last_source = None;
        // No source to consult — the panel, the rail, the composer. A row that
        // is nothing but chrome is still dropped.
        if FENCE_ONLY.is_match(&strip_ansi(&row.text)) {
            continue;
        }
        let cells = extract_span(&row.text, span.from, span.to);
        out.push(strip_chrome(&strip_right_border(&cells)));
    }
    out.join("\n")
}

/// The whole selection as the CELLS hold it, one line per screen row.
///
/// `row_at` maps a screen row to the styled line rendered there, returning
/// `None` for a row that shows nothing. Those rows contribute an empty line
/// rather than being skipped, because a selection that spans a gap should paste
/// with the gap in it. [`selected_copy`] is what the clipboard gets; this is the
/// literal reading, kept for callers that want exactly what is on screen.
pub fn selected_text(sel: &Selection, row_at: impl Fn(i64) -> Option<String>) -> String {
    let (top, bottom) = sel_rows(sel);
    let mut out: Vec<String> = Vec::new();
    for y in top..=bottom {
        let Some(span) = row_span(sel, y) else {
            continue;
        };
        out.push(match row_at(y) {
            Some(line) => extract_span(&line, span.from, span.to),
            None => String::new(),
        });
    }
    // A drag that ends on empty padding should not paste trailing blank lines.
    while out.len() > 1 && out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out.join("\n")
}

/// The OSC 8 target under 0-based column `col`, or `None`.
///
/// A plain click can then open the link even though the TUI's mouse reporting
/// keeps the terminal's own hit-testing away. The escapes are zero-width, so
/// the column math counts only the visible text between markers; a wrapped URL
/// works because the wrapper re-opens the link (with the full target) on each
/// continuation line.
///
/// PORT NOTE: `format.ts` owns this beside `urlAcross`; the plain-text half
/// (rejoining a URL wrapped across content rows) is row 3.21. This half lives
/// here so the click path is complete without reaching into another porter's
/// file.
pub fn link_at(text: &str, col: usize) -> Option<String> {
    static OSC8: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("\u{1b}\\]8;;([^\u{7}\u{1b}]*)(?:\u{7}|\u{1b}\\\\)").unwrap());
    let mut url: Option<String> = None;
    let mut w = 0usize;
    let mut last = 0usize;
    for m in OSC8.captures_iter(text) {
        let whole = m.get(0).unwrap();
        w += crate::ansi::width(&text[last..whole.start()]);
        if col < w {
            return url;
        }
        url = Some(m[1].to_string()).filter(|u| !u.is_empty());
        last = whole.end();
    }
    match url {
        Some(u) if col < w + crate::ansi::width(&text[last..]) => Some(u),
        _ => None,
    }
}

/// Characters that can belong to an address (`format.ts::URL_CHARS`).
fn url_char(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '│'
        )
}

/// The bare URL under 0-based column `col` of a PLAIN row.
///
/// [`link_at`] answers the same question from OSC 8 markers, which only the
/// transcript emits. Everything else bough paints — a panel message, a rail
/// row, a job card — is plain text, so a URL sitting in it was unclickable no
/// matter how obviously it was a URL. This reads the characters instead, which
/// works on any surface.
///
/// Rejoining an address across the rows it was wrapped onto is [`url_across`].
pub fn url_at(plain: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = plain.chars().collect();
    let mut start = 0usize;
    while start < chars.len() {
        let rest: String = chars[start..].iter().collect();
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < chars.len() && url_char(chars[end]) {
            end += 1;
        }
        if col >= start && col < end {
            // Trailing sentence punctuation is not part of the address.
            let url: String = chars[start..end].iter().collect();
            return Some(
                url.trim_end_matches(['.', ',', ';', ':', '!', '?'])
                    .to_string(),
            );
        }
        start = end.max(start + 1);
    }
    None
}

/// The URL under `(row, col)`, rejoined across the rows it was wrapped onto.
///
/// A long URL — an OAuth authorization link is the case that matters — is laid
/// out across four or five rows, and each of them holds a fragment that is not
/// an address. Clicking one and opening the fragment would be worse than doing
/// nothing.
///
/// `rows` are CONTENT rows: already stripped of any border or padding, so "these
/// two rows join" is a fact about the text rather than about the box it is drawn
/// in. Two rows join when the upper ends and the lower begins on characters that
/// could both belong to a URL — which is what a wrap inside an address looks
/// like, and what a wrap between two words does not.
pub fn url_across(rows: &[String], row: usize, col: usize) -> Option<String> {
    // A row that CONTINUES an address is one unbroken token — an address has no
    // spaces, so a wrap inside one produces a row that is nothing but more
    // address. A row with a space in it is prose, or the next list entry.
    let joins = |above: &str, below: &str| -> bool {
        let a = above.trim_end();
        let b = below.trim();
        !a.is_empty()
            && !b.is_empty()
            && !b.chars().any(char::is_whitespace)
            && url_char(a.chars().next_back().unwrap())
            && url_char(b.chars().next().unwrap())
    };
    let at = |i: usize| -> &str { rows.get(i).map(String::as_str).unwrap_or("") };
    // BACKWARD FIRST: the click usually lands in the middle of a long address, on
    // a row that carries no scheme at all.
    let mut start = row;
    while start > 0 && joins(at(start - 1), at(start)) {
        start -= 1;
    }
    // Then forward, remembering where the clicked cell ended up in the joined text.
    let mut joined = String::new();
    let mut click_at: Option<usize> = None;
    for y in start..rows.len() {
        if y > start && !joins(at(y - 1), at(y)) {
            break;
        }
        if y == row {
            click_at = Some(joined.chars().count() + col);
        }
        joined.push_str(at(y).trim_end());
    }
    url_at(&joined, click_at?)
}

// ---------------------------------------------------------------------------
// Tests — ports of `src/tui/selection.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(ax: i64, ay: i64, fx: i64, fy: i64) -> Selection {
        Selection {
            anchor: Point { x: ax, y: ay },
            focus: Point { x: fx, y: fy },
        }
    }
    fn span(from: usize, to: usize) -> Option<Span> {
        Some(Span { from, to })
    }

    #[test]
    fn a_single_row_drag_clips_both_ends_and_includes_the_release_cell() {
        let s = sel(3, 5, 7, 5);
        assert_eq!(row_span(&s, 5), span(2, 7));
        assert_eq!(row_span(&s, 4), None);
        assert_eq!(row_span(&s, 6), None);
    }

    #[test]
    fn a_backwards_drag_normalizes_to_reading_order() {
        assert_eq!(row_span(&sel(7, 5, 3, 5), 5), span(2, 7));

        let up = sel(2, 8, 6, 4);
        assert_eq!(sel_rows(&up), (4, 8));
        assert_eq!(row_span(&up, 4), span(5, EOL)); // first row: x → EOL
        assert_eq!(row_span(&up, 6), span(0, EOL)); // interior: whole line
        assert_eq!(row_span(&up, 8), span(0, 2)); // last row: up to x, inclusive
    }

    #[test]
    fn a_single_cell_selects_exactly_one_column() {
        let s = sel(4, 2, 4, 2);
        assert_eq!(row_span(&s, 2), span(3, 4));
        assert!(is_empty_selection(&s));
        assert!(!is_empty_selection(&sel(4, 2, 5, 2)));
    }

    #[test]
    fn extract_span_strips_sgr_codes_and_clips_by_display_column() {
        let styled = "\u{1b}[1mhello\u{1b}[0m \u{1b}[32mworld\u{1b}[0m";
        assert_eq!(extract_span(styled, 0, EOL), "hello world");
        assert_eq!(extract_span(styled, 6, 11), "world");
        assert_eq!(extract_span(styled, 0, 5), "hello");
    }

    #[test]
    fn extract_span_drops_trailing_whitespace_and_tolerates_spans_past_eol() {
        assert_eq!(extract_span("hi   ", 0, EOL), "hi");
        assert_eq!(extract_span("hi", 0, 80), "hi");
        assert_eq!(extract_span("hi", 5, 10), "");
    }

    #[test]
    fn highlight_span_wraps_the_span_in_inverse_video() {
        assert_eq!(highlight_span("hello", 1, 3), "h\u{1b}[7mel\u{1b}[27mlo");
        assert_eq!(highlight_span("hello", 0, EOL), "\u{1b}[7mhello\u{1b}[27m");
    }

    #[test]
    fn the_selected_span_loses_its_own_colours_and_the_rest_keeps_them() {
        let out = highlight_span("\u{1b}[32mgreen\u{1b}[0m plain", 6, 11);
        // Selection reads as one solid band, not inverse-video highlighting.
        assert!(out.contains("\u{1b}[7mplain\u{1b}[27m"), "{out:?}");
        assert!(out.contains("green"));
    }

    #[test]
    fn an_empty_span_is_a_no_op_rather_than_a_zero_width_inverse_pair() {
        assert_eq!(highlight_span("hi", 5, 10), "hi");
    }

    #[test]
    fn selected_text_joins_the_rows_the_drag_covered_clipped_at_both_ends() {
        let rows = [
            "first line here",
            "\u{1b}[1msecond\u{1b}[0m line",
            "third line",
        ];
        let at = |y: i64| {
            usize::try_from(y - 1)
                .ok()
                .and_then(|i| rows.get(i))
                .map(|s| s.to_string())
        };
        assert_eq!(
            selected_text(&sel(7, 1, 5, 3), at),
            "line here\nsecond line\nthird"
        );
    }

    #[test]
    fn a_row_that_shows_nothing_contributes_a_blank_line_not_a_skipped_one() {
        // Padding above a short transcript: the gap is part of what was dragged.
        let at = |y: i64| {
            if y == 2 {
                None
            } else {
                Some(format!("row {y}"))
            }
        };
        assert_eq!(selected_text(&sel(1, 1, 5, 3), at), "row 1\n\nrow 3");
    }

    #[test]
    fn a_drag_ending_on_empty_padding_does_not_paste_trailing_blank_lines() {
        let at = |y: i64| (y <= 2).then(|| format!("row {y}"));
        assert_eq!(selected_text(&sel(1, 1, 9, 5), at), "row 1\nrow 2");
    }

    // ---- what the clipboard actually gets ----------------------------------

    fn rows_at(rows: Vec<Option<CopyRow>>) -> impl Fn(i64) -> Option<CopyRow> {
        move |y: i64| {
            usize::try_from(y - 1)
                .ok()
                .and_then(|i| rows.get(i))
                .cloned()
                .flatten()
        }
    }

    #[test]
    fn a_copy_across_a_wrap_rejoins_the_line_and_drops_the_gutter() {
        // Two screen rows, one source line — a wrapped code block.
        let src = "const x = await bash(\"a very long command\");";
        let at = rows_at(vec![
            Some(CopyRow::with_src(
                "  │ const x = await bash(\"a very long",
                src,
            )),
            Some(CopyRow::with_src("  │  command\");", src)),
        ]);
        let out = selected_copy(&sel(1, 1, 40, 2), at);
        assert_eq!(out, src);
        assert!(!out.contains('│'), "the gutter is chrome, not content");
        assert!(!out.contains('\n'), "one source line pastes as one line");
    }

    const PARA: &str = "the quick brown fox jumps over the lazy dog and keeps going";

    fn para() -> impl Fn(i64) -> Option<CopyRow> {
        rows_at(vec![
            Some(CopyRow::with_src("the quick brown fox jumps", PARA)),
            Some(CopyRow::with_src("over the lazy dog and", PARA)),
            Some(CopyRow::with_src("keeps going", PARA)),
        ])
    }

    #[test]
    fn a_drag_inside_a_wrapped_line_copies_the_drag_not_the_whole_line() {
        // The bug, reported from a real terminal: selecting a phrase in a
        // message pasted the entire message.
        let out = selected_copy(&sel(5, 1, 9, 2), para());
        assert_eq!(out, "quick brown fox jumps\nover the");
        assert!(!out.contains("keeps going"), "a row the drag never reached");
        assert!(
            !out.contains("the quick"),
            "text before where the drag started"
        );
    }

    #[test]
    fn a_drag_that_starts_mid_paragraph_does_not_reach_back_for_the_rows_above() {
        // Rows 2-3 covered edge to edge — but row 1 is the same source and
        // outside the drag, so the source is not the answer here either.
        let out = selected_copy(&sel(1, 2, 40, 3), para());
        assert_eq!(out, "over the lazy dog and\nkeeps going");
        assert!(!out.contains("quick brown"), "row 1 was never selected");
    }

    #[test]
    fn a_drag_holding_every_row_of_a_source_still_rejoins_it() {
        assert_eq!(selected_copy(&sel(1, 1, 40, 3), para()), PARA);
    }

    #[test]
    fn distinct_source_lines_stay_on_distinct_lines() {
        let at = rows_at(vec![
            Some(CopyRow::with_src("  │ first()", "first()")),
            Some(CopyRow::with_src("  │ second()", "second()")),
        ]);
        assert_eq!(selected_copy(&sel(1, 1, 20, 2), at), "first()\nsecond()");
    }

    #[test]
    fn a_row_with_no_source_falls_back_to_its_cells_minus_the_gutter() {
        // The panel, the rail and the composer have no `src`.
        let at = rows_at(vec![
            Some(CopyRow::painted("  │ painted only")),
            Some(CopyRow::painted("plain row")),
        ]);
        assert_eq!(
            selected_copy(&sel(1, 1, 20, 2), at),
            "  painted only\nplain row"
        );
    }

    #[test]
    fn a_single_row_drag_stays_exact_the_span_not_the_source_line() {
        let at = rows_at(vec![Some(CopyRow::with_src(
            "hello wide world",
            "hello wide world and much more beyond the edge",
        ))]);
        assert_eq!(selected_copy(&sel(7, 1, 10, 1), at), "wide");
    }

    #[test]
    fn a_gap_the_selection_crossed_pastes_as_a_blank_line() {
        let at = rows_at(vec![
            Some(CopyRow::with_src("top", "top")),
            None,
            Some(CopyRow::with_src("bottom", "bottom")),
        ]);
        assert_eq!(selected_copy(&sel(1, 1, 10, 3), at), "top\n\nbottom");
    }

    #[test]
    fn a_highlighted_source_is_stripped_no_escape_bytes_reach_the_clipboard() {
        let styled = "\u{1b}[38;5;140mconst\u{1b}[39m x = 1";
        let at = rows_at(vec![
            Some(CopyRow::with_src(
                format!("  \u{1b}[2m│\u{1b}[22m {styled}"),
                styled,
            )),
            Some(CopyRow::with_src("  next", "next")),
        ]);
        let out = selected_copy(&sel(1, 1, 30, 2), at);
        assert_eq!(out, "const x = 1\nnext");
        assert!(!out.contains('\u{1b}'), "escapes leaked: {out:?}");
    }

    #[test]
    fn a_fence_contributes_nothing_opener_label_and_closer_alike() {
        let at = rows_at(vec![
            Some(CopyRow::with_src("  ╭ code", "╭ code")),
            Some(CopyRow::with_src("  │ body()", "body()")),
            Some(CopyRow::with_src("  ╰", "╰")),
        ]);
        assert_eq!(selected_copy(&sel(1, 1, 20, 3), at), "body()");
    }

    #[test]
    fn a_panel_row_loses_its_right_border_too() {
        // The mcp tab's authorization URL was the visible victim.
        let at = rows_at(vec![Some(CopyRow::painted(
            "│ https://example.com/auth    │",
        ))]);
        assert_eq!(
            selected_copy(&sel(1, 1, 40, 1), at),
            "https://example.com/auth"
        );
    }

    #[test]
    fn row_content_reports_what_the_left_strip_cost_so_a_click_hit_tests_true() {
        let (content, offset) = row_content("  \u{1b}[2m│\u{1b}[22m https://x.dev   │");
        // The row's own indentation survives — only the gutter and its pad are
        // chrome, so a click's column still lines up with the text it names.
        assert_eq!(content, "  https://x.dev");
        assert_eq!(offset, 2, "the gutter and the space after it");
    }

    #[test]
    fn url_at_reads_a_bare_address_out_of_a_plain_row() {
        let row = "open https://x.dev/a?b=1 now";
        assert_eq!(url_at(row, 0), None, "the click was on prose");
        assert_eq!(url_at(row, 5).as_deref(), Some("https://x.dev/a?b=1"));
        assert_eq!(
            url_at(row, 20).as_deref(),
            Some("https://x.dev/a?b=1"),
            "mid-address"
        );
        assert_eq!(url_at(row, 25), None, "past the address");
        // Sentence punctuation is not part of the address.
        assert_eq!(
            url_at("see http://x.dev.", 6).as_deref(),
            Some("http://x.dev")
        );
        assert_eq!(url_at("no address here", 3), None);
    }

    #[test]
    fn link_at_answers_the_osc8_target_under_the_column() {
        let text = "see \u{1b}]8;;https://x.dev\u{7}the docs\u{1b}]8;;\u{7} now";
        assert_eq!(link_at(text, 0), None, "before the link");
        assert_eq!(link_at(text, 4).as_deref(), Some("https://x.dev"));
        assert_eq!(link_at(text, 11).as_deref(), Some("https://x.dev"));
        assert_eq!(link_at(text, 12), None, "past the closing marker");
        assert_eq!(link_at("plain text", 3), None);
    }

    fn rows(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn url_across_rejoins_a_url_wrapped_over_several_rows() {
        // Exactly the shape of an OAuth link in the mcp tab: it runs to the edge of
        // each row and continues on the next with no space.
        let r = rows(&[
            "open this, then come back: https://mcp.example.com/authorize?response_ty",
            "pe=code&client_id=abc&code_challenge=xyz&redirect_uri=http%3A%2F%2F127.0",
            ".0.1%3A4399&scope=read+write",
        ]);
        assert_eq!(
            url_across(&r, 0, 30).as_deref(),
            Some(
                "https://mcp.example.com/authorize?response_type=code\
                 &client_id=abc&code_challenge=xyz&redirect_uri=http%3A%2F%2F127.0\
                 .0.1%3A4399&scope=read+write"
            )
        );
    }

    #[test]
    fn url_across_does_not_glue_the_next_row_onto_a_url_that_already_ended() {
        let r = rows(&[
            "see https://example.com/a and more text",
            "notacontinuation",
        ]);
        assert_eq!(
            url_across(&r, 0, 10).as_deref(),
            Some("https://example.com/a")
        );
    }

    #[test]
    fn url_across_stops_when_a_continuation_row_carries_anything_else() {
        // An address has no spaces, so a row with one is prose or the next list
        // entry. Nothing of it is taken.
        let r = rows(&[
            "https://example.com/averylongpathrunningtotheedge",
            "tail and then words",
        ]);
        assert_eq!(
            url_across(&r, 0, 5).as_deref(),
            Some("https://example.com/averylongpathrunningtotheedge")
        );
    }

    #[test]
    fn url_across_does_not_weld_the_row_below_a_finished_address_onto_it() {
        // The exact shape that broke it live: the last row of a wrapped
        // authorization URL is short, and the mcp list row under it starts with "1".
        let r = rows(&[
            "open this: https://mcp.example.com/authorize?response_type=code&client_i",
            "d=abc&resource=https%3A%2F%2Fmcp.example.com%2Fmcp",
            "1 linear  off · needs auth",
        ]);
        let url = url_across(&r, 1, 10);
        assert_eq!(
            url.as_deref(),
            Some(
                "https://mcp.example.com/authorize?response_type=code&client_i\
                 d=abc&resource=https%3A%2F%2Fmcp.example.com%2Fmcp"
            )
        );
        assert!(!url.unwrap().ends_with('1'), "a row below leaked in");
    }

    #[test]
    fn clicking_a_continuation_row_resolves_the_whole_address() {
        // The click usually lands mid-URL, on a row carrying no scheme at all —
        // which is why the search has to run backward before it runs forward.
        let r = rows(&[
            "open this: https://mcp.example.com/authorize?response_type=code&client_i",
            "d=abc&scope=read",
        ]);
        assert_eq!(
            url_across(&r, 1, 3).as_deref(),
            Some("https://mcp.example.com/authorize?response_type=code&client_id=abc&scope=read")
        );
    }
}
