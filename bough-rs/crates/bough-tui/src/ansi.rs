//! ANSI escapes back into structure, and every measurement over them
//! (port of the ANSI half of `src/tui/format.ts`; replaces `string-width`,
//! `slice-ansi`, `strip-ansi` and `wrap-ansi`).
//!
//! THE INVARIANT: **display width is never a character count.** Every
//! measurement here skips escapes (SGR sequences and OSC 8 hyperlinks occupy
//! zero columns) and counts wide CJK glyphs as two. And the parsed spans
//! concatenate to exactly the stripped text — which is what makes the rendered
//! row the same width as the measured one.
//!
//! There is no drop-in crate for ANSI-aware slice/wrap, so everything is done
//! **over parsed spans**: parse once, operate on `(char, style)` runs,
//! re-serialize. OSC 8 links are zero-width in all of it.
//!
//! This module is also THE bridge to ratatui: [`spans_to_line`] turns a parsed
//! row into a `Line` of styled `Span`s, so raw escapes never reach the
//! renderer's cell diff.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// One run of text with a single style — the unit the renderer is handed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnsiSpan {
    pub text: String,
    /// `#rrggbb`, resolved from truecolor, 256-color or the base-16 names.
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub strikethrough: bool,
    /// OSC 8 target — the run is a hyperlink.
    pub link: Option<String>,
}

impl AnsiSpan {
    fn same_style(&self, other: &AnsiSpan) -> bool {
        self.fg == other.fg
            && self.bg == other.bg
            && self.bold == other.bold
            && self.dim == other.dim
            && self.italic == other.italic
            && self.underline == other.underline
            && self.reverse == other.reverse
            && self.strikethrough == other.strikethrough
            && self.link == other.link
    }

    fn is_plain(&self) -> bool {
        self.fg.is_none()
            && self.bg.is_none()
            && !self.bold
            && !self.dim
            && !self.italic
            && !self.underline
            && !self.reverse
            && !self.strikethrough
    }
}

// ---- the xterm palette -------------------------------------------------------

/// The 6-value ramp the xterm cube is built from.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
/// xterm's first sixteen, which are palette-defined and have no formula.
const BASE16: [&str; 16] = [
    "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
    "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
];

fn hex(r: i64, g: i64, b: i64) -> String {
    let clamp = |c: i64| c.clamp(0, 255) as u8;
    format!("#{:02x}{:02x}{:02x}", clamp(r), clamp(g), clamp(b))
}

/// 256-color index → hex. 0–15 from the table, 16–231 the cube, 232–255 the ramp.
fn xterm256(n: i64) -> String {
    if (0..16).contains(&n) {
        return BASE16[n as usize].to_string();
    }
    if (16..232).contains(&n) {
        let i = n - 16;
        return hex(
            CUBE[((i / 36) % 6) as usize] as i64,
            CUBE[((i / 6) % 6) as usize] as i64,
            CUBE[(i % 6) as usize] as i64,
        );
    }
    if (232..256).contains(&n) {
        let v = 8 + (n - 232) * 10;
        return hex(v, v, v);
    }
    "#ffffff".to_string()
}

// ---- SGR ---------------------------------------------------------------------

