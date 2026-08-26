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
use ratatui::text::Line;

pub use about::{about_from_step, AboutView};
pub use diff::{diff_block, diff_spec_from_args, DiffSpec};
pub use highlight::highlight;
pub use text::{markdownish, wrap};

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

/// The collapsed header: `▸ bash  ls -la …            ✓ 0.4s`. Always exactly one line.
pub fn tool_header(_v: &ToolCallView<'_>) -> Line<'static> {
    todo!("WP-3")
}

/// The expanded body, per §9's declared intent. `max_lines` folds the tail with a `… N more`
/// marker rather than truncating silently.
pub fn tool_body(_v: &ToolCallView<'_>, _max_lines: usize) -> Vec<Line<'static>> {
    todo!("WP-3")
}

/// GENERIC: sorted key/value block over the args object, then the result content, wrapped.
pub fn generic_block(
    _args: &serde_json::Value,
    _result: Option<&ToolResultBody>,
    _width: u16,
    _theme: &Theme,
) -> Vec<Line<'static>> {
    todo!("WP-3")
}

/// TERMINAL: monospace output, ANSI stripped, with the exit-code / failure line.
pub fn terminal_block(
    _content: &str,
    _result: Option<&ToolResultBody>,
    _width: u16,
    _theme: &Theme,
) -> Vec<Line<'static>> {
    todo!("WP-3")
}
