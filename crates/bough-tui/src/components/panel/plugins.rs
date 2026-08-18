//! The plugins tab: what each installed plugin ships, and the switch on every
//! piece of it.
//!
//! THE INVARIANT THIS HOLDS, borrowed from the hooks tab because the failure it
//! prevents is the same: **an empty list, an unanswered fetch and a plugin with
//! nothing in it are three different screens.** `None` means nothing has
//! answered yet — say so, and never render it as "no plugins installed", which
//! is a claim about `~/.bough/plugins` this component has not read.
//!
//! WHY THE PLUGIN IS A ROW AND NOT A HEADER. The hooks tab rides its source
//! header on the first row of each group, deliberately, so the cursor never
//! lands somewhere it cannot act. Here the plugin IS a switch — turning the
//! whole thing off in one keystroke is the reason a plugin is a directory — so
//! it earns a row of its own, and ⏎ on it does the obvious thing.
//!
//! TWO ANSWERS PER ITEM, AND THE ROW SAYS WHICH IS IN FORCE. An item under a
//! disabled plugin keeps its own switch, because turning the plugin back on
//! must restore the picture you left. So a row under a disabled plugin prints
//! `—` rather than `on`: what it is set to is not what is happening, and a row
//! that said `on` while nothing ran would be the lie this avoids.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::Deserialize;

use crate::components::panel::{legend_line, paint_rows};
use crate::components::{accent, info, warn};
use crate::store::selectors::clip;

/// One item of one plugin, as `GET /plugins` serves it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginItemRow {
    /// What the switch names — `acme/guard.lua`, `acme/skills/review`.
    #[serde(default)]
    pub id: String,
    /// `hook` · `skill` · `extension`.
    #[serde(default)]
    pub surface: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    /// This item's OWN switch, which is not the whole answer when its plugin
    /// is off.
    #[serde(default)]
    pub enabled: bool,
}

/// One plugin directory and everything in it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginGroupRow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub items: Vec<PluginItemRow>,
}

/// One row of the flat list the cursor moves over: a plugin, or something
/// inside one. Both are switches, which is why they share a row type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRow {
    /// What ⏎ sends to `POST /plugins/:id`.
    pub id: String,
    /// `plugin` for the group row, else the item's surface.
    pub kind: String,
    /// What the row prints after the state box.
    pub label: String,
    /// The switch this row would flip.
    pub enabled: bool,
    /// Whether this row is actually in force — false for every item under a
    /// disabled plugin, however its own switch is set.
    pub live: bool,
    /// Group rows are flush; item rows are indented under them.
    pub is_plugin: bool,
}

/// The flat, cursor-addressable list. Nothing is hidden under a collapsed
/// plugin: a switch you cannot see is a switch you cannot find, and the whole
/// point of the tab is finding the one piece you want off.
pub fn plugin_rows(groups: &[PluginGroupRow]) -> Vec<PluginRow> {
    let mut out = Vec::new();
    for group in groups {
        out.push(PluginRow {
            id: group.name.clone(),
            kind: "plugin".to_string(),
            label: format!("{} · {}", group.name, group.dir),
            enabled: group.enabled,
            live: group.enabled,
            is_plugin: true,
        });
        for item in &group.items {
            out.push(PluginRow {
                id: item.id.clone(),
                kind: item.surface.clone(),
                label: item.id.clone(),
                enabled: item.enabled,
                live: group.enabled && item.enabled,
                is_plugin: false,
            });
        }
    }
    out
}

pub struct PluginsTabProps<'a> {
    /// `None` = nothing has answered yet. Never fake an empty list.
    pub plugins: Option<&'a [PluginGroupRow]>,
    /// The directory that was walked, printed under the list: "why is my
    /// plugin not listed?" is almost always answered by naming it.
    pub dir: Option<&'a str>,
    pub rows: usize,
    pub cols: usize,
    pub selected: usize,
    /// Shown instead of the list when there is nothing to show yet.
    pub note: Option<&'a str>,
}

impl Default for PluginsTabProps<'_> {
    fn default() -> Self {
        PluginsTabProps {
            plugins: None,
            dir: None,
            rows: 10,
            cols: 96,
            selected: 0,
            note: None,
        }
    }
}

/// The one-line summary above the list.
pub fn plugins_summary(groups: &[PluginGroupRow]) -> String {
    let on = groups.iter().filter(|p| p.enabled).count();
    let items: usize = groups.iter().map(|p| p.items.len()).sum();
    let live: usize = groups
        .iter()
        .filter(|p| p.enabled)
        .map(|p| p.items.iter().filter(|i| i.enabled).count())
        .sum();
    format!(
        "{on}/{} plugin{} on · {live}/{items} piece{} live",
        groups.len(),
        if groups.len() == 1 { "" } else { "s" },
        if items == 1 { "" } else { "s" },
    )
}

