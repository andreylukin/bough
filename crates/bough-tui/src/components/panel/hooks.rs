//! The hooks tab: the Lua that runs inside the loop, and its off switches.
//!
//! THE INVARIANT THIS HOLDS, borrowed wholesale from the skills tab because
//! the failure it prevents is the same: **an empty list, an unanswered fetch
//! and a BROKEN hook are three different screens.** `None` means nothing has
//! answered yet — say so, and never render it as "no hooks installed", which
//! is a claim about the user's `~/.bough/hooks` that this component has not
//! read. A hook whose Lua does not parse is listed WITH its error rather than
//! omitted; a hook that silently vanished from the list is discovered as a
//! hook that quietly never fired, which is the worst way to learn it.
//!
//! The row says three things a toggle screen has to say: whether it is on,
//! whether it LOADED, and what it wired. "on" with zero listeners is a hook
//! that ran and registered nothing — a different problem from one that failed
//! to parse, and the count is what tells them apart.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::Deserialize;

use crate::components::panel::{legend_line, paint_rows};
use crate::components::{accent, error, info, warn};
use crate::store::selectors::clip;

/// One row of `GET /hooks`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookRow {
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub enabled: bool,
    /// Listeners this file registered. Zero on a disabled hook, because a
    /// disabled hook is not loaded at all.
    #[serde(default)]
    pub autocmds: usize,
    /// How many times it has acted, and what it did last.
    #[serde(default)]
    pub fired: u64,
    #[serde(default)]
    pub last: Option<String>,
    /// Why it did not load, when it did not.
    #[serde(default)]
    pub error: Option<String>,
}

pub struct HooksTabProps<'a> {
    /// `None` = nothing has answered yet. Never fake an empty list.
    pub hooks: Option<&'a [HookRow]>,
    /// The directory that was walked, printed under the list: "why is my hook
    /// not listed?" is almost always answered by naming it.
    pub dir: Option<&'a str>,
    pub rows: usize,
    pub cols: usize,
    pub selected: usize,
    /// Shown instead of the list when there is nothing to show yet.
    pub note: Option<&'a str>,
}

impl Default for HooksTabProps<'_> {
    fn default() -> Self {
        HooksTabProps {
            hooks: None,
            dir: None,
            rows: 10,
            cols: 96,
            selected: 0,
            note: None,
        }
    }
}

/// The one-line summary above the list.
pub fn hooks_summary(hooks: &[HookRow]) -> String {
    let on = hooks.iter().filter(|h| h.enabled).count();
    let broken = hooks.iter().filter(|h| h.error.is_some()).count();
    let listeners: usize = hooks.iter().map(|h| h.autocmds).sum();
    let mut text = format!(
        "{on}/{} on · {listeners} listener{}",
        hooks.len(),
        if listeners == 1 { "" } else { "s" },
    );
    if broken > 0 {
        text.push_str(&format!(" · {broken} failed to load"));
    }
    text
}