/// Read one SGR parameter at `i` into `style`, returning how many EXTRA
/// parameters it consumed. Only the codes bough emits are honoured; an unknown
/// parameter is skipped rather than guessed at.
fn apply_sgr(style: &mut AnsiSpan, ps: &[i64], i: usize) -> usize {
    let p = ps[i];
    match p {
        0 => {
            // A full reset clears colour and attributes; an OSC 8 link is not
            // SGR state and survives one — that is what keeps a bolded URL
            // clickable to its end.
            let link = style.link.take();
            *style = AnsiSpan {
                text: std::mem::take(&mut style.text),
                link,
                ..AnsiSpan::default()
            };
            0
        }
        1 => {
            style.bold = true;
            0
        }
        2 => {
            style.dim = true;
            0
        }
        3 => {
            style.italic = true;
            0
        }
        4 => {
            style.underline = true;
            0
        }
        7 => {
            style.reverse = true;
            0
        }
        9 => {
            style.strikethrough = true;
            0
        }
        22 => {
            style.bold = false;
            style.dim = false;
            0
        }
        23 => {
            style.italic = false;
            0
        }
        24 => {
            style.underline = false;
            0
        }
        27 => {
            style.reverse = false;
            0
        }
        29 => {
            style.strikethrough = false;
            0
        }
        39 => {
            style.fg = None;
            0
        }
        49 => {
            style.bg = None;
            0
        }
        30..=37 => {
            style.fg = Some(BASE16[(p - 30) as usize].to_string());
            0
        }
        90..=97 => {
            style.fg = Some(BASE16[(p - 90 + 8) as usize].to_string());
            0
        }
        40..=47 => {
            style.bg = Some(BASE16[(p - 40) as usize].to_string());
            0
        }
        100..=107 => {
            style.bg = Some(BASE16[(p - 100 + 8) as usize].to_string());
            0
        }
        38 | 48 => {
            let set = |style: &mut AnsiSpan, value: String| {
                if p == 38 {
                    style.fg = Some(value)
                } else {
                    style.bg = Some(value)
                }
            };
            if ps.get(i + 1) == Some(&5) {
                set(style, xterm256(ps.get(i + 2).copied().unwrap_or(0)));
                return 2;
            }
            if ps.get(i + 1) == Some(&2) {
                set(
                    style,
                    hex(
                        ps.get(i + 2).copied().unwrap_or(0),
                        ps.get(i + 3).copied().unwrap_or(0),
                        ps.get(i + 4).copied().unwrap_or(0),
                    ),
                );
                return 4;
            }
            0
        }
        _ => 0,
    }
}

// ---- the parser --------------------------------------------------------------

enum Escape {
    /// `\x1b[…m` — SGR parameter list.
    Sgr(Vec<i64>),
    /// `\x1b]8;;URL(\x07|\x1b\)` — link open (`Some`) or close (`None`).
    Osc8(Option<String>),
    /// Any other `\x1b[…<letter>` CSI, dropped.
    Other,
}

/// Match one escape at the start of `s` (which begins with ESC). Returns the
/// byte length consumed and what it was, or `None` — mirroring the TS regex: an
/// escape it does not match stays as literal text.
fn match_escape(s: &str) -> Option<(usize, Escape)> {
    let bytes = s.as_bytes();
    debug_assert_eq!(bytes[0], 0x1b);
    match bytes.get(1) {
        Some(b'[') => {
            // SGR: `[0-9;]*m`. Generic CSI: `[0-9;?]*[A-Za-z]`.
            let mut i = 2;
            while i < bytes.len() && matches!(bytes[i], b'0'..=b'9' | b';' | b'?') {
                i += 1;
            }
            let final_byte = *bytes.get(i)?;
            if final_byte == b'm' && !bytes[2..i].contains(&b'?') {
                let params = &s[2..i];
                let ps: Vec<i64> = if params.is_empty() {
                    vec![0]
                } else {
                    params
                        .split(';')
                        .map(|p| {
                            if p.is_empty() {
                                0
                            } else {
                                p.parse().unwrap_or(0)
                            }
                        })
                        .collect()
                };
                Some((i + 1, Escape::Sgr(ps)))
            } else if final_byte.is_ascii_alphabetic() {
                Some((i + 1, Escape::Other))
            } else {
                None
            }
        }
        Some(b']') => {
            // OSC 8 only: `]8;;` then the target, terminated by BEL or ST.
            let rest = s.get(2..)?;
            let rest = rest.strip_prefix("8;;")?;
            let mut end = None;
            for (idx, ch) in rest.char_indices() {
                if ch == '\x07' {
                    end = Some((idx, 1));
                    break;
                }
                if ch == '\x1b' {
                    if rest[idx..].starts_with("\x1b\\") {
                        end = Some((idx, 2));
                    }
                    break; // a bare ESC inside the target ends the match either way
                }
            }
            let (url_end, term_len) = end?;
            let url = &rest[..url_end];
            let consumed = 2 + 3 + url_end + term_len;
            Some((
                consumed,
                Escape::Osc8(if url.is_empty() {
                    None
                } else {
                    Some(url.to_string())
                }),
            ))
        }
        _ => None,
    }
}

