//! Invariant: wrapping is GRAPHEME-AWARE and width-correct. A CJK cell is two columns and a
//! combining mark is zero, so a wrapped line never overflows its pane and never splits a cluster.

use bough_plugin_tui_shell::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display columns a string occupies.
pub(crate) fn cols(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` to at most `width` columns, appending `…` when anything was dropped. Never splits
/// a grapheme cluster.
pub(crate) fn truncate_cols(s: &str, width: usize) -> String {
    if cols(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for g in s.graphemes(true) {
        let w = cols(g);
        if used + w > budget {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

/// Pad `s` on the right to exactly `width` columns (it is assumed to fit).
pub(crate) fn pad_cols(s: &str, width: usize) -> String {
    let w = cols(s);
    let mut out = s.to_string();
    for _ in w..width {
        out.push(' ');
    }
    out
}

/// Grapheme-aware wrapping used by all of the above. Hard newlines in `text` are honoured; a run
/// longer than `width` is broken at a grapheme boundary rather than overflowing.
pub fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut used = 0usize;
        for word in split_keeping_spaces(raw) {
            let w = cols(&word);
            if used > 0 && used + w > width {
                // A trailing run of spaces at a break point is dropped, as a terminal would.
                let broken = std::mem::take(&mut line);
                out.push(broken.trim_end_matches(' ').to_string());
                used = 0;
                if word.trim().is_empty() {
                    continue;
                }
            }
            if w <= width {
                line.push_str(&word);
                used += w;
            } else {
                // Longer than a whole line: hard-break on grapheme boundaries.
                for g in word.graphemes(true) {
                    let gw = cols(g);
                    if used + gw > width {
                        out.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    line.push_str(g);
                    used += gw;
                }
            }
        }
        out.push(line);
    }
    out
}

/// Split into alternating word / whitespace runs, keeping both.
fn split_keeping_spaces(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_space: Option<bool> = None;
    for g in s.graphemes(true) {
        let is_space = g.chars().all(|c| c.is_whitespace());
        match cur_space {
            Some(prev) if prev == is_space => cur.push_str(g),
            Some(_) => {
                out.push(std::mem::take(&mut cur));
                cur.push_str(g);
                cur_space = Some(is_space);
            }
            None => {
                cur.push_str(g);
                cur_space = Some(is_space);
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// ANSI stripped, for the TERMINAL intent. CSI, OSC (BEL- or ST-terminated) and two-byte escapes
/// all go; everything else is kept byte for byte.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.peek().copied() {
            Some('[') => {
                it.next();
                for c in it.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                it.next();
                while let Some(c) = it.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && it.peek() == Some(&'\\') {
                        it.next();
                        break;
                    }
                }
            }
            Some(_) => {
                it.next();
            }
            None => {}
        }
    }
    out
}

/// One piece of inline markdown.
#[derive(Clone, Debug, PartialEq)]
enum Piece {
    Text(String),
    Bold(String),
    Code(String),
    /// A `[label](url)` link's label (or its url, when the label is empty).
    Link(String),
    /// The ` (url)` a labelled link trails: kept on screen because a terminal link is not
    /// clickable — hiding the url would strand the reader with a name and no address.
    Url(String),
}

/// PURE: split one source line into styled pieces.
///
/// An UNTERMINATED marker styles to the end of the line rather than printing itself. That is not
/// leniency, it is the streaming rule: on a live tail the closing `` ` `` or `**` has simply not
/// arrived yet, and an orphan backtick on screen is the thing the audit saw (M19).
fn inline(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut plain = String::new();
    loop {
        let bold = rest.find("**");
        let code = rest.find('`');
        let link = rest.find('[');
        // Whichever opener comes first wins; a backtick inside `**bold**` is still code.
        let earliest = [bold.map(|b| (b, 0u8)), code.map(|c| (c, 1)), link.map(|l| (l, 2))]
            .into_iter()
            .flatten()
            .min();
        let Some((at, kind)) = earliest else { break };

        // `[label](url)` — rendered as the label with the address alongside, never the raw
        // markers (M19). `[text]` with no `(` is literal (a footnote, a `[[wiki]]` ref), and an
        // unterminated label or url is the streaming rule again: style what has arrived.
        if kind == 2 {
            plain.push_str(&rest[..at]);
            // An image `![alt](url)` renders as its link; the bang is a marker too.
            if plain.ends_with('!') {
                plain.pop();
            }
            let body_from = at + 1;
            match rest[body_from..].find(']') {
                None => {
                    if !plain.is_empty() {
                        out.push(Piece::Text(std::mem::take(&mut plain)));
                    }
                    let label = rest[body_from..].to_string();
                    if !label.is_empty() {
                        out.push(Piece::Link(label));
                    }
                    rest = "";
                    break;
                }
                Some(j) => {
                    let label = &rest[body_from..body_from + j];
                    let after = &rest[body_from + j + 1..];
                    if let Some(url_rest) = after.strip_prefix('(') {
                        let (url, remaining) = match url_rest.find(')') {
                            Some(k) => (&url_rest[..k], &url_rest[k + 1..]),
                            None => (url_rest, ""),
                        };
                        if !plain.is_empty() {
                            out.push(Piece::Text(std::mem::take(&mut plain)));
                        }
                        if label.is_empty() {
                            out.push(Piece::Link(url.to_string()));
                        } else {
                            out.push(Piece::Link(label.to_string()));
                            if !url.is_empty() && url != label {
                                out.push(Piece::Url(format!(" ({url})")));
                            }
                        }
                        rest = remaining;
                    } else {
                        plain.push('[');
                        plain.push_str(label);
                        plain.push(']');
                        rest = after;
                    }
                    continue;
                }
            }
        }

        let is_bold = kind == 0;
        let (open, close) = if is_bold {
            (2usize, "**")
        } else {
            (1usize, "`")
        };
        plain.push_str(&rest[..at]);
        let body_from = at + open;
        match rest[body_from..].find(close) {
            Some(j) => {
                if !plain.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut plain)));
                }
                let body = rest[body_from..body_from + j].to_string();
                out.push(if is_bold {
                    Piece::Bold(body)
                } else {
                    Piece::Code(body)
                });
                rest = &rest[body_from + j + open..];
            }
            None => {
                if !plain.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut plain)));
                }
                let body = rest[body_from..].to_string();
                if !body.is_empty() {
                    out.push(if is_bold {
                        Piece::Bold(body)
                    } else {
                        Piece::Code(body)
                    });
                }
                rest = "";
                break;
            }
        }
    }
    if !rest.is_empty() {
        plain.push_str(rest);
    }
    if !plain.is_empty() {
        out.push(Piece::Text(plain));
    }
    out
}

