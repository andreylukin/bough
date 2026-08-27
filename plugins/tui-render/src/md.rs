//! Invariant: NO WRAPPED LINE IS EVER STORED (phase ux1 §2.6). The ledger holds the text, the row
//! holds the text, and wrapping plus markdown happen in `render`, against the width of the frame
//! being painted — so a chunk boundary cannot survive a repaint, a resize or a relaunch (M10,
//! M19, nit 39). Every function here is PURE.
//!
//! The parser is TOTAL over unterminated input because it runs on a LIVE TAIL: half a fence, half
//! a table and a heading with no trailing newline are all documents.

use bough_plugin_tui_shell::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::text::{cols, styled_wrap, truncate_cols};

/// One block of an accumulated markdown document.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Para(String),
    Item {
        level: u8,
        marker: String,
        text: String,
    },
    Code {
        lang: Option<String>,
        body: String,
    },
    Table {
        head: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Quote(String),
    Rule,
}

/// PURE and TOTAL: any string is a document. Unterminated fences, half-written tables and a
/// heading with no blank line after it all parse — the parser runs on a LIVE tail.
pub fn blocks(doc: &str) -> Vec<Block> {
    let lines: Vec<&str> = doc
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut out: Vec<Block> = Vec::new();
    // The paragraph under construction, and the quote under construction: both are runs of
    // consecutive source lines, ended by anything that is not more of the same.
    let mut para: Vec<String> = Vec::new();
    let mut quote: Vec<String> = Vec::new();
    let mut i = 0usize;

    macro_rules! flush {
        () => {
            if !para.is_empty() {
                out.push(Block::Para(std::mem::take(&mut para).join("\n")));
            }
            if !quote.is_empty() {
                out.push(Block::Quote(std::mem::take(&mut quote).join("\n")));
            }
        };
    }

    while i < lines.len() {
        let raw = lines[i];
        let trimmed = raw.trim_start();

        // A fence swallows everything to the closing fence, or to the end of the document: an
        // unterminated fence is the normal state of a streaming answer, not an error.
        if let Some(rest) = trimmed.strip_prefix("```") {
            flush!();
            let lang = rest.trim().to_string();
            let indent = raw.len() - trimmed.len();
            let mut body: Vec<String> = Vec::new();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if l.trim_start().starts_with("```") {
                    i += 1;
                    break;
                }
                // Strip the fence's own indent so an indented block does not render doubly.
                body.push(l.get(indent.min(l.len())..).unwrap_or(l).to_string());
                i += 1;
            }
            out.push(Block::Code {
                lang: (!lang.is_empty()).then_some(lang),
                body: body.join("\n"),
            });
            continue;
        }

        if trimmed.is_empty() {
            flush!();
            i += 1;
            continue;
        }

        if is_rule(trimmed) {
            flush!();
            out.push(Block::Rule);
            i += 1;
            continue;
        }

        if let Some((level, text)) = heading(trimmed) {
            flush!();
            out.push(Block::Heading { level, text });
            i += 1;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            if !para.is_empty() {
                out.push(Block::Para(std::mem::take(&mut para).join("\n")));
            }
            quote.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            i += 1;
            continue;
        }

        if let Some((level, marker, text)) = item(raw) {
            flush!();
            out.push(Block::Item {
                level,
                marker,
                text,
            });
            i += 1;
            continue;
        }

        // A table: a pipe row, optionally followed by a delimiter row. The delimiter is what
        // distinguishes a table from prose containing a pipe — EXCEPT at the very end of the
        // document, where the delimiter may simply not have streamed in yet.
        if let Some((table, used)) = table(&lines[i..]) {
            flush!();
            out.push(table);
            i += used;
            continue;
        }

        if !quote.is_empty() {
            out.push(Block::Quote(std::mem::take(&mut quote).join("\n")));
        }
        para.push(raw.trim_end().to_string());
        i += 1;
    }
    flush!();
    out
}

fn is_rule(t: &str) -> bool {
    let t = t.trim_end();
    for c in ['-', '*', '_'] {
        if t.len() >= 3 && t.chars().all(|x| x == c) {
            return true;
        }
    }
    false
}

fn heading(t: &str) -> Option<(u8, String)> {
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &t[hashes..];
    // `#tag` is not a heading; `#` alone at the end of a stream is a heading with no text yet.
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((
        hashes as u8,
        rest.trim().trim_end_matches('#').trim_end().to_string(),
    ))
}

/// `- x`, `* x`, `+ x`, `1. x`, `2) x` — with one indent LEVEL per two leading spaces (or one tab).
fn item(raw: &str) -> Option<(u8, String, String)> {
    let indent: usize = raw
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum();
    let t = raw.trim_start();
    let (marker, rest) = if let Some(r) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    {
        ("•".to_string(), r)
    } else {
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let after = &t[digits..];
        let r = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))?;
        (format!("{}.", &t[..digits]), r)
    };
    Some((
        (indent / 2).min(6) as u8,
        marker,
        rest.trim_end().to_string(),
    ))
}

fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_delimiter(line: &str) -> bool {
    let t = line.trim();
    t.contains('-')
        && t.contains('|')
        && t.chars().all(|c| matches!(c, '-' | '|' | ':' | ' ' | '\t'))
}

