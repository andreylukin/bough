//! Invariant: every function here is PURE. No ctx, no service key, no row, no I/O, no clock — a
//! `(input, width, theme)` triple always renders to the same `Vec<Line>`. That is what lets the
//! panes be tested by their row projection and the rendering be tested by its bytes, separately.
//!
//! §9's declared `RenderIntent` is the dispatch: a surface renders a tool by what the tool said it
//! is, never by sniffing its name.

pub mod about;
pub mod diff;
pub mod highlight;
pub mod invariant;
pub mod text;

use bough_plugin_tools::{RenderIntent, ToolResultBody};
use bough_plugin_tui_shell::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub use about::{about_from_step, AboutView};
pub use diff::{diff_block, diff_spec_from_args, DiffSpec};
pub use highlight::highlight;
pub use text::{markdownish, strip_ansi, wrap};

use text::{cols, pad_cols, truncate_cols};

/// One tool call, as a surface wants to draw it.
pub struct ToolCallView<'a> {
    pub name: &'a str,
    pub intent: RenderIntent,
    pub args: &'a serde_json::Value,
    pub result: Option<&'a ToolResultBody>,
    pub expanded: bool,
    pub width: u16,
    pub theme: &'a Theme,
}

/// The glyph a result gets in the header. A call with no result yet is still running.
fn outcome_glyph(result: Option<&ToolResultBody>) -> (&'static str, bool) {
    use bough_plugin_tools::ToolOutcomeKind::*;
    match result.map(|r| r.outcome) {
        None => ("⋯", false),
        Some(Ok) => ("✓", false),
        Some(Error) => ("✗", true),
        Some(Denied) => ("⊘", true),
        Some(Blocked) => ("⊘", true),
        Some(Unknown) => ("?", true),
    }
}

/// A one-line gist of the arguments for the header: the argument a human reads first, falling back
/// to the alphabetically first scalar so no tool renders a nameless header.
fn arg_gist(args: &serde_json::Value) -> String {
    let Some(obj) = args.as_object() else {
        return scalar(args);
    };
    for key in ["command", "path", "pattern", "url", "query"] {
        if let Some(v) = obj.get(key) {
            return scalar(v);
        }
    }
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    keys.first().map(|k| scalar(&obj[*k])).unwrap_or_default()
}

/// A json value as one line of text: strings raw, everything else compact json.
fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.replace('\n', " "),
        other => other.to_string(),
    }
}

