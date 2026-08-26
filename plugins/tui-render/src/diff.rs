//! Invariant: a `RenderIntent::Diff` tool whose arguments match none of the documented shapes
//! falls back to `generic_block` with a dim note — NEVER to nothing (P3-D9). A generic renderer
//! cannot know each tool's argument names, so the intent carries a contract, and
//! `tests/args.rs` checks that contract against `tools-baseline`'s real schemas so the two cannot
//! drift apart silently.

use bough_plugin_tui_shell::Theme;
use ratatui::text::Line;

/// The two sides of a diff, and the path that decides the syntax.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffSpec {
    pub path: Option<String>,
    pub before: String,
    pub after: String,
}

/// DIFF: `similar::TextDiff::from_lines`, unified hunks with ± gutters, each line syntax
/// highlighted by the path's extension.
pub fn diff_block(_spec: &DiffSpec, _width: u16, _theme: &Theme) -> Vec<Line<'static>> {
    todo!("WP-3")
}

/// The ARGS CONVENTION a `RenderIntent::Diff` tool must satisfy, in this order (P3-D9):
///   `{path, old, new}` | `{path, old_string, new_string}` | `{path, content}` (whole-file add).
/// `None` ⇒ the renderer falls back to `generic_block` with a dim note, never to nothing.
pub fn diff_spec_from_args(_args: &serde_json::Value) -> Option<DiffSpec> {
    todo!("WP-3")
}
