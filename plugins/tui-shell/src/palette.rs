//! Invariant: the `/` palette's STATE and filtering live in `bough-plugin-commands` (which knows
//! the command list); only its DRAWING lives here, because drawing needs the [`Theme`] and the
//! commands crate cannot depend on the shell without a cycle (phase ux1 scaffold deviation D1).

use bough_plugin_commands::palette::Item;
use ratatui::text::Line;

use crate::theme::Theme;

/// PURE: the overlay's lines, selected row highlighted, sized to `min(items, max_rows)` — it
/// never reserves rows it has no content for (M12).
pub fn lines(
    items: &[Item],
    selected: usize,
    width: u16,
    max_rows: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let _ = (items, selected, width, max_rows, theme);
    todo!("WP-5")
}
