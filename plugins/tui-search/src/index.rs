//! Invariant: search indexes what the CONVERSATION SHOWS, never the ledger's JSON (M11). An
//! envelope step produces no entry at all, so `{"as_of":53,"budget":96000,…}` can never be a hit.
//! Every function here is PURE.

use std::ops::Range;

use bough_plugin_ledger::AgentName;
use bough_plugin_ledger::StepId;
use bough_plugin_tui_focus::rows::{Row, ENVELOPE};
use bough_plugin_tui_shell::{HitId, Theme};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// One searchable unit: a RENDERED row of the conversation, not a ledger record.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub step: StepId,
    pub agent: AgentName,
    /// "andrey", "sol", "write_file", "turn" — what the row shows as its speaker.
    pub speaker: String,
    /// The row's rendered text, markdown markers stripped, one line.
    pub text: String,
}

/// PURE: the rows of a trajectory as search entries. `request/header` and the other ENVELOPE
/// types produce NO entry.
///
/// Deviation D-WP6-1 from §2.7's sketch: `entries` takes the agent whose trajectory these rows
/// are. `Entry::agent` is in the plan's own struct and a `Row` does not carry it — the rows are a
/// projection of ONE trajectory, so the owner is the caller's fact, not a field to invent.
pub fn entries(agent: &AgentName, rows: &[Row]) -> Vec<Entry> {
    let mut out = Vec::new();
    for row in rows {
        let Some((speaker, text)) = speaker_and_text(agent, row) else {
            continue;
        };
        let text = strip_markdown(&text);
        if text.is_empty() {
            continue;
        }
        out.push(Entry {
            step: row.step().clone(),
            agent: agent.clone(),
            speaker,
            text,
        });
    }
    out
}

/// PURE: what a row says, and who said it. `None` for a row that shows the machinery of a wake
/// rather than anything Andrey reads.
fn speaker_and_text(agent: &AgentName, row: &Row) -> Option<(String, String)> {
    match row {
        Row::Mail { from, subject, .. } => Some((from.clone(), subject.clone())),
        Row::Andrey { text, .. } => Some(("andrey".to_string(), text.clone())),
        Row::Text { text, .. } => Some((agent.as_str().to_string(), text.clone())),
        Row::Reasoning { text, .. } => {
            Some((format!("{} (thinking)", agent.as_str()), text.clone()))
        }
        Row::Claim {
            kind,
            title,
            body,
            state,
            ..
        } => Some((
            format!("claim ({kind})"),
            format!("{title} {body} {}", state.word()),
        )),
        Row::Tool {
            name, args, result, ..
        } => {
            let mut text = args_text(args);
            if let Some(res) = result {
                if !res.content.trim().is_empty() {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(&res.content);
                }
            }
            Some((name.clone(), text))
        }
        // A program row is searchable by the SOURCE the model wrote and by what it printed — the
        // two things a person goes looking for. Its inner calls are folded into the same row, so
        // their arguments and output join the same haystack rather than being lost with them.
        Row::Program {
            source,
            console,
            subs,
            ..
        } => {
            let mut text = format!("{source} {console}");
            for sub in subs {
                text.push(' ');
                text.push_str(&sub.name);
                text.push(' ');
                text.push_str(&args_text(&sub.args));
                if let Some(res) = &sub.result {
                    if !res.content.trim().is_empty() {
                        text.push(' ');
                        text.push_str(&res.content);
                    }
                }
            }
            Some(("program".to_string(), text))
        }
        // A draft card (the TUI brief, D6) is searchable by what it says and who it was for.
        Row::Draft {
            kind,
            audience,
            subject,
            body,
            ..
        } => Some((
            format!("draft ({kind})"),
            format!("{subject} {body} to {audience}"),
        )),
        Row::About { view, .. } => Some((
            "about".to_string(),
            format!("{} {}", view.state, view.intent),
        )),
        // The turn marker is a row Andrey reads, and "turn" is the vocabulary the status chrome
        // uses; its reason is searchable, its phase is not a word anyone types.
        Row::WakeMark { reason, .. } => reason.as_ref().map(|r| ("turn".to_string(), r.clone())),
        // An unknown step type still renders; an ENVELOPE one is machinery and is dropped here as
        // well as in `rows_from_steps`, so no path can reach the index with one.
        Row::Other { kind, .. } => {
            if ENVELOPE.contains(&kind.as_str()) {
                None
            } else {
                Some((kind.as_str().to_string(), String::new()))
            }
        }
    }
}

