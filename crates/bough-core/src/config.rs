//! ONE listing of everything the harness injects, and one switch on each of it.
//!
//! WHY THIS EXISTS. Hooks, skills and extensions each grew their own listing,
//! their own route and their own panel tab, and two of the three had no off
//! switch outside a plugin. But the question a user actually asks is one
//! question — *what is this harness putting into my turns, and how do I stop
//! one of them* — and answering it meant three screens, one of which was
//! read-only. This module answers it once.
//!
//! ## Groups, and why a group is a switch
//!
//! Everything lands in a GROUP, and a group's id is the id prefix of everything
//! under it: `bundled`, `local`, a plugin's name, a cloned repo's slug,
//! `project`, `foreign`. So a plugin's switch and "turn off every skill that
//! shipped with bough" are the same mechanism rather than two, and the rule
//! over both is the one plugins already stated: **a group that is off
//! contributes nothing, whatever its items say** — and the items keep their own
//! switches for when it comes back.
//!
//! ## What this does NOT re-decide
//!
//! Defaults. A hook that arrived from a repo, a plugin or the bundle is OFF
//! until asked for; a hook you wrote, and every skill and extension, is ON
//! until switched off. `hooks::sources` carries that argument and this listing
//! only reports it.
//!
//! ## A disabled thing is still listed
//!
//! A group that is off is not a hook source, its skills are not resolvable and
//! its extensions are not discovered — so none of the three surfaces can be
//! asked what it holds. This walks the directories itself for exactly that
//! reason: a switchboard that could not show you what you turned off would be a
//! one-way door.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hooks::{self, SourceKind};
use crate::plugins::Surface;
use crate::skills::{self, SkillSource};
use crate::switches::Switches;

/// One switchable thing, whatever surface it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigItem {
    /// What the switch names, and what `POST /config/:id` takes.
    pub id: String,
    pub surface: Surface,
    /// The bare name: `guard.lua`, `review`, `gh.js`.
    pub name: String,
    pub path: String,
    /// This item's OWN switch, which is not the whole answer when its group is
    /// off.
    pub enabled: bool,
    /// Is it actually in force — its own switch AND its group's?
    pub live: bool,
    /// Hooks only: listeners registered, times fired, and what it did last.
    /// A hook that is on and wired nothing is a different problem from one
    /// that failed to parse, and the count is what tells them apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autocmds: Option<usize>,
    #[serde(default)]
    pub fired: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    /// Why it did not load. A broken thing is LISTED with its error rather
    /// than omitted — one that silently vanished is discovered as one that
    /// quietly never fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where a group came from — what the panel prints as its header, and the
/// thing that decides whether its hooks default on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupKind {
    /// Shipped inside the binary.
    Bundled,
    /// A repository cloned by `bough hooks add`.
    Git,
    /// A directory in `~/.bough/plugins`.
    Plugin,
    /// Yours: `~/.bough/hooks`, `~/.bough/skills`, `~/.bough/extensions`.
    Local,
    /// This checkout's `.agents` / `.claude` directories.
    Project,
    /// Another harness entirely — Claude Code, Codex — whose configuration
    /// bough adopts: its user tier and its installed plugins, under the name
    /// of the harness rather than lumped into one "foreign" pile. Which
    /// harness is putting something in the prompt is the first thing anyone
    /// looking at these rows wants to know.
    Harness,
}

impl GroupKind {
    /// Sort key: the order the panel prints groups in, which is the order the
    /// surfaces load them — the last one to speak wins a name collision.
    fn rank(self) -> u8 {
        match self {
            GroupKind::Bundled => 0,
            GroupKind::Git => 1,
            GroupKind::Plugin => 2,
            GroupKind::Harness => 3,
            GroupKind::Project => 4,
            GroupKind::Local => 5,
        }
    }
}

/// One group and everything under it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigGroup {
    /// The switch's name, and the id prefix of every item under it.
    pub id: String,
    pub kind: GroupKind,
    /// The directories walked, so "why is mine not listed?" has an answer on
    /// screen.
    pub dirs: Vec<String>,
    pub enabled: bool,
    /// Git groups only: what you are actually running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// Hooks, then skills, then extensions; name-sorted within each.
    pub items: Vec<ConfigItem>,
}

