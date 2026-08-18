//! The config tab: everything the harness injects, and the switch on each of
//! it.
//!
//! ONE TAB, because it is one question. This replaces the hooks tab and the
//! plugins tab, which answered two halves of "what is this thing putting into
//! my turns" and left the skills tab answering a third half read-only. A user
//! who wants to stop one thing happening should not have to know which of
//! three surfaces implemented it.
//!
//! THE INVARIANT THIS HOLDS, borrowed from both tabs it replaces because the
//! failure it prevents is the same: **an empty list, an unanswered fetch and a
//! group with nothing in it are three different screens.** `None` means nothing
//! has answered yet — say so, and never render it as "nothing installed", which
//! is a claim about the user's directories this component has not read.
//!
//! TWO LEVELS, AND THE GROUP LEVEL IS THE DEFAULT. One flat list of every hook,
//! skill and extension buries the groups: a handful of sources shipping a
//! handful of things each is fifty rows in a ten-row panel, and "what have I
//! got" cannot be answered by scrolling through the answer to a different
//! question. So the list level is groups, and you open the one you want to pick
//! through.
//!
//! TWO ANSWERS PER ITEM, AND THE ROW SAYS WHICH IS IN FORCE. An item under a
//! disabled group keeps its own switch, because turning the group back on must
//! restore the picture you left. So such a row prints `—` rather than `on`:
//! what it is set to is not what is happening, and a row that said `on` while
//! nothing ran would be the lie this avoids.
//!
//! A BROKEN THING IS LISTED WITH ITS ERROR rather than omitted. A hook that
//! silently vanished from the list is discovered as a hook that quietly never
//! fired, which is the worst way to learn it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::Deserialize;

use crate::components::panel::{legend_line, paint_rows};
use crate::components::{accent, error, info, warn};
use crate::store::selectors::clip;

/// One switchable thing, as `GET /config` serves it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigItemRow {
    /// What the switch names — `acme/guard.lua`, `local/skills/mine`.
    #[serde(default)]
    pub id: String,
    /// `hook` · `skill` · `extension`.
    #[serde(default)]
    pub surface: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    /// This item's OWN switch, which is not the whole answer when its group is
    /// off.
    #[serde(default)]
    pub enabled: bool,
    /// Whether it is actually in force: its own switch AND its group's.
    #[serde(default)]
    pub live: bool,
    /// Hooks only: listeners registered. `Some(0)` on a live hook is a hook
    /// that ran and wired nothing — a different problem from one that failed
    /// to parse, and this is what tells them apart.
    #[serde(default)]
    pub autocmds: Option<usize>,
    #[serde(default)]
    pub fired: u64,
    #[serde(default)]
    pub last: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// One group and everything under it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigGroupRow {
    #[serde(default)]
    pub id: String,
    /// `bundled` · `git` · `plugin` · `local` · `project` · `foreign` ·
    /// `foreign-plugin`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub items: Vec<ConfigItemRow>,
}

/// One row the cursor moves over. A group at the list level, one of its things
/// once you have opened it — both are switches, which is why they share a row
/// type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRow {
    /// What a toggle sends to `POST /config/:id`.
    pub id: String,
    /// What the row prints before the label: `group` at the list level, else
    /// the item's surface.
    pub kind: String,
    pub label: String,
    /// The switch this row would flip.
    pub enabled: bool,
    /// Whether the row is actually in force.
    pub live: bool,
    /// Does ⏎ open this row, or toggle it?
    pub is_group: bool,
    /// Printed dim after the label — why it is not running, or what it did.
    pub note: Option<String>,
}