/// A table starting at `lines[0]`, and how many source lines it consumed.
fn table(lines: &[&str]) -> Option<(Block, usize)> {
    let head_line = lines[0];
    if !head_line.contains('|') {
        return None;
    }
    let head = cells(head_line);
    if head.len() < 2 {
        return None;
    }
    match lines.get(1) {
        Some(second) if is_delimiter(second) => {
            let mut rows = Vec::new();
            let mut used = 2;
            while let Some(l) = lines.get(used) {
                if !l.contains('|') || l.trim().is_empty() {
                    break;
                }
                rows.push(cells(l));
                used += 1;
            }
            Some((Block::Table { head, rows }, used))
        }
        // The half-written table: the header row has arrived and the delimiter has not. Only at
        // the very tail, so prose with a pipe in the middle of a document stays prose.
        None => Some((
            Block::Table {
                head,
                rows: Vec::new(),
            },
            1,
        )),
        Some(_) => None,
    }
}

/// PURE: blocks to styled lines at `width`. Items hang-indent to the text after the marker
/// (nit 34); tables lay out to their widest cell and scroll-clip, never wrap a cell; code goes
/// through [`crate::highlight`].
///
/// Exactly one blank line separates two adjacent blocks and none is emitted at either end. That
/// count does not depend on `width`, which is what makes a resize a re-wrap and not a re-spacing
/// (nit 39).
pub fn render(blocks: &[Block], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            out.push(Line::from(Span::raw("")));
        }
        out.extend(block(b, width, theme));
    }
    out
}

fn block(b: &Block, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    match b {
        Block::Heading { level, text } => {
            let style = Style::default()
                .fg(if *level <= 2 { theme.accent } else { theme.fg })
                .add_modifier(Modifier::BOLD);
            // Body contrast, never `dim`: a heading is the most readable line on screen (M22).
            crate::text::wrap(text, width)
                .into_iter()
                .map(|l| Line::from(Span::styled(l, style)))
                .collect()
        }
        Block::Para(text) => {
            let mut out = Vec::new();
            for src in text.split('\n') {
                out.extend(styled_wrap(src, width, 0, theme));
            }
            out
        }
        Block::Item {
            level,
            marker,
            text,
        } => {
            let pad = "  ".repeat(*level as usize);
            let head = format!("{pad}{marker} ");
            let hang = cols(&head).min(width.saturating_sub(1) as usize);
            let mut lines = styled_wrap(text, width, hang, theme);
            if let Some(first) = lines.first_mut() {
                // The hanging indent (nit 34): the marker occupies the space `styled_wrap` left
                // blank on the FIRST line, and every continuation stays under the text.
                let mut spans = vec![Span::styled(head, Style::default().fg(theme.dim))];
                spans.extend(first.spans.iter().skip(1).cloned());
                *first = Line::from(spans);
            }
            lines
        }
        Block::Code { lang, body } => crate::highlight(body, lang.as_deref(), theme),
        Block::Table { head, rows } => table_lines(head, rows, width, theme),
        Block::Quote(text) => {
            let bar = Span::styled("│ ".to_string(), Style::default().fg(theme.dim));
            let mut out = Vec::new();
            for src in text.split('\n') {
                for mut l in styled_wrap(src, width, 2, theme) {
                    let mut spans = vec![bar.clone()];
                    spans.extend(l.spans.drain(..).skip(1));
                    out.push(Line::from(spans));
                }
            }
            out
        }
        Block::Rule => vec![Line::from(Span::styled(
            "─".repeat(width as usize),
            Style::default().fg(theme.dim),
        ))],
    }
}

/// A table laid out to its widest cell and CLIPPED, never wrapped: a wrapped cell stops being a
/// column, which is what made the audit read `|----------|` as text.
fn table_lines(
    head: &[String],
    rows: &[Vec<String>],
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let n = head
        .len()
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if n == 0 {
        return Vec::new();
    }
    let mut w: Vec<usize> = vec![0; n];
    for (slot, c) in w.iter_mut().zip(head.iter()) {
        *slot = (*slot).max(cols(c));
    }
    for r in rows {
        for (slot, c) in w.iter_mut().zip(r.iter()) {
            *slot = (*slot).max(cols(c));
        }
    }
    // Shrink proportionally, widest first, until the row fits: the separators cost 3 columns each.
    let sep = 3 * n.saturating_sub(1);
    let mut budget = (width as usize).saturating_sub(sep).max(n);
    while w.iter().sum::<usize>() > budget {
        let widest = (0..n).max_by_key(|i| w[*i]).expect("n > 0");
        if w[widest] <= 1 {
            break;
        }
        w[widest] -= 1;
        budget = budget.max(n);
    }
    let head_style = Style::default().fg(theme.fg).add_modifier(Modifier::BOLD);
    let body_style = Style::default().fg(theme.fg);
    let dim = Style::default().fg(theme.dim);
    let row_line = |cells: &[String], style: Style| {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, col) in w.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ".to_string(), dim));
            }
            let text = cells.get(i).cloned().unwrap_or_default();
            spans.push(Span::styled(
                crate::text::pad_cols(&truncate_cols(&text, *col), *col),
                style,
            ));
        }
        Line::from(spans)
    };
    let mut out = vec![row_line(head, head_style)];
    let rule: String = w
        .iter()
        .map(|c| "─".repeat(*c))
        .collect::<Vec<_>>()
        .join("─┼─");
    out.push(Line::from(Span::styled(rule, dim)));
    for r in rows {
        out.push(row_line(r, body_style));
    }
    out
}

/// The whole path in one call, which is what the pane uses.
pub fn document(doc: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    render(&blocks(doc), width, theme)
}