/// Everything, for one workspace. The workspace decides only the project tier
/// — a skill in `.agents/skills` belongs to the checkout, not to the machine.
pub fn list(workspace: &Path) -> Vec<ConfigGroup> {
    list_over(workspace, &crate::switches::read())
}

/// [`list`] against a given switchboard, which is how tests ask what a
/// particular set of switches produces without writing one.
pub fn list_over(workspace: &Path, state: &Switches) -> Vec<ConfigGroup> {
    let mut groups: BTreeMap<String, ConfigGroup> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();

    let group = |groups: &mut BTreeMap<String, ConfigGroup>,
                 order: &mut Vec<String>,
                 id: &str,
                 kind: GroupKind,
                 dir: String| {
        let entry = groups.entry(id.to_string()).or_insert_with(|| {
            order.push(id.to_string());
            ConfigGroup {
                id: id.to_string(),
                kind,
                dirs: Vec::new(),
                enabled: state.plugin_on(id),
                repo: None,
                rev: None,
                sha: None,
                items: Vec::new(),
            }
        });
        if !dir.is_empty() && !entry.dirs.contains(&dir) {
            entry.dirs.push(dir);
        }
    };

    // ---- hooks -----------------------------------------------------------
    // Listed against an ALL-ON switchboard: a plugin that is off is not a hook
    // source, and its hooks would vanish from the one screen that can turn it
    // back on.
    let sources = hooks::sources::sources_from(
        &hooks::sources::sources_path(),
        &hooks::sources::repos_dir(),
        &crate::paths::plugins_dir(),
        &hooks::hooks_dir(),
        &Switches::all_on(),
    );
    for file in hooks::list_hooks_over(&sources, state) {
        // The adapters are bundled BYTES under a harness's NAME, and the name
        // is what decides the section: `claude-code` holds the adapter that
        // reads Claude Code's configuration and the skills Claude Code brought,
        // which is the one place to stop taking anything from it.
        let kind = match file.kind {
            _ if is_harness(&file.source) => GroupKind::Harness,
            SourceKind::Bundled => GroupKind::Bundled,
            SourceKind::Git => GroupKind::Git,
            SourceKind::Plugin => GroupKind::Plugin,
            SourceKind::Local => GroupKind::Local,
        };
        let dir = Path::new(&file.path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        group(&mut groups, &mut order, &file.source, kind, dir);
        let entry = groups.get_mut(&file.source).expect("just inserted");
        entry.repo = file.repo.clone().or(entry.repo.take());
        entry.rev = file.rev.clone().or(entry.rev.take());
        entry.sha = file.sha.clone().or(entry.sha.take());
        let live = entry.enabled && file.enabled;
        entry.items.push(ConfigItem {
            id: file.id,
            surface: Surface::Hook,
            name: file.name,
            path: file.path,
            enabled: file.enabled,
            live,
            autocmds: Some(file.autocmds),
            fired: file.fired,
            last: file.last,
            error: file.error,
        });
    }

    // ---- skills ----------------------------------------------------------
    for rung in skills::sources_for(workspace) {
        let id = skills::switch_group(&rung);
        let kind = group_kind_for(&rung);
        group(
            &mut groups,
            &mut order,
            &id,
            kind,
            rung.dir.to_string_lossy().into_owned(),
        );
        let entry = groups.get_mut(&id).expect("just inserted");
        for name in skill_names(&rung.dir) {
            let item_id = skills::switch_id(&rung, &name);
            // A name can appear in two rungs; the switch is per rung, so both
            // are listed and each is switched where it lives.
            if entry.items.iter().any(|i| i.id == item_id) {
                continue;
            }
            let enabled = state.is_on(&item_id, true);
            entry.items.push(ConfigItem {
                id: item_id,
                surface: Surface::Skill,
                path: rung.dir.join(&name).to_string_lossy().into_owned(),
                name,
                enabled,
                live: entry.enabled && enabled,
                autocmds: None,
                fired: 0,
                last: None,
                error: None,
            });
        }
    }

    // ---- extensions ------------------------------------------------------
    for (id, dir) in crate::extensions::extension_sources(workspace) {
        let files = crate::extensions::files_in(&dir);
        if files.is_empty() && !groups.contains_key(&id) {
            continue;
        }
        let kind = match id.as_str() {
            "local" => GroupKind::Local,
            "project" => GroupKind::Project,
            _ => GroupKind::Plugin,
        };
        group(
            &mut groups,
            &mut order,
            &id,
            kind,
            dir.to_string_lossy().into_owned(),
        );
        let entry = groups.get_mut(&id).expect("just inserted");
        for (rel, path) in files {
            let item_id = crate::plugins::extension_id(&id, &rel);
            let enabled = state.is_on(&item_id, true);
            entry.items.push(ConfigItem {
                id: item_id,
                surface: Surface::Extension,
                name: rel,
                path: path.to_string_lossy().into_owned(),
                enabled,
                live: entry.enabled && enabled,
                autocmds: None,
                fired: 0,
                last: None,
                error: None,
            });
        }
    }

    // A plugin with nothing bough recognizes in it is still a plugin, and
    // still has to be listed — otherwise "I installed it and nothing happened"
    // has no screen to be answered on.
    for dir in crate::paths::plugin_dirs() {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        group(
            &mut groups,
            &mut order,
            name,
            GroupKind::Plugin,
            dir.to_string_lossy().into_owned(),
        );
    }

    let mut out: Vec<ConfigGroup> = order
        .into_iter()
        .filter_map(|id| groups.remove(&id))
        .collect();
    out.sort_by_key(|g| (g.kind.rank(), g.id.clone()));
    for g in &mut out {
        g.items
            .sort_by_key(|i| (surface_rank(i.surface), i.name.clone()));
    }
    out
}

/// Is this group id one of the harnesses bough adopts configuration from?
fn is_harness(id: &str) -> bool {
    hooks::sources::ADAPTERS.iter().any(|(name, _)| *name == id)
}

fn surface_rank(surface: Surface) -> u8 {
    match surface {
        Surface::Hook => 0,
        Surface::Skill => 1,
        Surface::Extension => 2,
    }
}

fn group_kind_for(rung: &SkillSource) -> GroupKind {
    use crate::skills::SkillSourceName as N;
    match rung.source {
        N::Bundled => GroupKind::Bundled,
        N::User => GroupKind::Local,
        N::Project => GroupKind::Project,
        // Both foreign tiers carry the harness that owns them; a rung with a
        // group that is not a harness name is one of bough's own plugins.
        N::Plugin | N::Foreign => match rung.group.as_deref() {
            Some(crate::skills::foreign::CLAUDE_CODE) | Some(crate::skills::foreign::CODEX) => {
                GroupKind::Harness
            }
            Some(_) => GroupKind::Plugin,
            None => GroupKind::Harness,
        },
    }
}

/// The skill folders in one directory, name-sorted. A folder without a
/// SKILL.md is not a skill and is not listed — the same answer the skills
/// surface gives, because a switch on something that would never load is a
/// switch that does nothing.
fn skill_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Turn one group, or one thing inside one, on or off.
///
/// Delegated to `plugins::set_enabled`, which knows the one thing that is not
/// uniform: flipping a HOOK rebuilds the interpreter, because a disabled hook
/// has to stop existing rather than stop being called.
pub fn set_enabled(id: &str, enabled: bool) -> std::io::Result<()> {
    crate::plugins::set_enabled(id, enabled)
}

/// Is this id one of the listed groups or items? The route asks before it
/// writes, so a typo cannot put a string into the state file that nothing will
/// ever match.
pub fn known(workspace: &Path, id: &str) -> bool {
    list(workspace)
        .iter()
        .any(|g| g.id == id || g.items.iter().any(|i| i.id == id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-cfg-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// One plugin shipping all three surfaces, plus a skill and an extension
    /// of the user's own.
    fn fixture(home: &Path) {
        let plugin = home.join("plugins/acme");
        std::fs::create_dir_all(plugin.join("hooks")).unwrap();
        std::fs::create_dir_all(plugin.join("skills/review")).unwrap();
        std::fs::create_dir_all(plugin.join("extensions")).unwrap();
        std::fs::write(plugin.join("hooks/guard.lua"), "").unwrap();
        std::fs::write(plugin.join("skills/review/SKILL.md"), "---\n---\nx").unwrap();
        std::fs::write(plugin.join("extensions/gh.js"), "").unwrap();
        std::fs::create_dir_all(home.join("skills/mine")).unwrap();
        std::fs::write(home.join("skills/mine/SKILL.md"), "---\n---\nx").unwrap();
        std::fs::create_dir_all(home.join("extensions")).unwrap();
        std::fs::write(home.join("extensions/tool.js"), "").unwrap();
        std::fs::create_dir_all(home.join("hooks")).unwrap();
        std::fs::write(home.join("hooks/mine.lua"), "").unwrap();
    }

    fn groups(home: &Path, ws: &Path, state: &Switches) -> Vec<ConfigGroup> {
        crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], || list_over(ws, state))
    }

    #[test]
    fn every_injected_thing_lands_in_one_listing_under_its_group() {
        let home = tmp("all");
        let ws = tmp("all-ws");
        fixture(&home);
        let out = groups(&home, &ws, &Switches::all_on());
        let acme = out.iter().find(|g| g.id == "acme").expect("the plugin");
        let ids: Vec<&str> = acme.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "acme/guard.lua",
                "acme/skills/review",
                "acme/extensions/gh.js"
            ],
            "hooks, then skills, then extensions"
        );
        // The three tiers that had no switch before this module: a skill you
        // wrote, an extension you wrote, and a hook you wrote, all under the
        // one group that means "yours".
        let local = out.iter().find(|g| g.id == "local").expect("yours");
        let ids: Vec<&str> = local.items.iter().map(|i| i.id.as_str()).collect();
        assert!(ids.contains(&"local/mine.lua"), "{ids:?}");
        assert!(ids.contains(&"local/skills/mine"), "{ids:?}");
        assert!(ids.contains(&"local/extensions/tool.js"), "{ids:?}");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn defaults_are_reported_not_re_decided() {
        let home = tmp("defaults");
        let ws = tmp("defaults-ws");
        fixture(&home);
        let out = groups(&home, &ws, &Switches::all_on());
        let item = |g: &str, id: &str| -> bool {
            out.iter()
                .find(|x| x.id == g)
                .and_then(|x| x.items.iter().find(|i| i.id == id))
                .map(|i| i.enabled)
                .unwrap_or_else(|| panic!("no {id}"))
        };
        assert!(
            !item("acme", "acme/guard.lua"),
            "a plugin's hook runs in-process on the next turn: off until asked for"
        );
        assert!(item("acme", "acme/skills/review"), "a skill is inert");
        assert!(item("acme", "acme/extensions/gh.js"));
        assert!(item("local", "local/mine.lua"), "a hook you wrote is on");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_group_that_is_off_is_still_listed_and_nothing_under_it_is_live() {
        let home = tmp("off");
        let ws = tmp("off-ws");
        fixture(&home);
        let state = Switches {
            off: vec!["acme".into()],
            ..Default::default()
        };
        let out = groups(&home, &ws, &state);
        let acme = out
            .iter()
            .find(|g| g.id == "acme")
            .expect("a switched-off group is still on screen, or the switch is a one-way door");
        assert!(!acme.enabled);
        let skill = acme
            .items
            .iter()
            .find(|i| i.surface == Surface::Skill)
            .unwrap();
        assert!(skill.enabled, "the item keeps its own switch");
        assert!(!skill.live, "and is not in force while its group is off");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn groups_print_in_load_order_so_the_last_word_is_last() {
        let home = tmp("order");
        let ws = tmp("order-ws");
        fixture(&home);
        let out = groups(&home, &ws, &Switches::all_on());
        let ranks: Vec<u8> = out.iter().map(|g| g.kind.rank()).collect();
        let mut sorted = ranks.clone();
        sorted.sort();
        assert_eq!(
            ranks,
            sorted,
            "{:?}",
            out.iter().map(|g| &g.id).collect::<Vec<_>>()
        );
        let at = |id: &str| out.iter().position(|g| g.id == id);
        assert!(at("bundled") < at("acme"), "bundled first");
        assert!(
            at("acme") < at("local"),
            "yours last: you get the last word"
        );
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&ws);
    }
}