/// The collapsed header: `▸ bash  ls -la …            ✓`. Always exactly one line, and always
/// exactly `width` columns wide when `width > 0`, so a pane can lay it out without measuring.
pub fn tool_header(v: &ToolCallView<'_>) -> Line<'static> {
    let width = v.width as usize;
    let (glyph, bad) = outcome_glyph(v.result);
    let marker = if v.expanded { "▾" } else { "▸" };
    let name_style = Style::default()
        .fg(v.theme.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(v.theme.dim);
    let glyph_style = Style::default().fg(if bad { v.theme.error } else { v.theme.fg });

    if width == 0 {
        return Line::from(Vec::<Span<'static>>::new());
    }
    let right = format!(" {glyph}");
    let right_w = cols(&right);
    let left_budget = width.saturating_sub(right_w);
    let head = format!("{marker} {} ", v.name);
    let head = truncate_cols(&head, left_budget);
    let rest = left_budget.saturating_sub(cols(&head));
    let gist = pad_cols(&truncate_cols(&arg_gist(v.args), rest), rest);
    let mut spans = vec![Span::styled(head, name_style), Span::styled(gist, dim)];
    if right_w <= width {
        spans.push(Span::styled(right, glyph_style));
    }
    Line::from(spans)
}

/// The expanded body, per §9's declared intent. `max_lines` folds the tail with a `… N more`
/// marker rather than truncating silently.
pub fn tool_body(v: &ToolCallView<'_>, max_lines: usize) -> Vec<Line<'static>> {
    let lines = match v.intent {
        RenderIntent::Generic => generic_block(v.args, v.result, v.width, v.theme),
        RenderIntent::Terminal => terminal_block(
            v.result.map(|r| r.content.as_str()).unwrap_or(""),
            v.result,
            v.width,
            v.theme,
        ),
        RenderIntent::Diff => match diff_spec_from_args(v.args) {
            Some(spec) => diff_block(&spec, v.width, v.theme),
            None => {
                // P3-D9: never nothing. A dim note says the contract was not met, then the
                // arguments are shown the generic way.
                let mut out = vec![Line::from(Span::styled(
                    "(diff arguments not recognised — showing raw arguments)".to_string(),
                    Style::default().fg(v.theme.dim),
                ))];
                out.extend(generic_block(v.args, v.result, v.width, v.theme));
                out
            }
        },
    };
    fold(lines, max_lines, v.theme)
}

/// Keep the head, and say how much was folded away. Never a silent truncation.
fn fold(mut lines: Vec<Line<'static>>, max_lines: usize, theme: &Theme) -> Vec<Line<'static>> {
    if max_lines == 0 {
        return Vec::new();
    }
    if lines.len() <= max_lines {
        return lines;
    }
    let hidden = lines.len() - (max_lines - 1);
    lines.truncate(max_lines - 1);
    lines.push(Line::from(Span::styled(
        format!("… {hidden} more"),
        Style::default().fg(theme.dim),
    )));
    lines
}

/// GENERIC: sorted key/value block over the args object, then the result content, wrapped.
pub fn generic_block(
    args: &serde_json::Value,
    result: Option<&ToolResultBody>,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let key_style = Style::default().fg(theme.dim);
    let val_style = Style::default().fg(theme.fg);
    let mut out: Vec<Line<'static>> = Vec::new();
    if let Some(obj) = args.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for k in keys {
            let label = format!("{k}: ");
            let indent = cols(&label).min(width as usize / 2);
            let body_w = (width as usize).saturating_sub(indent).max(1) as u16;
            let value = scalar_multiline(&obj[k]);
            for (i, chunk) in wrap(&value, body_w).into_iter().enumerate() {
                if i == 0 {
                    out.push(Line::from(vec![
                        Span::styled(label.clone(), key_style),
                        Span::styled(chunk, val_style),
                    ]));
                } else {
                    out.push(Line::from(vec![
                        Span::raw(" ".repeat(indent)),
                        Span::styled(chunk, val_style),
                    ]));
                }
            }
        }
    } else if !args.is_null() {
        for chunk in wrap(&scalar_multiline(args), width) {
            out.push(Line::from(Span::styled(chunk, val_style)));
        }
    }
    if let Some(r) = result {
        if !r.content.is_empty() {
            out.push(Line::from(Span::raw("")));
            for chunk in wrap(&strip_ansi(&r.content), width) {
                out.push(Line::from(Span::styled(chunk, val_style)));
            }
        }
    }
    out
}

/// A json value for the body: strings keep their newlines, everything else is pretty json.
fn scalar_multiline(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// TERMINAL: monospace output, ANSI stripped, with the exit-code / failure line. `content` is
/// passed separately from `result` so a STREAMING call can render its output before a result
/// exists.
pub fn terminal_block(
    content: &str,
    result: Option<&ToolResultBody>,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let out_style = Style::default().fg(theme.fg);
    let mut out: Vec<Line<'static>> = wrap(&strip_ansi(content), width)
        .into_iter()
        .map(|l| Line::from(Span::styled(l, out_style)))
        .collect();
    if let Some(r) = result {
        let code = r
            .value
            .as_ref()
            .and_then(|v| v.get("exit_code"))
            .and_then(|v| v.as_i64());
        let (text, bad) = match code {
            Some(0) => ("exit 0".to_string(), false),
            Some(c) => (format!("exit {c}"), true),
            None => {
                let (_, bad) = outcome_glyph(Some(r));
                (format!("{:?}", r.outcome).to_lowercase(), bad)
            }
        };
        out.push(Line::from(Span::styled(
            text,
            Style::default().fg(if bad { theme.error } else { theme.dim }),
        )));
    }
    out
}