/// Split a styled string into runs. Every recognized escape is consumed; the
/// returned spans concatenate to exactly the stripped text. Adjacent runs that
/// agree on style are one run.
pub fn ansi_spans(text: &str) -> Vec<AnsiSpan> {
    let mut out: Vec<AnsiSpan> = Vec::new();
    let mut style = AnsiSpan::default();
    let push = |out: &mut Vec<AnsiSpan>, style: &AnsiSpan, chunk: &str| {
        if chunk.is_empty() {
            return;
        }
        if let Some(prev) = out.last_mut() {
            if prev.same_style(style) {
                prev.text.push_str(chunk);
                return;
            }
        }
        out.push(AnsiSpan {
            text: chunk.to_string(),
            ..style.clone()
        });
    };

    let mut rest = text;
    let mut plain = String::new();
    while let Some(esc_at) = rest.find('\x1b') {
        match match_escape(&rest[esc_at..]) {
            Some((len, escape)) => {
                plain.push_str(&rest[..esc_at]);
                push(&mut out, &style, &plain);
                plain.clear();
                match escape {
                    Escape::Sgr(ps) => {
                        let mut i = 0;
                        while i < ps.len() {
                            i += apply_sgr(&mut style, &ps, i) + 1;
                        }
                    }
                    Escape::Osc8(url) => style.link = url,
                    Escape::Other => {}
                }
                rest = &rest[esc_at + len..];
            }
            None => {
                // Not one of ours: the ESC is literal text, exactly as the TS
                // regex leaves it.
                plain.push_str(&rest[..esc_at + 1]);
                rest = &rest[esc_at + 1..];
            }
        }
    }
    plain.push_str(rest);
    push(&mut out, &style, &plain);
    out
}

// ---- serialization -----------------------------------------------------------

/// Spans back into an ANSI string. Round-trips through [`ansi_spans`].
pub fn spans_to_ansi(spans: &[AnsiSpan]) -> String {
    let mut out = String::new();
    for span in spans {
        if let Some(url) = &span.link {
            out.push_str(&format!("\x1b]8;;{url}\x1b\\"));
        }
        let styled = !span.is_plain();
        if styled {
            let mut params: Vec<String> = Vec::new();
            if span.bold {
                params.push("1".into());
            }
            if span.dim {
                params.push("2".into());
            }
            if span.italic {
                params.push("3".into());
            }
            if span.underline {
                params.push("4".into());
            }
            if span.reverse {
                params.push("7".into());
            }
            if span.strikethrough {
                params.push("9".into());
            }
            if let Some(fg) = span.fg.as_deref().and_then(parse_hex) {
                params.push(format!("38;2;{};{};{}", fg.0, fg.1, fg.2));
            }
            if let Some(bg) = span.bg.as_deref().and_then(parse_hex) {
                params.push(format!("48;2;{};{};{}", bg.0, bg.1, bg.2));
            }
            out.push_str(&format!("\x1b[{}m", params.join(";")));
        }
        out.push_str(&span.text);
        if styled {
            out.push_str("\x1b[0m");
        }
        if span.link.is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
    }
    out
}

fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() == 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        return Some((r, g, b));
    }
    if hex.len() == 3 {
        let d = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
        return Some((d(0)?, d(1)?, d(2)?));
    }
    None
}

// ---- measurement -------------------------------------------------------------

/// The stripped text: what the spans concatenate to.
pub fn strip_ansi(text: &str) -> String {
    ansi_spans(text).into_iter().map(|s| s.text).collect()
}

/// Display columns, escapes excluded and wide characters counted as two.
pub fn width(text: &str) -> usize {
    if !text.contains('\x1b') {
        return UnicodeWidthStr::width(text);
    }
    ansi_spans(text)
        .iter()
        .map(|s| UnicodeWidthStr::width(s.text.as_str()))
        .sum()
}

