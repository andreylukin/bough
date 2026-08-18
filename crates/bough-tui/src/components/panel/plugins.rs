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

/// One row the cursor moves over. A plugin at the list level, one of its
/// pieces once you have opened it — both are switches, which is why they share
/// a row type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRow {
    /// What a toggle sends to `POST /plugins/:id`.
    pub id: String,
    /// `plugin` at the list level, else the item's surface.
    pub kind: String,
    /// What the row prints after the state box.
    pub label: String,
    /// The switch this row would flip.
    pub enabled: bool,
    /// Whether this row is actually in force — false for every piece of a
    /// disabled plugin, however its own switch is set.
    pub live: bool,
    /// Does ⏎ open this row, or toggle it?
    pub is_plugin: bool,
}

/// What one plugin ships, as the list row says it: `1 hook · 2 skills`.
///
/// The SURFACES, not a total. "3 pieces" does not answer the question the list
/// level exists for, which is what this plugin is allowed to do — a plugin that
/// ships one hook and one that ships three skills are not the same risk, and
/// the hook is the one that runs code around every turn.
fn shipped(group: &PluginGroupRow) -> String {
    let count = |surface: &str| group.items.iter().filter(|i| i.surface == surface).count();
    let part = |n: usize, noun: &str| match n {
        0 => None,
        1 => Some(format!("1 {noun}")),
        n => Some(format!("{n} {noun}s")),
    };
    let parts: Vec<String> = [
        part(count("hook"), "hook"),
        part(count("skill"), "skill"),
        part(count("extension"), "extension"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return "nothing in it".to_string();
    }
    let on = group.items.iter().filter(|i| i.enabled).count();
    format!("{} · {on}/{} on", parts.join(" · "), group.items.len())
}

/// The rows the cursor addresses at the level currently open: every plugin, or
/// the pieces of the one named by `open`.
///
/// TWO LEVELS, AND THE LIST LEVEL IS THE DEFAULT. One flat list of every piece
/// of every plugin buries the plugins themselves: five plugins shipping eight
/// things each is forty rows in a ten-row panel, and the question "what have I
/// installed" cannot be answered by scrolling through the answer to a different
/// one. So the list level is plugins, and you open the one you want to pick
/// through.
///
/// An `open` naming a plugin that is no longer there falls back to the list
/// rather than to an empty screen — a plugin can be uninstalled between two
/// fetches, and a level with nothing in it reads as a broken tab.
pub fn plugin_rows(groups: &[PluginGroupRow], open: Option<&str>) -> Vec<PluginRow> {
    if let Some(group) = open.and_then(|name| groups.iter().find(|g| g.name == name)) {
        return group
            .items
            .iter()
            .map(|item| PluginRow {
                id: item.id.clone(),
                kind: item.surface.clone(),
                // The bare name: the header above says which plugin this is,
                // so the id's prefix is noise at this level.
                label: item.name.clone(),
                enabled: item.enabled,
                live: group.enabled && item.enabled,
                is_plugin: false,
            })
            .collect();
    }
    groups
        .iter()
        .map(|group| PluginRow {
            id: group.name.clone(),
            kind: "plugin".to_string(),
            label: format!("{} · {}", group.name, shipped(group)),
            enabled: group.enabled,
            live: group.enabled,
            is_plugin: true,
        })
        .collect()
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
    /// The plugin whose pieces are on screen, or `None` for the list of
    /// plugins.
    pub open: Option<&'a str>,
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
            open: None,
        }
    }
}

/// The one-line summary above the list of plugins.
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

/// The line above an opened plugin's pieces: which plugin, where it is, and —
/// when it is off — that nothing under it is running whatever the rows say.
pub fn open_header(group: &PluginGroupRow) -> String {
    format!(
        "{} · {}{}",
        group.name,
        group.dir,
        if group.enabled {
            String::new()
        } else {
            " · OFF — nothing here runs".to_string()
        }
    )
}