/// What one group ships, as the list row says it: `1 hook · 2 skills`.
///
/// The SURFACES, not a total. "3 things" does not answer the question the list
/// level exists for, which is what this group is allowed to do — a group that
/// ships one hook and one that ships three skills are not the same risk, and
/// the hook is the one that runs code around every turn.
pub fn shipped(group: &ConfigGroupRow) -> String {
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

/// What a group IS, in the words that let you decide about it: a clone says
/// which repo and which commit, because "which code is this" is the question a
/// third-party source raises and a name cannot answer.
pub fn group_detail(group: &ConfigGroupRow) -> String {
    match (&group.repo, &group.sha) {
        (Some(repo), Some(sha)) => format!(
            "{repo}{} · {}",
            group
                .rev
                .as_ref()
                .map(|r| format!(" @{r}"))
                .unwrap_or_default(),
            &sha[..sha.len().min(7)]
        ),
        (Some(repo), None) => repo.clone(),
        _ => match group.kind.as_str() {
            "bundled" => "shipped with bough".to_string(),
            "local" => "yours".to_string(),
            "project" => "this checkout".to_string(),
            "foreign" => "another harness".to_string(),
            "foreign-plugin" => "another harness's plugins".to_string(),
            _ => group.dirs.first().cloned().unwrap_or_default(),
        },
    }
}

/// What one hook has been doing, for the row: whether it wired anything, how
/// often it has acted, and what it did last.
fn hook_note(item: &ConfigItemRow) -> Option<String> {
    if let Some(err) = &item.error {
        return Some(format!("error: {err}"));
    }
    if !item.live {
        return None;
    }
    let listeners = item.autocmds?;
    let mut note = match listeners {
        0 => "wired nothing".to_string(),
        1 => "1 listener".to_string(),
        n => format!("{n} listeners"),
    };
    if item.fired > 0 {
        note.push_str(&format!(" · fired {}", item.fired));
    }
    if let Some(last) = &item.last {
        note.push_str(&format!(" · {last}"));
    }
    Some(note)
}

/// The rows the cursor addresses at the level currently open: every group, or
/// the things inside the one named by `open`.
///
/// An `open` naming a group that is no longer there falls back to the list
/// rather than to an empty screen — a plugin can be uninstalled between two
/// fetches, and a level with nothing in it reads as a broken tab.
pub fn config_rows(groups: &[ConfigGroupRow], open: Option<&str>) -> Vec<ConfigRow> {
    if let Some(group) = open.and_then(|id| groups.iter().find(|g| g.id == id)) {
        return group
            .items
            .iter()
            .map(|item| ConfigRow {
                id: item.id.clone(),
                kind: item.surface.clone(),
                // The bare name: the header above says which group this is, so
                // the id's prefix is noise at this level.
                label: item.name.clone(),
                enabled: item.enabled,
                live: item.live,
                is_group: false,
                note: match item.surface.as_str() {
                    "hook" => hook_note(item),
                    _ => item.error.clone().map(|e| format!("error: {e}")),
                },
            })
            .collect();
    }
    groups
        .iter()
        .map(|group| ConfigRow {
            id: group.id.clone(),
            kind: group.kind.clone(),
            label: format!("{} · {}", group.id, shipped(group)),
            enabled: group.enabled,
            live: group.enabled,
            is_group: true,
            note: Some(group_detail(group)),
        })
        .collect()
}

pub struct ConfigTabProps<'a> {
    /// `None` = nothing has answered yet. Never fake an empty list.
    pub groups: Option<&'a [ConfigGroupRow]>,
    pub rows: usize,
    pub cols: usize,
    pub selected: usize,
    /// Shown instead of the list when there is nothing to show yet.
    pub note: Option<&'a str>,
    /// The group whose things are on screen, or `None` for the list.
    pub open: Option<&'a str>,
}

impl Default for ConfigTabProps<'_> {
    fn default() -> Self {
        ConfigTabProps {
            groups: None,
            rows: 10,
            cols: 96,
            selected: 0,
            note: None,
            open: None,
        }
    }
}

/// The one-line summary above the list of groups.
pub fn config_summary(groups: &[ConfigGroupRow]) -> String {
    let on = groups.iter().filter(|g| g.enabled).count();
    let items: usize = groups.iter().map(|g| g.items.len()).sum();
    let live: usize = groups
        .iter()
        .map(|g| g.items.iter().filter(|i| i.live).count())
        .sum();
    format!(
        "{on}/{} source{} on · {live}/{items} thing{} live",
        groups.len(),
        if groups.len() == 1 { "" } else { "s" },
        if items == 1 { "" } else { "s" },
    )
}

/// The line above an opened group: which group, where it is, and — when it is
/// off — that nothing under it runs whatever the rows say.
pub fn open_header(group: &ConfigGroupRow) -> String {
    format!(
        "{} · {}{}",
        group.id,
        group
            .dirs
            .first()
            .cloned()
            .unwrap_or_else(|| group_detail(group)),
        if group.enabled {
            String::new()
        } else {
            " · OFF — nothing here runs".to_string()
        }
    )
}