/// Below this a wrap produces one column of letters; clamp instead.
pub const MIN_WRAP: usize = 20;

/// The visible characters of a parsed string, each with the style it carries.
fn styled_chars(text: &str) -> Vec<(char, AnsiSpan)> {
    let mut out = Vec::new();
    for span in ansi_spans(text) {
        let style = AnsiSpan {
            text: String::new(),
            ..span.clone()
        };
        for ch in span.text.chars() {
            out.push((ch, style.clone()));
        }
    }
    out
}

fn chars_to_ansi(chars: &[(char, AnsiSpan)]) -> String {
    let mut spans: Vec<AnsiSpan> = Vec::new();
    for (ch, style) in chars {
        if let Some(prev) = spans.last_mut() {
            if prev.same_style(style) {
                prev.text.push(*ch);
                continue;
            }
        }
        let mut span = style.clone();
        span.text = ch.to_string();
        spans.push(span);
    }
    spans_to_ansi(&spans)
}

/// Slice by VISIBLE character index (escapes zero-width, kept and closed) —
/// the `slice-ansi` replacement, done over parsed spans.
pub fn slice_ansi(text: &str, start: usize, end: usize) -> String {
    let chars = styled_chars(text);
    let end = end.min(chars.len());
    if start >= end {
        return String::new();
    }
    chars_to_ansi(&chars[start..end])
}

/// Truncate to `max` display columns, keeping every escape intact and closed.
/// One character is not one column: a CJK glyph is two and an escape is zero,
/// so a character-count slice would overflow the row on the first wide glyph.
/// The ellipsis is charged against the budget. A string that already fits is
/// returned untouched (bytes and all).
pub fn truncate_ansi(text: &str, max: usize, ellipsis: &str) -> String {
    if max == 0 {
        return String::new();
    }
    if width(text) <= max {
        return text.to_string();
    }
    let tail_w = width(ellipsis);
    if tail_w >= max {
        return String::new();
    }
    let budget = max - tail_w;
    let chars = styled_chars(text);
    let mut taken = 0usize;
    let mut used = 0usize;
    for (ch, _) in &chars {
        let w = UnicodeWidthChar::width(*ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        taken += 1;
    }
    format!("{}{}", chars_to_ansi(&chars[..taken]), ellipsis)
}

/// Hard-wrap text to `max` columns (the `wrap-ansi` replacement: `hard: true`,
/// `trim: false`). A word longer than the width is split instead of
/// overhanging; leading indentation is kept, and the space at a break stays on
/// the upper row — both load-bearing for code blocks.
///
/// EMBEDDED NEWLINES ARE ROW BREAKS, like the `wrapAnsi(...).split("\n")` this
/// replaces. Missing that read a rendered markdown block — `md()` returns its
/// lines joined by `\n` — as ONE logical line, so `- **Hello**\n- **Bough**`
/// painted as `• Hello• Bough`: two list items welded into one row.
pub fn wrap_line(text: &str, max: usize) -> Vec<String> {
    if text.contains('\n') {
        // Each line is styled and reset by its producer, so the split cannot
        // strand an SGR run across the break.
        return text.split('\n').flat_map(|l| wrap_line(l, max)).collect();
    }
    let max = max.max(MIN_WRAP);
    let chars = styled_chars(text);

    // Tokenize into alternating space/word runs of styled chars.
    let mut tokens: Vec<(bool, Vec<(char, AnsiSpan)>)> = Vec::new();
    for pair in chars {
        let is_space = pair.0 == ' ';
        match tokens.last_mut() {
            Some((sp, run)) if *sp == is_space => run.push(pair),
            _ => tokens.push((is_space, vec![pair])),
        }
    }

    let char_w = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);
    let run_w = |run: &[(char, AnsiSpan)]| run.iter().map(|(c, _)| char_w(*c)).sum::<usize>();

    let mut rows: Vec<Vec<(char, AnsiSpan)>> = Vec::new();
    let mut current: Vec<(char, AnsiSpan)> = Vec::new();
    let mut cur_w = 0usize;

    for (is_space, run) in tokens {
        if is_space {
            // trim:false — spaces are content; the run rides the current row.
            cur_w += run_w(&run);
            current.extend(run);
            continue;
        }
        let w = run_w(&run);
        if cur_w + w <= max {
            cur_w += w;
            current.extend(run);
        } else if w <= max {
            rows.push(std::mem::take(&mut current));
            cur_w = w;
            current = run;
        } else {
            // A word longer than the width: fill this row, then whole rows.
            for pair in run {
                let cw = char_w(pair.0);
                if cur_w + cw > max {
                    rows.push(std::mem::take(&mut current));
                    cur_w = 0;
                }
                cur_w += cw;
                current.push(pair);
            }
        }
    }
    rows.push(current);
    rows.iter().map(|row| chars_to_ansi(row)).collect()
}