/// Assistant text: the whole markdown path, at the width of the frame being painted.
///
/// Kept as a thin shim over [`crate::md::document`] so the tool-result renderers and `tui-strip`
/// do not all change at once (phase ux1 §2.6).
pub fn markdownish(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    crate::md::document(text, width, theme)
}

/// One source line of prose as styled, wrapped lines, hanging-indented by `hang` columns.
///
/// EVERY returned line starts with a `hang`-wide raw pad span, so a caller that owns the gutter
/// (a list marker, a quote bar) replaces `spans[0]` and nothing else has to know about it.
pub(crate) fn styled_wrap(raw: &str, width: u16, hang: usize, theme: &Theme) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let hang = hang.min(width.saturating_sub(1));
    let body_w = width.saturating_sub(hang).max(1) as u16;
    let plain = Style::default().fg(theme.fg);
    let bold = plain.add_modifier(Modifier::BOLD);
    // Code is a TEXTURE, not a colour (visual audit F5): its own foreground on its own ground,
    // so `inline_code` no longer reads as a link or a speaker.
    let code = Style::default().fg(theme.code).bg(theme.code_bg);
    // A link is a thing you would OPEN: `interactive`, the palette's one "this responds" colour
    // (visual audit F5), underlined; the address trails in `dim`.
    let link = Style::default()
        .fg(theme.interactive)
        .add_modifier(Modifier::UNDERLINED);
    let url = Style::default().fg(theme.dim);
    let pad = || Span::raw(" ".repeat(hang));

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = vec![pad()];
    let mut used = 0usize;
    for piece in inline(raw) {
        let (body, style) = match piece {
            Piece::Text(t) => (t, plain),
            Piece::Bold(t) => (t, bold),
            Piece::Code(t) => (t, code),
            Piece::Link(t) => (t, link),
            Piece::Url(t) => (t, url),
        };
        for (i, chunk) in wrap_from(&body, body_w, used).into_iter().enumerate() {
            if i > 0 {
                lines.push(Line::from(std::mem::take(&mut cur)));
                cur.push(pad());
                used = 0;
            }
            if !chunk.is_empty() {
                used += cols(&chunk);
                cur.push(Span::styled(chunk, style));
            }
        }
    }
    lines.push(Line::from(cur));
    lines
}

/// Wrap `body` when the first line already has `used` columns spent.
fn wrap_from(body: &str, width: u16, used: usize) -> Vec<String> {
    if used == 0 {
        return wrap(body, width);
    }
    let pad = " ".repeat(used);
    let mut got = wrap(&format!("{pad}{body}"), width);
    if let Some(first) = got.first_mut() {
        *first = first.chars().skip(used).collect();
    }
    got
}