/// One row: the state box, what kind of thing it is, its label, and its note.
pub fn config_line(row: &ConfigRow, selected: bool, cols: usize) -> Line<'static> {
    let mark = if selected { "▸ " } else { "  " };
    // Three states, not two. `—` is "its group is off", which is neither the
    // row being off nor the row being on.
    let (state, state_style) = match (row.live, row.enabled) {
        (true, _) => ("[on] ", Style::default().fg(accent())),
        (false, true) => ("[—]  ", Style::default().fg(warn())),
        (false, false) => ("[  ] ", Style::default().add_modifier(Modifier::DIM)),
    };
    let body = Style::default();
    let label_style = match (row.live, row.is_group) {
        (true, true) => body.add_modifier(Modifier::BOLD),
        (true, false) => body,
        (false, _) => body.add_modifier(Modifier::DIM),
    };
    let kind_style = {
        let s = Style::default().fg(info());
        if row.live {
            s
        } else {
            s.add_modifier(Modifier::DIM)
        }
    };
    let budget = cols.saturating_sub(22);
    let mut spans = vec![
        Span::styled(format!("{mark}{state}"), state_style),
        Span::styled(row.kind.clone(), kind_style),
        Span::styled(format!("  {}", clip(&row.label, budget)), label_style),
    ];
    // A broken thing says so in the error colour; everything else's note is
    // dim, because it is context and not a verdict.
    let broken = row
        .note
        .as_deref()
        .is_some_and(|n| n.starts_with("error: "));
    if !row.live && row.enabled && !row.is_group {
        spans.push(Span::styled(
            "  · its source is off".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ));
    } else if let Some(note) = &row.note {
        spans.push(Span::styled(
            format!("  · {}", clip(note, budget)),
            if broken {
                Style::default().fg(error())
            } else {
                Style::default().add_modifier(Modifier::DIM)
            },
        ));
    }
    Line::from(spans)
}