// ---- the ratatui bridge ------------------------------------------------------

/// Parsed spans → one ratatui line. Raw escapes must never reach the renderer;
/// this is the boundary where strings become structure.
pub fn spans_to_line(spans: &[AnsiSpan]) -> ratatui::text::Line<'static> {
    use ratatui::style::{Color, Modifier, Style};
    let mut out: Vec<ratatui::text::Span<'static>> = Vec::new();
    for span in spans {
        let mut style = Style::default();
        if let Some((r, g, b)) = span.fg.as_deref().and_then(parse_hex) {
            style = style.fg(Color::Rgb(r, g, b));
        }
        if let Some((r, g, b)) = span.bg.as_deref().and_then(parse_hex) {
            style = style.bg(Color::Rgb(r, g, b));
        }
        let mut m = Modifier::empty();
        if span.bold {
            m |= Modifier::BOLD;
        }
        if span.dim {
            m |= Modifier::DIM;
        }
        if span.italic {
            m |= Modifier::ITALIC;
        }
        if span.underline {
            m |= Modifier::UNDERLINED;
        }
        if span.reverse {
            m |= Modifier::REVERSED;
        }
        if span.strikethrough {
            m |= Modifier::CROSSED_OUT;
        }
        style = style.add_modifier(m);
        out.push(ratatui::text::Span::styled(span.text.clone(), style));
    }
    ratatui::text::Line::from(out)
}

