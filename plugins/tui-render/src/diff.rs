//! Invariant: a `RenderIntent::Diff` tool whose arguments match none of the documented shapes
//! falls back to `generic_block` with a dim note — NEVER to nothing (P3-D9). A generic renderer
//! cannot know each tool's argument names, so the intent carries a contract, and
//! `tests/args.rs` checks that contract against `tools-baseline`'s real schemas so the two cannot
//! drift apart silently.

use bough_plugin_tui_shell::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use crate::highlight::Highlighter;
use crate::text::truncate_cols;

/// The two sides of a diff, and the path that decides the syntax.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffSpec {
    pub path: Option<String>,
    pub before: String,
    pub after: String,
}

/// Context lines kept around each hunk. A protocol constant of the rendering, not a deployment
/// knob: it is what a unified diff means.
const CONTEXT: usize = 3;

/// DIFF: `similar::TextDiff::from_lines`, unified hunks with ± gutters, each line syntax
/// highlighted by the path's extension. When no syntax matches the path, the line keeps its
/// `added` / `removed` theme role instead, so a diff is always readable as a diff.
pub fn diff_block(spec: &DiffSpec, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let dim = Style::default().fg(theme.dim);
    let mut out: Vec<Line<'static>> = Vec::new();
    if let Some(p) = &spec.path {
        out.push(Line::from(Span::styled(
            truncate_cols(&format!("── {p}"), width),
            dim,
        )));
    }
    let ext = spec
        .path
        .as_deref()
        .and_then(|p| p.rsplit_once('.').map(|(_, e)| e.to_string()));
    let mut hl = Highlighter::new(ext.as_deref(), theme);

    let diff = TextDiff::from_lines(&spec.before, &spec.after);
    let groups = diff.grouped_ops(CONTEXT);
    if groups.is_empty() {
        out.push(Line::from(Span::styled("(no change)".to_string(), dim)));
        return out;
    }
    for group in groups.iter() {
        let first = group.first().expect("a group is never empty");
        let last = group.last().expect("a group is never empty");
        let (os, oe) = (first.old_range().start, last.old_range().end);
        let (ns, ne) = (first.new_range().start, last.new_range().end);
        out.push(Line::from(Span::styled(
            format!("@@ -{},{} +{},{} @@", os + 1, oe - os, ns + 1, ne - ns),
            dim,
        )));
        for op in group {
            for change in diff.iter_changes(op) {
                let (gutter, role) = match change.tag() {
                    ChangeTag::Delete => ('-', theme.removed),
                    ChangeTag::Insert => ('+', theme.added),
                    ChangeTag::Equal => (' ', theme.dim),
                };
                let text = change.value().trim_end_matches(['\n', '\r']).to_string();
                let body = truncate_cols(&text, width.saturating_sub(1));
                let mut spans = vec![Span::styled(gutter.to_string(), Style::default().fg(role))];
                if hl.active() && change.tag() != ChangeTag::Delete {
                    spans.extend(hl.line(&body, theme));
                } else if hl.active() {
                    // A removed line is highlighted through its OWN pass, so text that is going
                    // away never disturbs the parse state of the file that survives.
                    let mut side = Highlighter::new(ext.as_deref(), theme);
                    spans.extend(side.line(&body, theme));
                } else {
                    spans.push(Span::styled(body, Style::default().fg(role)));
                }
                out.push(Line::from(spans));
            }
        }
    }
    out
}

/// The ARGS CONVENTION a `RenderIntent::Diff` tool must satisfy, in this order (P3-D9):
///   `{path, old, new}` | `{path, old_string, new_string}` | `{path, content}` (whole-file add).
/// `None` ⇒ the renderer falls back to `generic_block` with a dim note, never to nothing.
pub fn diff_spec_from_args(args: &serde_json::Value) -> Option<DiffSpec> {
    let obj = args.as_object()?;
    let path = obj.get("path").and_then(|v| v.as_str()).map(String::from);
    let s = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(String::from);
    if let (Some(before), Some(after)) = (s("old"), s("new")) {
        return Some(DiffSpec {
            path,
            before,
            after,
        });
    }
    if let (Some(before), Some(after)) = (s("old_string"), s("new_string")) {
        return Some(DiffSpec {
            path,
            before,
            after,
        });
    }
    if let Some(after) = s("content") {
        return Some(DiffSpec {
            path,
            before: String::new(),
            after,
        });
    }
    None
}
