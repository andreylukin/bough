//! Invariant: the `/` palette's STATE and filtering live in `bough-plugin-commands` (which knows
//! the command list); only its DRAWING lives here, because drawing needs the [`Theme`] and the
//! commands crate cannot depend on the shell without a cycle (phase ux1 scaffold deviation D1).

use bough_plugin_commands::palette::Item;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

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
    if items.is_empty() || max_rows == 0 || width == 0 {
        return Vec::new();
    }
    // The window slides only far enough to keep the selection on screen, so a long list does not
    // re-page under the cursor on every keystroke.
    let rows = (max_rows as usize).min(items.len());
    let first = selected
        .saturating_sub(rows.saturating_sub(1))
        .min(items.len() - rows);
    items[first..first + rows]
        .iter()
        .enumerate()
        .map(|(i, item)| row(item, first + i == selected, width, theme))
        .collect()
}

/// PURE: one palette row, clipped to `width`, `usage` at body contrast and `summary` dimmed.
fn row(item: &Item, is_selected: bool, width: u16, theme: &Theme) -> Line<'static> {
    let base = if is_selected {
        Style::default().fg(theme.fg).bg(theme.sel_bg)
    } else {
        Style::default().fg(theme.fg).bg(theme.bg)
    };
    let dim = if is_selected {
        Style::default().fg(theme.fg).bg(theme.sel_bg)
    } else {
        Style::default().fg(theme.dim).bg(theme.bg)
    };
    let head = format!("{} {}", if is_selected { '>' } else { ' ' }, item.usage);
    let mut spans = vec![Span::styled(clip(&head, width as usize), base)];
    let used = head.chars().count();
    let left = (width as usize).saturating_sub(used + 2);
    if left > 0 && !item.summary.is_empty() {
        spans.push(Span::styled("  ".to_string(), dim));
        spans.push(Span::styled(clip(&item.summary, left), dim));
    }
    // Painting the row to the full width is what makes the selection read as a bar, not a word.
    let painted: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if painted < width as usize {
        spans.push(Span::styled(
            " ".repeat(width as usize - painted),
            if is_selected {
                base
            } else {
                Style::default().bg(theme.bg)
            },
        ));
    }
    Line::from(spans)
}

/// PURE: hard clip to `n` columns, never mid-nothing — the palette is one row per command.
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeName;
    use bough_plugin_commands::CommandName;

    fn items(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                name: CommandName::new(format!("c{i}")),
                usage: format!("/c{i}"),
                summary: format!("does thing {i}"),
            })
            .collect()
    }

    #[test]
    fn the_palette_is_sized_to_its_content_and_never_taller() {
        let t = Theme::of(ThemeName::Dark);
        assert!(lines(&[], 0, 40, 10, &t).is_empty());
        assert_eq!(lines(&items(3), 0, 40, 10, &t).len(), 3);
        assert_eq!(lines(&items(30), 0, 40, 10, &t).len(), 10);
    }

    #[test]
    fn the_window_follows_the_selection_and_no_row_overflows_the_width() {
        let t = Theme::of(ThemeName::Dark);
        let its = items(30);
        for sel in [0usize, 9, 10, 29] {
            let ls = lines(&its, sel, 40, 10, &t);
            assert_eq!(ls.len(), 10);
            for l in &ls {
                let w: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
                assert_eq!(w, 40, "row must paint exactly the width");
            }
            let marked: Vec<usize> = ls
                .iter()
                .enumerate()
                .filter(|(_, l)| l.spans[0].content.starts_with('>'))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(marked.len(), 1, "exactly one row is selected at sel={sel}");
        }
    }
}