pub fn render_config(props: &ConfigTabProps<'_>, area: Rect, buf: &mut Buffer) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let Some(groups) = props.groups else {
        let note = props.note.unwrap_or("reading what is installed…");
        buf.set_line(
            area.x,
            area.y,
            &Line::from(Span::styled(note.to_string(), dim)),
            area.width,
        );
        return;
    };
    if groups.is_empty() {
        paint_rows(
            &[
                Line::from(Span::styled("nothing installed".to_string(), dim)),
                Line::from(Span::styled(
                    "hooks, skills and extensions live in ~/.bough — and in a plugin directory"
                        .to_string(),
                    dim,
                )),
            ],
            area,
            buf,
        );
        return;
    }

    let open = props.open.and_then(|id| groups.iter().find(|g| g.id == id));
    let rows = config_rows(groups, props.open);
    // The head line says WHICH LEVEL you are on, because the two look alike
    // otherwise: a list of groups and a list of one group's things are both
    // rows with state boxes.
    let mut lines = vec![Line::from(Span::styled(
        match open {
            Some(group) => clip(&open_header(group), props.cols),
            None => config_summary(groups),
        },
        dim,
    ))];
    if open.is_some_and(|g| g.items.is_empty()) {
        lines.push(Line::from(Span::styled("nothing in it".to_string(), dim)));
    }
    // Chrome the list gives way to: the head line, plus the legend, which is
    // the last row of the tab and never gives up its place.
    let chrome = 1usize;
    let avail = props.rows.saturating_sub(chrome + 1);
    let at = props.selected.min(rows.len().saturating_sub(1));
    let start = at
        .saturating_sub(avail / 2)
        .min(rows.len().saturating_sub(avail.max(1)));
    for (i, row) in rows.iter().enumerate().skip(start).take(avail.max(1)) {
        lines.push(config_line(row, i == at, props.cols));
    }
    // The legend is the level's, because ⏎ means two things: it OPENS a group
    // and it switches a thing. A legend that said "on/off" on both would teach
    // the wrong key for the level you are actually on.
    let keys: Vec<String> = match open {
        Some(_) => ["⏎ on/off", "↑↓ move", "esc back"],
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

    /// A plugin shipping a hook that is off and a skill that is on.
    pub fn acme(enabled: bool) -> ConfigGroupRow {
        ConfigGroupRow {
            id: "acme".into(),
            kind: "plugin".into(),
            dirs: vec!["/home/u/.bough/plugins/acme".into()],
            enabled,
            items: vec![
                ConfigItemRow {
                    id: "acme/guard.lua".into(),
                    surface: "hook".into(),
                    name: "guard.lua".into(),
                    path: "/home/u/.bough/plugins/acme/hooks/guard.lua".into(),
                    enabled: false,
                    live: false,
                    autocmds: Some(0),
                    ..Default::default()
                },
                ConfigItemRow {
                    id: "acme/skills/review".into(),
                    surface: "skill".into(),
                    name: "review".into(),
                    path: "/home/u/.bough/plugins/acme/skills/review".into(),
                    enabled: true,
                    live: enabled,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    /// Yours: a live hook that has acted, a skill and an extension.
    pub fn local() -> ConfigGroupRow {
        ConfigGroupRow {
            id: "local".into(),
            kind: "local".into(),
            dirs: vec!["/home/u/.bough/hooks".into()],
            enabled: true,
            items: vec![
                ConfigItemRow {
                    id: "local/mine.lua".into(),
                    surface: "hook".into(),
                    name: "mine.lua".into(),
                    path: "/home/u/.bough/hooks/mine.lua".into(),
                    enabled: true,
                    live: true,
                    autocmds: Some(2),
                    fired: 3,
                    last: Some("added context".into()),
                    ..Default::default()
                },
                ConfigItemRow {
                    id: "local/skills/mine".into(),
                    surface: "skill".into(),
                    name: "mine".into(),
                    path: "/home/u/.bough/skills/mine".into(),
                    enabled: true,
                    live: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
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
    fn the_list_level_is_sources_and_says_what_each_one_ships() {
        let rows = config_rows(&[fixtures::acme(true), fixtures::local()], None);
        assert_eq!(rows.len(), 2, "one row per source, not per thing: {rows:?}");
        assert!(rows[0].is_group, "⏎ opens this row rather than toggling it");
        assert_eq!(
            text(&config_line(&rows[0], false, 96)),
            "  [on] plugin  acme · 1 hook · 1 skill · 1/2 on  · /home/u/.bough/plugins/acme",
            "the surfaces, because they are what the source is allowed to do"
        );
    }

    #[test]
    fn one_tab_holds_every_surface_including_the_two_that_had_no_switch() {
        // The whole point of the merge: a skill you wrote and an extension you
        // wrote are switchable in the same place a hook is.
        let rows = config_rows(&[fixtures::local()], Some("local"));
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["local/mine.lua", "local/skills/mine"]);
        assert!(rows.iter().all(|r| !r.is_group), "⏎ switches these");
        assert_eq!(
            text(&config_line(&rows[1], false, 96)),
            "  [on] skill  mine",
            "the bare name: the header above says which source"
        );
    }

    #[test]
    fn a_live_hook_says_what_it_wired_and_what_it_last_did() {
        let rows = config_rows(&[fixtures::local()], Some("local"));
        assert_eq!(
            text(&config_line(&rows[0], false, 96)),
            "  [on] hook  mine.lua  · 2 listeners · fired 3 · added context",
            "on with zero listeners is a different problem from failing to parse"
        );
    }

    #[test]
    fn a_thing_under_a_disabled_source_prints_neither_on_nor_off() {
        let rows = config_rows(&[fixtures::acme(false)], Some("acme"));
        let skill = &rows[1];
        assert!(skill.enabled, "it keeps its own switch");
        assert!(!skill.live);
        assert_eq!(
            text(&config_line(skill, false, 96)),
            "  [—]  skill  review  · its source is off",
            "a row that said `on` while nothing ran would be a lie"
        );
    }

    #[test]
    fn a_broken_hook_is_listed_with_its_error_rather_than_omitted() {
        let mut group = fixtures::local();
        group.items[0].error = Some("unexpected symbol".into());
        let rows = config_rows(&[group], Some("local"));
        assert!(
            text(&config_line(&rows[0], false, 96)).contains("error: unexpected symbol"),
            "a hook that vanished is discovered as one that quietly never fired"
        );
    }

    #[test]
    fn an_open_source_that_is_gone_falls_back_to_the_list_not_to_an_empty_screen() {
        let rows = config_rows(&[fixtures::acme(true)], Some("uninstalled"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "acme");
    }

    #[test]
    fn an_unanswered_fetch_is_not_an_empty_directory() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 3));
        render_config(
            &ConfigTabProps {
                groups: None,
                ..Default::default()
            },
            Rect::new(0, 0, 60, 3),
            &mut buf,
        );
        let first: String = (0..60).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            first.contains("reading"),
            "never claim nothing is installed before anything has answered: {first:?}"
        );
    }
}