/// One row: the state box, what kind of thing it is, and its id.
pub fn plugin_line(row: &PluginRow, selected: bool, cols: usize) -> Line<'static> {
    let mark = if selected { "▸ " } else { "  " };
    // Three states, not two. `—` is "its plugin is off", which is neither the
    // row being off nor the row being on.
    let (state, state_style) = match (row.live, row.enabled) {
        (true, _) => ("[on] ", Style::default().fg(accent())),
        (false, true) => ("[—]  ", Style::default().fg(warn())),
        (false, false) => ("[  ] ", Style::default().add_modifier(Modifier::DIM)),
    };
    let body = Style::default();
    let label_style = if row.live {
        if row.is_plugin {
            body.add_modifier(Modifier::BOLD)
        } else {
            body
        }
    } else {
        body.add_modifier(Modifier::DIM)
    };
    let indent = if row.is_plugin { "" } else { "  " };
    let mut spans = vec![
        Span::styled(format!("{mark}{state}"), state_style),
        Span::styled(format!("{indent}{}", row.kind), {
            let s = Style::default().fg(info());
            if row.live {
                s
            } else {
                s.add_modifier(Modifier::DIM)
            }
        }),
        Span::styled(
            format!("  {}", clip(&row.label, cols.saturating_sub(20))),
            label_style,
        ),
    ];
    if !row.live && row.enabled && !row.is_plugin {
        spans.push(Span::styled(
            "  · its plugin is off".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

pub fn render_plugins(props: &PluginsTabProps<'_>, area: Rect, buf: &mut Buffer) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let Some(groups) = props.plugins else {
        let note = props.note.unwrap_or("reading ~/.bough/plugins…");
        buf.set_line(
            area.x,
            area.y,
            &Line::from(Span::styled(note.to_string(), dim)),
            area.width,
        );
        return;
    };
    if groups.is_empty() {
        let mut lines = vec![Line::from(Span::styled(
            "no plugins installed".to_string(),
            dim,
        ))];
        if let Some(dir) = props.dir {
            lines.push(Line::from(Span::styled(
                format!("a plugin is one directory in {dir}, holding hooks/ skills/ extensions/"),
                dim,
            )));
        }
        paint_rows(&lines, area, buf);
        return;
    }

    let rows = plugin_rows(groups);
    let mut lines = vec![Line::from(Span::styled(plugins_summary(groups), dim))];
    // Chrome the list gives way to: the summary and the directory line, plus
    // the legend, which is the last row of the tab and never gives up its place.
    let chrome = 2usize;
    let avail = props.rows.saturating_sub(chrome + 1);
    let at = props.selected.min(rows.len().saturating_sub(1));
    let start = at
        .saturating_sub(avail / 2)
        .min(rows.len().saturating_sub(avail.max(1)));
    for (i, row) in rows.iter().enumerate().skip(start).take(avail.max(1)) {
        lines.push(plugin_line(row, i == at, props.cols));
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
pub mod fixtures {
    use super::*;

    pub fn acme(enabled: bool) -> PluginGroupRow {
        PluginGroupRow {
            name: "acme".into(),
            dir: "/home/u/.bough/plugins/acme".into(),
            enabled,
            items: vec![
                PluginItemRow {
                    id: "acme/guard.lua".into(),
                    surface: "hook".into(),
                    name: "guard.lua".into(),
                    path: "/home/u/.bough/plugins/acme/hooks/guard.lua".into(),
                    enabled: false,
                },
                PluginItemRow {
                    id: "acme/skills/review".into(),
                    surface: "skill".into(),
                    name: "review".into(),
                    path: "/home/u/.bough/plugins/acme/skills/review".into(),
                    enabled: true,
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn the_plugin_and_everything_in_it_are_rows_the_cursor_can_act_on() {
        let rows = plugin_rows(&[fixtures::acme(true)]);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            ["acme", "acme/guard.lua", "acme/skills/review"],
            "the plugin's own switch is a row, not a header"
        );
        assert!(rows[0].is_plugin);
        assert!(!rows[1].is_plugin);
    }

    #[test]
    fn a_row_under_a_disabled_plugin_says_it_is_not_in_force_rather_than_on() {
        let rows = plugin_rows(&[fixtures::acme(false)]);
        let skill = rows.iter().find(|r| r.id == "acme/skills/review").unwrap();
        assert!(
            skill.enabled && !skill.live,
            "its own switch is untouched; nothing is running"
        );
        assert_eq!(
            text(&plugin_line(skill, false, 96)),
            "  [—]    skill  acme/skills/review  · its plugin is off"
        );
        // And the plugin row itself is simply off.
        assert_eq!(
            text(&plugin_line(&rows[0], false, 96)),
            "  [  ] plugin  acme · /home/u/.bough/plugins/acme"
        );
    }

    #[test]
    fn a_row_says_whether_it_is_on_and_selection_is_a_mark_not_a_colour() {
        let rows = plugin_rows(&[fixtures::acme(true)]);
        assert_eq!(
            text(&plugin_line(&rows[2], false, 96)),
            "  [on]   skill  acme/skills/review"
        );
        // A plugin's hook arrives off, and the row says so.
        assert_eq!(
            text(&plugin_line(&rows[1], false, 96)),
            "  [  ]   hook  acme/guard.lua"
        );
        assert!(text(&plugin_line(&rows[0], true, 96)).starts_with("▸ "));
    }

    #[test]
    fn the_summary_counts_what_is_live_not_what_is_merely_switched_on() {
        assert_eq!(
            plugins_summary(&[fixtures::acme(true)]),
            "1/1 plugin on · 1/2 pieces live"
        );
        assert_eq!(
            plugins_summary(&[fixtures::acme(false)]),
            "0/1 plugin on · 0/2 pieces live",
            "a disabled plugin's items are not live, whatever they are set to"
        );
    }

    #[test]
    fn an_unanswered_fetch_is_not_an_empty_directory() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 4));
        render_plugins(
            &PluginsTabProps {
                plugins: None,
                ..Default::default()
            },
            Rect::new(0, 0, 60, 4),
            &mut buf,
        );
        let first: String = (0..60).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(first.contains("reading"), "{first:?}");
        assert!(
            !first.contains("no plugins"),
            "an unanswered fetch never claims the directory is empty"
        );
    }
}