/// PURE: a tool call's arguments as prose, never as JSON. `{"path":"a.rs"}` reads `path a.rs`:
/// the braces and quotes the audit screenshotted are what made a hit unreadable.
fn args_text(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| format!("{k} {}", scalar(v)))
            .collect::<Vec<_>>()
            .join(" "),
        other => scalar(other),
    }
}

fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Array(items) => items.iter().map(scalar).collect::<Vec<_>>().join(" "),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| format!("{k} {}", scalar(v)))
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

/// PURE: markdown markers off, whitespace collapsed, one line. What the reader SEES is what the
/// index holds, so `**wake**` is found by typing `wake`.
pub fn strip_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_line_start = true;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\r' | '\t' => {
                out.push(' ');
                at_line_start = true;
            }
            '#' | '>' if at_line_start => {}
            ' ' if at_line_start => {}
            '-' | '*' | '+' if at_line_start && chars.peek() == Some(&' ') => {
                chars.next();
            }
            '`' | '*' | '_' | '~' => {}
            other => {
                out.push(other);
                at_line_start = false;
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One hit, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub step: StepId,
    /// The agent whose trajectory the hit is in, so Enter can focus it.
    pub agent: AgentName,
    pub speaker: String,
    /// The snippet, already clipped around the match.
    pub snippet: String,
    /// Byte range of the match INSIDE `snippet`, for the highlight span.
    pub at: Range<usize>,
}

/// PURE: case-insensitive substring search with a `radius`-character window around the match.
/// Substring, deliberately: the audit found THREE matching "5-step mechanism" through FTS
/// stemming and nobody could tell why.
///
/// Every occurrence is its own hit, which is what `n of N` counts and what `n`/`N` step through.
pub fn search(entries: &[Entry], query: &str, radius: usize) -> Vec<Hit> {
    let needle = query.trim();
    if needle.is_empty() {
        return Vec::new();
    }
    let lower_needle = needle.to_lowercase();
    let mut hits = Vec::new();
    for entry in entries {
        let hay = entry.text.to_lowercase();
        // Lowercasing can change byte lengths in general; when it does, byte offsets from the
        // lowered haystack no longer index the original, so that entry falls back to an
        // ASCII-safe scan over the original text.
        let offsets: Vec<usize> = if hay.len() == entry.text.len() {
            find_all(&hay, &lower_needle)
        } else {
            find_all(&entry.text, needle)
        };
        for start in offsets {
            let end = start + lower_needle.len().min(entry.text.len() - start);
            let Some((snippet, at)) = window(&entry.text, start..end, radius) else {
                continue;
            };
            hits.push(Hit {
                step: entry.step.clone(),
                agent: entry.agent.clone(),
                speaker: entry.speaker.clone(),
                snippet,
                at,
            });
        }
    }
    hits
}

fn find_all(hay: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(i) = hay[from..].find(needle) {
        let at = from + i;
        out.push(at);
        from = at + needle.len().max(1);
        if from >= hay.len() {
            break;
        }
    }
    out
}

/// PURE: `radius` characters either side of `at`, clipped to char boundaries, with `…` for what
/// was cut. Returns the snippet and the match's byte range INSIDE it.
fn window(text: &str, at: Range<usize>, radius: usize) -> Option<(String, Range<usize>)> {
    if !text.is_char_boundary(at.start) || !text.is_char_boundary(at.end) {
        return None;
    }
    let start = back(text, at.start, radius);
    let end = forward(text, at.end, radius);
    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    let offset = snippet.len();
    snippet.push_str(&text[start..end]);
    if end < text.len() {
        snippet.push('…');
    }
    let lo = offset + (at.start - start);
    let hi = offset + (at.end - start);
    Some((snippet, lo..hi))
}

fn back(text: &str, from: usize, chars: usize) -> usize {
    let mut i = from;
    for _ in 0..chars {
        if i == 0 {
            break;
        }
        i -= 1;
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
    }
    i
}

