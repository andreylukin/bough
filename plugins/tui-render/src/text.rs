//! Invariant: wrapping is GRAPHEME-AWARE and width-correct. A CJK cell is two columns and a
//! combining mark is zero, so a wrapped line never overflows its pane and never splits a cluster.

use bough_plugin_tui_shell::Theme;
use ratatui::text::Line;

/// Assistant text: wrap at `width`, style `**bold**` and `` `code` ``, and highlight fenced
/// blocks through [`crate::highlight`]. No termimad in this phase (P3-D10).
pub fn markdownish(_text: &str, _width: u16, _theme: &Theme) -> Vec<Line<'static>> {
    todo!("WP-3")
}

/// Grapheme-aware wrapping used by all of the above.
pub fn wrap(_text: &str, _width: u16) -> Vec<String> {
    todo!("WP-3")
}

/// ANSI stripped, for the TERMINAL intent.
pub fn strip_ansi(_text: &str) -> String {
    todo!("WP-3")
}