/// One row: state, name, and what it wired or why it did not.
pub fn hook_line(hook: &HookRow, selected: bool, cols: usize) -> Line<'static> {
    let mark = if selected { "▸ " } else { "  " };
    let state = if hook.enabled { "[on] " } else { "[  ] " };
    let mut spans = vec![
        Span::styled(
            format!("{mark}{state}"),
            if hook.enabled {
                Style::default().fg(accent())
            } else {
                Style::default().add_modifier(Modifier::DIM)
            },
        ),
        Span::styled(
            hook.name.clone(),
            if hook.enabled {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            },
        ),
    ];
    let detail = match (&hook.error, hook.enabled, hook.autocmds) {
        (Some(err), _, _) => (
            format!("  {}", clip(err, cols.saturating_sub(30))),
            Style::default().fg(error()),
        ),
        (None, false, _) => (
            "  off".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        // Loaded and wired nothing: not an error, but not what the author
        // meant either, and the only place it is visible is here.
        (None, true, 0) => ("  no listeners".to_string(), Style::default().fg(warn())),
        (None, true, n) => (
            format!("  {n} listener{}", if n == 1 { "" } else { "s" }),
            Style::default().fg(info()),
        ),
    };
    spans.push(Span::styled(detail.0, detail.1));
    // What it has actually DONE, which is the question the listener count
    // cannot answer: a hook wired to an event that never fires and one that
    // rewrites every command look the same until this column exists.
    if hook.enabled && hook.fired > 0 {
        let last = hook.last.clone().unwrap_or_else(|| "acted".into());
        spans.push(Span::styled(
            format!("  · fired {}× · {last}", hook.fired),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

pub fn render_hooks(props: &HooksTabProps<'_>, area: Rect, buf: &mut Buffer) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let Some(hooks) = props.hooks else {
        let note = props.note.unwrap_or("reading ~/.bough/hooks…");
        buf.set_line(
            area.x,
            area.y,
            &Line::from(Span::styled(note.to_string(), dim)),
            area.width,
        );
        return;
    };
    if hooks.is_empty() {
        let mut lines = vec![Line::from(Span::styled(
            "no hooks installed".to_string(),
            dim,
        ))];
        if let Some(dir) = props.dir {
            lines.push(Line::from(Span::styled(
                format!("drop a .lua file in {dir}"),
                dim,
            )));
        }
        paint_rows(&lines, area, buf);
        return;
    }

    let mut lines = vec![Line::from(Span::styled(hooks_summary(hooks), dim))];
    // The legend and the directory line are chrome the list gives way to.
    let chrome = 2usize;
    let avail = (props.rows).saturating_sub(chrome + 1);
    let at = props.selected.min(hooks.len().saturating_sub(1));
    let start = at
        .saturating_sub(avail / 2)
        .min(hooks.len().saturating_sub(avail.max(1)));
    for (i, hook) in hooks.iter().enumerate().skip(start).take(avail.max(1)) {
        lines.push(hook_line(hook, i == at, props.cols));
    }
    if let Some(dir) = props.dir {
        lines.push(Line::from(Span::styled(
            clip(&format!("read from {dir}"), props.cols),
            dim,
        )));
    }
    lines.push(Line::from(Span::styled(
        legend_line(
            &["⏎ toggle", "↑↓ move", "esc back"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            Some(props.cols),
        ),
        dim,
    )));
    paint_rows(&lines, area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, enabled: bool, autocmds: usize, error: Option<&str>) -> HookRow {
        HookRow {
            name: name.into(),
            path: format!("/home/u/.bough/hooks/{name}"),
            enabled,
            autocmds,
            fired: 0,
            last: None,
            error: error.map(String::from),
        }
    }

    fn acted(name: &str, fired: u64, last: &str) -> HookRow {
        HookRow {
            fired,
            last: Some(last.into()),
            ..row(name, true, 1, None)
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn a_row_says_whether_it_is_on_and_what_it_wired() {
        assert_eq!(
            text(&hook_line(&row("fmt.lua", true, 2, None), false, 96)),
            "  [on] fmt.lua  2 listeners"
        );
        assert_eq!(
            text(&hook_line(&row("fmt.lua", false, 0, None), false, 96)),
            "  [  ] fmt.lua  off"
        );
        // Selected is a mark, not a colour alone — the panel is read on
        // terminals that flatten styling.
        assert!(text(&hook_line(&row("fmt.lua", true, 1, None), true, 96)).starts_with("▸ "));
    }

    #[test]
    fn a_row_says_what_the_hook_has_actually_done_not_only_what_it_wired() {
        assert_eq!(
            text(&hook_line(
                &acted("guard.lua", 3, "denied a command"),
                false,
                96
            )),
            "  [on] guard.lua  1 listener  · fired 3× · denied a command"
        );
        // Never fired: no activity column at all, rather than a "0×" that
        // reads as a measurement of something.
        assert_eq!(
            text(&hook_line(&row("idle.lua", true, 1, None), false, 96)),
            "  [on] idle.lua  1 listener"
        );
    }

    #[test]
    fn a_hook_that_loaded_but_wired_nothing_says_so_rather_than_reading_as_healthy() {
        assert_eq!(
            text(&hook_line(&row("empty.lua", true, 0, None), false, 96)),
            "  [on] empty.lua  no listeners"
        );
    }

    #[test]
    fn a_broken_hook_shows_its_reason_not_a_blank_row() {
        let line = text(&hook_line(
            &row("bad.lua", true, 0, Some("syntax error near '('")),
            false,
            96,
        ));
        assert!(line.contains("bad.lua"), "{line}");
        assert!(line.contains("syntax error"), "{line}");
    }

    #[test]
    fn the_summary_counts_what_is_on_what_is_wired_and_what_failed() {
        let hooks = [
            row("a.lua", true, 2, None),
            row("b.lua", false, 0, None),
            row("c.lua", true, 0, Some("boom")),
        ];
        assert_eq!(
            hooks_summary(&hooks),
            "2/3 on · 2 listeners · 1 failed to load"
        );
        assert_eq!(
            hooks_summary(&[row("a.lua", true, 1, None)]),
            "1/1 on · 1 listener"
        );
    }

    #[test]
    fn an_unanswered_fetch_is_never_rendered_as_an_empty_directory() {
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        render_hooks(
            &HooksTabProps {
                hooks: None,
                note: Some("could not reach the server"),
                ..Default::default()
            },
            area,
            &mut buf,
        );
        let painted: String = (0..60)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect::<String>();
        assert!(painted.contains("could not reach"), "{painted}");
        assert!(
            !painted.contains("no hooks installed"),
            "an unanswered fetch must not claim the directory is empty: {painted}"
        );
    }

    #[test]
    fn an_empty_directory_says_where_to_put_one() {
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        render_hooks(
            &HooksTabProps {
                hooks: Some(&[]),
                dir: Some("/home/u/.bough/hooks"),
                ..Default::default()
            },
            area,
            &mut buf,
        );
        let row1: String = (0..60).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(row1.contains("drop a .lua file in"), "{row1}");
    }
}