fn forward(text: &str, from: usize, chars: usize) -> usize {
    let mut i = from;
    for _ in 0..chars {
        if i >= text.len() {
            break;
        }
        i += 1;
        while i < text.len() && !text.is_char_boundary(i) {
            i += 1;
        }
    }
    i
}

/// PURE: `"3 of 17"`, or `"no matches"`. The pane renders `""` while the query is empty; a
/// counter cannot tell "typed nothing" from "matched nothing" without the query, and the plan's
/// signature does not carry it (deviation D-WP6-2).
pub fn counter(selected: usize, total: usize) -> String {
    if total == 0 {
        return "no matches".to_string();
    }
    format!("{} of {}", selected.min(total - 1) + 1, total)
}

/// PURE: step forward/backward through the hits, WRAPPING. `n` and `N`.
pub fn step_selection(selected: usize, total: usize, forward: bool) -> usize {
    if total == 0 {
        return 0;
    }
    let cur = selected.min(total - 1);
    if forward {
        (cur + 1) % total
    } else {
        (cur + total - 1) % total
    }
}

/// The chrome the field wears, so it is a FIELD and not a dim floating string (M-search).
pub const FIELD_LABEL: &str = "search";

/// PURE: the pane's lines, with the match span carrying [`Theme::sel_bg`].
pub fn lines(
    hits: &[Hit],
    selected: usize,
    query: &str,
    width: u16,
    theme: &Theme,
) -> Vec<(Line<'static>, Option<HitId>)> {
    let mut out: Vec<(Line<'static>, Option<HitId>)> = Vec::with_capacity(hits.len() + 1);
    // The field: a label, a boxed input, and the counter. Never a bare "/".
    let mut field = vec![
        Span::styled(format!("{FIELD_LABEL} "), Style::default().fg(theme.accent)),
        Span::styled("[".to_string(), Style::default().fg(theme.dim)),
        Span::styled(query.to_string(), Style::default().fg(theme.fg)),
        Span::styled("▏]".to_string(), Style::default().fg(theme.dim)),
    ];
    if !query.trim().is_empty() {
        field.push(Span::styled(
            format!("  {}", counter(selected, hits.len())),
            Style::default().fg(theme.hint),
        ));
    }
    out.push((clip(Line::from(field), width), None));

    for (i, hit) in hits.iter().enumerate() {
        let marker = if i == selected { "› " } else { "  " };
        let mut spans = vec![
            Span::styled(marker.to_string(), Style::default().fg(theme.accent)),
            Span::styled(format!("{}  ", hit.speaker), Style::default().fg(theme.dim)),
        ];
        let body = if i == selected { theme.fg } else { theme.dim };
        let at = &hit.at;
        // The three pieces of the snippet: before, the MATCH, after. `sel_bg` sits on exactly the
        // match bytes and nothing else.
        if at.start <= hit.snippet.len() && at.end <= hit.snippet.len() {
            spans.push(Span::styled(
                hit.snippet[..at.start].to_string(),
                Style::default().fg(body),
            ));
            spans.push(Span::styled(
                hit.snippet[at.start..at.end].to_string(),
                Style::default().fg(theme.fg).bg(theme.sel_bg),
            ));
            spans.push(Span::styled(
                hit.snippet[at.end..].to_string(),
                Style::default().fg(body),
            ));
        } else {
            spans.push(Span::styled(hit.snippet.clone(), Style::default().fg(body)));
        }
        out.push((
            clip(Line::from(spans), width),
            Some(crate::hit_id(&hit.step)),
        ));
    }
    out
}

/// PURE: cut a line to `width` columns, keeping each surviving span's style.
fn clip(line: Line<'static>, width: u16) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::from(Vec::<Span<'static>>::new());
    }
    let mut used = 0usize;
    let mut kept: Vec<Span<'static>> = Vec::with_capacity(line.spans.len());
    for span in line.spans {
        let n = span.content.chars().count();
        if used + n <= width {
            used += n;
            kept.push(span);
            continue;
        }
        let room = width - used;
        let text: String = span.content.chars().take(room).collect();
        kept.push(Span::styled(text, span.style));
        break;
    }
    Line::from(kept)
}