/// Parse-and-bridge in one step, for callers holding a styled string.
pub fn line_from_ansi(text: &str) -> ratatui::text::Line<'static> {
    spans_to_line(&ansi_spans(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rendered markdown block arrives as one string with newlines in it; a
    /// wrap that does not break on them welds the list into one row.
    #[test]
    fn embedded_newlines_are_row_breaks() {
        assert_eq!(
            wrap_line("• Hello\n• Bough", 40),
            vec!["• Hello".to_string(), "• Bough".to_string()]
        );
        // A blank line stays a blank row.
        assert_eq!(wrap_line("a\n\nb", 40), vec!["a", "", "b"]);
        // And a line with none is untouched.
        assert_eq!(wrap_line("plain", 40), vec!["plain"]);
    }

    // ---- the gate: span concat == stripped text ------------------------------

    #[test]
    fn spans_concatenate_to_exactly_the_stripped_text() {
        let samples = [
            "plain text",
            "\x1b[1mbold\x1b[22m and \x1b[38;5;245mmuted\x1b[39m",
            "\x1b[38;2;10;20;30mtrue\x1b[0mcolor",
            "\x1b]8;;https://x.dev\x1b\\link\x1b]8;;\x1b\\ tail",
            "cursor moves \x1b[2Adropped",
            "日本語 \x1b[31mred\x1b[39m テキスト",
        ];
        for text in samples {
            let concat: String = ansi_spans(text).into_iter().map(|s| s.text).collect();
            assert_eq!(concat, strip_ansi(text), "{text:?}");
            assert!(!concat.contains('\x1b'), "{text:?}");
        }
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[22m plain"), "bold plain");
        assert_eq!(
            strip_ansi("cursor moves \x1b[2Adropped"),
            "cursor moves dropped"
        );
    }

    #[test]
    fn styles_resolve_to_hex_from_all_three_color_forms() {
        let spans = ansi_spans("\x1b[38;5;245ma\x1b[38;2;1;2;3mb\x1b[31mc\x1b[39md");
        assert_eq!(spans[0].fg.as_deref(), Some("#8a8a8a")); // 245 = ramp 8+13*10
        assert_eq!(spans[1].fg.as_deref(), Some("#010203"));
        assert_eq!(spans[2].fg.as_deref(), Some("#cd0000"));
        assert_eq!(spans[3].fg, None);
        // The cube: 38;5;35 = 16 + r*36+g*6+b → index 19 → (0,95,135)... assert one.
        let cube = ansi_spans("\x1b[38;5;35mx");
        assert_eq!(cube[0].fg.as_deref(), Some("#00af5f"));
    }

    #[test]
    fn a_full_reset_clears_style_but_an_osc8_link_survives_it() {
        // SGR 0 is not link state: a bolded URL stays clickable to its end.
        let spans = ansi_spans("\x1b]8;;https://x.dev\x1b\\\x1b[1ma\x1b[0mb\x1b]8;;\x1b\\c");
        assert_eq!(spans.len(), 3);
        assert!(spans[0].bold);
        assert_eq!(spans[0].link.as_deref(), Some("https://x.dev"));
        assert!(!spans[1].bold);
        assert_eq!(spans[1].link.as_deref(), Some("https://x.dev"));
        assert_eq!(spans[2].link, None);
    }

    #[test]
    fn adjacent_same_style_runs_merge() {
        // A style that never changed must not look like it did.
        let spans = ansi_spans("ab\x1b[1m\x1b[22mcd");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "abcd");
    }

    #[test]
    fn serialization_round_trips_through_the_parser() {
        let original =
            "\x1b[1;38;2;10;20;30mbold\x1b[0m \x1b]8;;https://x.dev\x1b\\link\x1b]8;;\x1b\\";
        let spans = ansi_spans(original);
        let reparsed = ansi_spans(&spans_to_ansi(&spans));
        assert_eq!(spans, reparsed);
    }

    // ---- the gate: OSC 8 is zero-width ---------------------------------------

    #[test]
    fn osc8_links_are_zero_width_in_width_truncate_and_wrap() {
        let linked = "\x1b]8;;https://example.com/very/long/target\x1b\\text\x1b]8;;\x1b\\";
        assert_eq!(width(linked), 4);
        // Fits: untouched. Cut: the link marker survives and the text is cut.
        assert_eq!(truncate_ansi(linked, 10, ""), linked);
        let cut = truncate_ansi(linked, 2, "");
        assert_eq!(width(&cut), 2);
        assert_eq!(
            ansi_spans(&cut)[0].link.as_deref(),
            Some("https://example.com/very/long/target")
        );
        // Wrapping a row with a link does not break early for the escape bytes.
        let padded = format!("{} {}", linked, "word".repeat(5));
        for row in wrap_line(&padded, 20) {
            assert!(width(&row) <= 20, "{row:?}");
        }
    }

    // ---- width ---------------------------------------------------------------

    #[test]
    fn width_counts_columns_not_chars() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本語テキスト"), 14);
        assert_eq!(width("\x1b[1mbold\x1b[22m"), 4);
    }

    // ---- truncation ----------------------------------------------------------

    #[test]
    fn truncate_cuts_to_display_columns_and_keeps_escapes_intact() {
        let styled = "\x1b[1mabcdefghij\x1b[22m klmno";
        let cut = truncate_ansi(styled, 6, "");
        assert_eq!(width(&cut), 6);
        assert!(cut.contains("\x1b["), "escapes must survive the slice");
        assert!(cut.ends_with('m'), "{cut:?}"); // closed, nothing bleeds
    }

    #[test]
    fn truncate_zero_width_escapes_are_not_counted_as_content() {
        let plain = "abcdef";
        let styled = "\x1b[38;5;80mabcdef\x1b[39m";
        assert_eq!(width(styled), 6);
        assert_eq!(truncate_ansi(plain, 10, ""), plain); // short enough: untouched
        assert_eq!(truncate_ansi(styled, 10, ""), styled);
        assert_eq!(width(&truncate_ansi(styled, 3, "")), 3);
    }

    #[test]
    fn truncate_a_wide_glyph_never_half_fills_the_last_column() {
        let text = "日本語テキスト"; // 2 columns each
        assert_eq!(width(text), 14);
        assert_eq!(width(&truncate_ansi(text, 5, "")), 4); // 2 glyphs fit, the third would not
        assert_eq!(width(&truncate_ansi(text, 6, "")), 6);
    }

    #[test]
    fn truncate_the_ellipsis_is_charged_against_the_budget() {
        assert_eq!(truncate_ansi("abcdefghij", 5, "…"), "abcd…");
        assert_eq!(width(&truncate_ansi("abcdefghij", 5, "…")), 5);
        assert_eq!(truncate_ansi("abc", 5, "…"), "abc"); // fits: no ellipsis
        assert_eq!(truncate_ansi("abcdef", 1, "…"), ""); // no room for content at all
        assert_eq!(truncate_ansi("abcdef", 0, ""), "");
    }

    // ---- wrapping ------------------------------------------------------------

    #[test]
    fn wrap_wraps_at_the_width_and_keeps_leading_indentation() {
        // trim:false — the two leading columns are indentation the caller meant,
        // and the break keeps its space rather than reflowing the row.
        assert_eq!(
            wrap_line("  alpha beta gamma delta", 20),
            vec!["  alpha beta gamma ", "delta"]
        );
        assert_eq!(wrap_line("alpha beta", 40), vec!["alpha beta"]); // fits: one row
    }

    #[test]
    fn wrap_a_word_longer_than_the_width_is_split_never_overhung() {
        let out = wrap_line(&"x".repeat(50), 20);
        assert_eq!(out.len(), 3);
        for l in &out {
            assert!(width(l) <= 20, "row too wide: {}", width(l));
        }
    }

    #[test]
    fn wrap_a_styled_line_measures_by_display_width_not_characters() {
        let styled = "\x1b[1malphabet\x1b[22m soup with rather a lot of words in it";
        assert!(styled.len() > 49); // escapes inflate the character count…
        for l in wrap_line(styled, 20) {
            assert!(width(&l) <= 20, "row measured {}: {l:?}", width(&l));
        }
    }

    #[test]
    fn wrap_a_sub_minimum_width_clamps_instead_of_producing_one_column() {
        let out = wrap_line("alpha beta gamma", 1);
        for l in &out {
            assert!(width(l) <= 20);
        }
        assert!(out.len() <= 2);
    }

    #[test]
    fn wrap_continuation_rows_reopen_the_style() {
        let styled = format!("\x1b[1m{}\x1b[22m", "y".repeat(30));
        let rows = wrap_line(&styled, 20);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let spans = ansi_spans(row);
            assert!(spans.iter().all(|s| s.bold), "{row:?}");
        }
    }

    // ---- slicing -------------------------------------------------------------

    #[test]
    fn slice_is_by_visible_char_index_and_keeps_the_style() {
        let styled = "ab\x1b[1mcd\x1b[22mef";
        assert_eq!(strip_ansi(&slice_ansi(styled, 1, 5)), "bcde");
        let spans = ansi_spans(&slice_ansi(styled, 2, 4));
        assert_eq!(spans.len(), 1);
        assert!(spans[0].bold);
        assert_eq!(slice_ansi("abc", 3, 3), "");
    }

    // ---- the ratatui bridge --------------------------------------------------

    #[test]
    fn the_bridge_maps_style_to_ratatui_without_raw_escapes() {
        use ratatui::style::{Color, Modifier};
        let line = line_from_ansi("\x1b[1;38;2;255;0;0mred\x1b[0m plain");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "red");
        assert_eq!(line.spans[0].style.fg, Some(Color::Rgb(255, 0, 0)));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[1].content, " plain");
        assert_eq!(line.spans[1].style.fg, None);
        for span in &line.spans {
            assert!(!span.content.contains('\x1b'));
        }
    }
}
