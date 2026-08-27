//! Invariant: search indexes what the CONVERSATION SHOWS, never the ledger's JSON (M11). An
//! envelope step produces no entry at all, so `{"as_of":53,"budget":96000,…}` can never be a hit.
//! Every function here is PURE.

use std::ops::Range;

use bough_plugin_ledger::AgentName;
use bough_plugin_ledger::StepId;
use bough_plugin_tui_focus::rows::Row;
use bough_plugin_tui_shell::{HitId, Theme};
use ratatui::text::Line;

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
pub fn entries(rows: &[Row]) -> Vec<Entry> {
    let _ = rows;
    todo!("WP-6")
}

/// One hit, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub step: StepId,
    pub speaker: String,
    /// The snippet, already clipped around the match.
    pub snippet: String,
    /// Byte range of the match INSIDE `snippet`, for the highlight span.
    pub at: Range<usize>,
}

/// PURE: case-insensitive substring search with a `radius`-character window around the match.
/// Substring, deliberately: the audit found THREE matching "5-step mechanism" through FTS
/// stemming and nobody could tell why.
pub fn search(entries: &[Entry], query: &str, radius: usize) -> Vec<Hit> {
    let _ = (entries, query, radius);
    todo!("WP-6")
}

/// PURE: `"3 of 17"`, or `"no matches"`, or `""` for an empty query.
pub fn counter(selected: usize, total: usize) -> String {
    let _ = (selected, total);
    todo!("WP-6")
}

/// PURE: the pane's lines, with the match span carrying [`Theme::sel_bg`].
pub fn lines(
    hits: &[Hit],
    selected: usize,
    query: &str,
    width: u16,
    theme: &Theme,
) -> Vec<(Line<'static>, Option<HitId>)> {
    let _ = (hits, selected, query, width, theme);
    todo!("WP-6")
}
