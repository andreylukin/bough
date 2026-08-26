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
}

fn inline(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut plain = String::new();
    while !rest.is_empty() {
        if let Some(i) = rest.find("**") {
            if let Some(j) = rest[i + 2..].find("**") {
                plain.push_str(&rest[..i]);
                if !plain.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut plain)));
                }
                out.push(Piece::Bold(rest[i + 2..i + 2 + j].to_string()));
                rest = &rest[i + 4 + j..];
                continue;
            }
        }
        if let Some(i) = rest.find('`') {
            if let Some(j) = rest[i + 1..].find('`') {
                plain.push_str(&rest[..i]);
                if !plain.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut plain)));
                }
                out.push(Piece::Code(rest[i + 1..i + 1 + j].to_string()));
                rest = &rest[i + 2 + j..];
                continue;
            }
        }
        plain.push_str(rest);
        rest = "";
    }
    if !plain.is_empty() {
        out.push(Piece::Text(plain));
    }
    out
}

/// Assistant text: wrap at `width`, style `**bold**` and `` `code` ``, and highlight fenced
/// blocks through [`crate::highlight`]. No termimad in this phase (P3-D10).
pub fn markdownish(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence: Option<(String, Vec<String>)> = None;
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            match fence.take() {
                Some((lang, body)) => {
                    let ext = if lang.is_empty() { None } else { Some(lang) };
                    out.extend(crate::highlight(&body.join("\n"), ext.as_deref(), theme));
                }
                None => fence = Some((rest.trim().to_string(), Vec::new())),
            }
            continue;
        }
        if let Some((_, body)) = fence.as_mut() {
            body.push(raw.to_string());
            continue;
        }
        out.extend(paragraph(raw, width, theme));
    }
    if let Some((lang, body)) = fence {
        let ext = if lang.is_empty() { None } else { Some(lang) };
        out.extend(crate::highlight(&body.join("\n"), ext.as_deref(), theme));
    }
    out
}

/// One source line of prose, wrapped across spans so a style never splits a cluster.
fn paragraph(raw: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let plain = Style::default().fg(theme.fg);
    let bold = plain.add_modifier(Modifier::BOLD);
    let code = Style::default().fg(theme.accent);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for piece in inline(raw) {
        let (body, style) = match piece {
            Piece::Text(t) => (t, plain),
            Piece::Bold(t) => (t, bold),
            Piece::Code(t) => (t, code),
        };
        for (i, chunk) in wrap_from(&body, width, used).into_iter().enumerate() {
            if i > 0 {
                lines.push(Line::from(std::mem::take(&mut cur)));
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
