//! Invariant: an UNKNOWN extension returns UNSTYLED lines rather than guessing a syntax. A wrong
//! highlight reads as a claim about the code; no highlight reads as what it is.

use bough_plugin_tui_shell::Theme;
use ratatui::text::Line;

/// syntect + two-face, fancy-regex, loaded once through a `OnceLock`. An unknown extension
/// returns unstyled lines rather than guessing.
pub fn highlight(_code: &str, _ext: Option<&str>, _theme: &Theme) -> Vec<Line<'static>> {
    todo!("WP-3")
}