/// One row: the state box, what kind of thing it is, and its label.
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
    let mut spans = vec![
        Span::styled(format!("{mark}{state}"), state_style),
        Span::styled(row.kind.clone(), {
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

    let open = props
        .open
        .and_then(|name| groups.iter().find(|g| g.name == name));
    let rows = plugin_rows(groups, props.open);
    // The head line says WHICH LEVEL you are on, because the two look alike
    // otherwise: a list of plugins and a list of one plugin's pieces are both
    // rows with state boxes.
    let mut lines = vec![Line::from(Span::styled(
        match open {
            Some(group) => clip(&open_header(group), props.cols),
            None => plugins_summary(groups),
        },
        dim,
    ))];
    if open.is_some_and(|g| g.items.is_empty()) {
        lines.push(Line::from(Span::styled("nothing in it".to_string(), dim)));
    }
    // Chrome the list gives way to: the head line and the directory line, plus
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
    // The legend is the level's, because ⏎ means two things: it OPENS a plugin
    // and it switches a piece. A legend that said "toggle" on both would teach
    // the wrong key for the level you are actually on.
    let keys: Vec<String> = match open {
        Some(_) => ["⏎ on/off", "↑↓ move", "esc plugins"],
        None => ["⏎ open", "x on/off", "↑↓ move"],
    }
    .iter()
    .map(|s| s.to_string())
    .collect();
    lines.push(Line::from(Span::styled(
        legend_line(&keys, Some(props.cols)),
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

    /// A second plugin, so "opening one shows its pieces" has something to be
    /// true against.
    pub fn other() -> PluginGroupRow {
        PluginGroupRow {
            name: "other".into(),
            dir: "/home/u/.bough/plugins/other".into(),
            enabled: true,
            items: vec![PluginItemRow {
                id: "other/fmt.js".into(),
                surface: "extension".into(),
                name: "fmt.js".into(),
                path: "/home/u/.bough/plugins/other/extensions/fmt.js".into(),
                enabled: true,
            }],
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
    fn the_list_level_is_plugins_and_says_what_each_one_ships() {
        let rows = plugin_rows(&[fixtures::acme(true)], None);
        assert_eq!(rows.len(), 1, "one row per plugin, not per piece: {rows:?}");
        assert_eq!(rows[0].id, "acme");
        assert!(
            rows[0].is_plugin,
            "⏎ opens this row rather than toggling it"
        );
        assert_eq!(
            text(&plugin_line(&rows[0], false, 96)),
            "  [on] plugin  acme · 1 hook · 1 skill · 1/2 on",
            "the surfaces, because they are what the plugin is allowed to do"
        );
    }

    #[test]
    fn opening_one_shows_its_pieces_and_nothing_elses() {
        let groups = [fixtures::acme(true), fixtures::other()];
        let rows = plugin_rows(&groups, Some("acme"));
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["acme/guard.lua", "acme/skills/review"]);
        assert!(rows.iter().all(|r| !r.is_plugin), "⏎ switches these");
        // The bare name, because the header above already says which plugin.
        assert_eq!(
            text(&plugin_line(&rows[1], false, 96)),
            "  [on] skill  review"
        );
    }

    #[test]
    fn an_open_plugin_that_is_gone_falls_back_to_the_list_not_to_an_empty_screen() {
        let rows = plugin_rows(&[fixtures::acme(true)], Some("uninstalled"));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].id, "acme",
            "a plugin can vanish between two fetches"
        );
    }

    #[test]
    fn a_piece_of_a_disabled_plugin_says_it_is_not_in_force_rather_than_on() {
        let rows = plugin_rows(&[fixtures::acme(false)], Some("acme"));
        let skill = rows.iter().find(|r| r.id == "acme/skills/review").unwrap();
        assert!(
            skill.enabled && !skill.live,
            "its own switch is untouched; nothing is running"
        );
        assert_eq!(
            text(&plugin_line(skill, false, 96)),
            "  [—]  skill  review  · its plugin is off"
        );
        // And the plugin's own row, back at the list level, is simply off.
        let list = plugin_rows(&[fixtures::acme(false)], None);
        assert_eq!(
            text(&plugin_line(&list[0], false, 96)),
            "  [  ] plugin  acme · 1 hook · 1 skill · 1/2 on"
        );
    }

    #[test]
    fn a_plugins_hook_arrives_off_and_selection_is_a_mark_not_a_colour() {
        let rows = plugin_rows(&[fixtures::acme(true)], Some("acme"));
        assert_eq!(
            text(&plugin_line(&rows[0], false, 96)),
            "  [  ] hook  guard.lua"
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
            "a disabled plugin's pieces are not live, whatever they are set to"
        );
    }

    #[test]
    fn a_plugin_with_nothing_in_it_says_so_at_both_levels() {
        let empty = PluginGroupRow {
            name: "hollow".into(),
            dir: "/p/hollow".into(),
            enabled: true,
            items: vec![],
        };
        assert_eq!(
            text(&plugin_line(
                &plugin_rows(std::slice::from_ref(&empty), None)[0],
                false,
                96
            )),
            "  [on] plugin  hollow · nothing in it"
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 5));
        render_plugins(
            &PluginsTabProps {
                plugins: Some(&[empty]),
                open: Some("hollow"),
                rows: 5,
                cols: 60,
                ..Default::default()
            },
            Rect::new(0, 0, 60, 5),
            &mut buf,
        );
        let second: String = (0..60).map(|x| buf[(x, 1)].symbol()).collect();
        assert!(second.contains("nothing in it"), "{second:?}");
    }

    #[test]
    fn the_legend_teaches_the_key_for_the_level_you_are_on() {
        let paint = |open: Option<&str>| -> String {
            let groups = [fixtures::acme(true)];
            let area = Rect::new(0, 0, 70, 6);
            let mut buf = Buffer::empty(area);
            render_plugins(
                &PluginsTabProps {
                    plugins: Some(&groups),
                    open,
                    rows: 6,
                    cols: 70,
                    ..Default::default()
                },
                area,
                &mut buf,
            );
            (0..6)
                .map(|y| (0..70).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let list = paint(None);
        assert!(list.contains("⏎ open"), "{list}");
        assert!(list.contains("x on/off"), "{list}");
        let inside = paint(Some("acme"));
        assert!(inside.contains("⏎ on/off"), "{inside}");
        assert!(inside.contains("esc plugins"), "{inside}");
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
