//! Plugins: one directory that ships hooks, skills and extensions — and the
//! switchboard over everything inside it.
//!
//! `paths::plugins_dir` states why a plugin is a DIRECTORY: the three surfaces
//! each had their own flat drop-box, so one coherent thing shipping all three
//! arrived as three unrelated files with nothing saying which came with which.
//! This module is the other half of that argument. A unit you can install in
//! one move is a unit you must be able to *take apart*: a plugin whose hook you
//! want and whose extension you do not is the normal case, and the answer to it
//! cannot be "delete the file the plugin will put back on its next update".
//!
//! ## One switch per thing, and the plugin is a thing too
//!
//! Two levels, and the rule between them is stated once: **a plugin that is off
//! contributes nothing, whatever its items say.** So an id is either a plugin's
//! name (`acme`) or one item inside it, and turning the plugin back on restores
//! exactly the per-item picture you left — the item switches are remembered,
//! not cleared, because "off for now" is what disabling a plugin means.
//!
//! ## A hook's switch is not stored here
//!
//! Hooks had an off switch before plugins existed, keyed by the hook's own
//! source-qualified id (`acme/guard.lua`) and living in `hooks-state.json`,
//! because turning one off is a RELOAD of the interpreter and not a flag read
//! at dispatch. That store stays the only answer for hooks: [`set_enabled`]
//! routes a hook id to [`crate::hooks::set_enabled`] and reads back through
//! [`crate::hooks::is_on`]. Two files that both claim to know whether a hook is
//! on is the bug this avoids; which file holds it is an implementation detail
//! of one id namespace.
//!
//! ## Defaults are not changed by there being a switch
//!
//! A plugin's hooks are OFF until asked for (`hooks::sources`: code that
//! arrived rather than being written must be turned on deliberately). Its
//! skills and extensions are ON, as they have been: a skill does nothing until
//! something names it, and an extension that stopped binding the day this
//! module shipped would be a working setup broken by an upgrade. The switch is
//! an opt-OUT for those two, and it says so in the panel.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hooks::SourceKind;
use crate::paths::{bough_path, plugin_dirs_in, plugins_dir};

/// Which of the three surfaces an item is. The switch does not care; the panel
/// and the "what does this plugin even do" question do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    Hook,
    Skill,
    Extension,
}

impl Surface {
    pub fn name(self) -> &'static str {
        match self {
            Surface::Hook => "hook",
            Surface::Skill => "skill",
            Surface::Extension => "extension",
        }
    }

    /// The subdirectory this surface is read from.
    pub fn dir(self) -> &'static str {
        match self {
            Surface::Hook => "hooks",
            Surface::Skill => "skills",
            Surface::Extension => "extensions",
        }
    }
}

/// One thing a plugin contributes, and its switch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginItem {
    /// What the switch names. `<plugin>/<file>.lua` for a hook — the hook's
    /// own id, because that is what `hooks-state.json` already keys on —
    /// `<plugin>/skills/<name>` and `<plugin>/extensions/<file>` for the rest.
    pub id: String,
    pub surface: Surface,
    /// The bare name: `guard.lua`, `review`, `gh.js`.
    pub name: String,
    pub path: String,
    /// This item's own switch. A disabled PLUGIN's items keep whatever they
    /// were set to and are inert regardless — `Plugin::enabled` is the other
    /// half of the answer.
    pub enabled: bool,
}

/// One plugin directory and everything in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plugin {
    /// The directory name, which is the plugin's identity.
    pub name: String,
    pub dir: String,
    pub enabled: bool,
    /// Hooks, then skills, then extensions; name-sorted within each.
    pub items: Vec<PluginItem>,
}

// ---------------------------------------------------------------------------
// The switchboard
// ---------------------------------------------------------------------------

/// `~/.bough/plugins-state.json` — beside `hooks-state.json`, and shaped like
/// the half of it that applies.
///
/// ONE LIST, not two, because unlike hooks there is no rung here whose default
/// runs the other way: a plugin and its skills and extensions are all on until
/// you say otherwise, so the only thing worth recording is what you turned off.
/// A hook's explicit "on" still needs recording, and still lives where it
/// always did.
pub fn state_path() -> PathBuf {
    bough_path(&["plugins-state.json"])
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginState {
    #[serde(default)]
    pub off: Vec<String>,
}

impl PluginState {
    /// Nothing turned off — what every caller gets on a machine with no state
    /// file, and what tests use to mean "the switchboard is not the subject".
    pub fn all_on() -> PluginState {
        PluginState::default()
    }

    /// Is this plugin contributing anything at all?
    pub fn plugin_on(&self, plugin: &str) -> bool {
        !self.off.iter().any(|o| o == plugin)
    }

    /// Is this item live: its plugin is on AND it has not been switched off.
    ///
    /// Hook ids are accepted and answered the same way, but a hook's OWN
    /// switch is not in this file — [`is_on`] is the whole answer for a hook,
    /// and this is only the plugin half.
    pub fn item_on(&self, plugin: &str, id: &str) -> bool {
        self.plugin_on(plugin) && !self.off.iter().any(|o| o == id)
    }
}

pub fn read_state(path: &Path) -> PluginState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_state(path: &Path, state: &PluginState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(state).unwrap_or_default(),
    )
}

/// The switchboard as it is on disk right now.
///
/// READ PER CALL, never cached, for the reason every other plugin surface
/// gives: a switch flipped mid-session applies on the next turn, and a
/// switchboard that needed a restart is a bug report.
pub fn state() -> PluginState {
    read_state(&state_path())
}

/// The switch's name for a plugin's skill.
pub fn skill_id(plugin: &str, name: &str) -> String {
    format!("{plugin}/skills/{name}")
}

/// The switch's name for a plugin's extension, `rel` being its path under
/// `extensions/` so a directory extension is `sub/index.js` and not `index.js`
/// — two of those in one plugin would otherwise share a switch.
pub fn extension_id(plugin: &str, rel: &str) -> String {
    format!("{plugin}/extensions/{rel}")
}

/// The plugin an id belongs to: everything before the first `/`, or the whole
/// id when it names a plugin.
pub fn plugin_of(id: &str) -> &str {
    id.split_once('/').map(|(p, _)| p).unwrap_or(id)
}

/// Does this id name a HOOK, whose switch lives in `hooks-state.json`?
///
/// The two namespaces are told apart by shape rather than by looking on disk,
/// so an id for a file that has been deleted still routes to the store that
/// holds its switch — which is what makes turning something off and removing
/// it, in either order, land in the same place.
fn is_hook_id(id: &str) -> bool {
    match id.split_once('/') {
        Some((_, rest)) => !rest.starts_with("skills/") && !rest.starts_with("extensions/"),
        None => false,
    }
}

/// Is this switch on? The one question every surface asks, whichever store
/// holds the answer.
pub fn is_on(id: &str) -> bool {
    let plugins = state();
    if !plugins.plugin_on(plugin_of(id)) {
        return false;
    }
    if is_hook_id(id) {
        return crate::hooks::is_on(
            &crate::hooks::read_state(&crate::hooks::state_path()),
            id,
            SourceKind::Plugin,
        );
    }
    !plugins.off.iter().any(|o| o == id)
}

/// Turn one plugin, or one thing inside one, on or off.
///
/// A hook is delegated: its switch is a RELOAD of the interpreter, not a flag,
/// and `hooks::set_enabled` is the thing that knows that. Turning a whole
/// PLUGIN over reloads for the same reason — its hooks stop being sources at
/// all, and a listener that was registered at load does not unregister itself.
pub fn set_enabled(id: &str, enabled: bool) -> std::io::Result<()> {
    if is_hook_id(id) {
        return crate::hooks::set_enabled(id, enabled);
    }
    let path = state_path();
    let mut state = read_state(&path);
    let was = !state.off.iter().any(|o| o == id);
    if was == enabled {
        return Ok(()); // already there
    }
    state.off.retain(|o| o != id);
    if !enabled {
        state.off.push(id.to_string());
        state.off.sort();
    }
    write_state(&path, &state)?;
    // A plugin's hooks are hook SOURCES, so switching the plugin changed which
    // Lua is loaded. Nothing else here reaches the interpreter.
    if !id.contains('/') {
        crate::hooks::reload();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// What is installed
// ---------------------------------------------------------------------------

/// Every plugin and everything in it, on or off.
///
/// This walks the plugin directories itself rather than asking the three
/// surfaces what they resolved, and it must: a disabled plugin is not a hook
/// source, its skills lose to nothing because they are never listed, and a
/// switchboard that could not show you what you turned off would be a one-way
/// door.
pub fn list() -> Vec<Plugin> {
    list_in(
        &plugins_dir(),
        &state(),
        &crate::hooks::read_state(&crate::hooks::state_path()),
    )
}

/// [`list`] with every store passed in — tests point the root somewhere
/// temporary instead of moving `BOUGH_HOME`.
pub fn list_in(root: &Path, state: &PluginState, hooks: &crate::hooks::HookState) -> Vec<Plugin> {
    plugin_dirs_in(root)
        .into_iter()
        .filter_map(|dir| {
            let name = dir.file_name()?.to_str()?.to_string();
            let mut items = Vec::new();
            for (surface, entries) in [
                (Surface::Hook, entries_for(&dir, Surface::Hook)),
                (Surface::Skill, entries_for(&dir, Surface::Skill)),
                (Surface::Extension, entries_for(&dir, Surface::Extension)),
            ] {
                for (rel, path) in entries {
                    let id = match surface {
                        Surface::Hook => format!("{name}/{rel}"),
                        Surface::Skill => skill_id(&name, &rel),
                        Surface::Extension => extension_id(&name, &rel),
                    };
                    let enabled = match surface {
                        // The hook's own store, consulted with the source kind
                        // that decides its default — OFF, because a plugin's
                        // Lua runs in-process on the next turn.
                        Surface::Hook => crate::hooks::is_on(hooks, &id, SourceKind::Plugin),
                        // On unless switched off. There is no second list to
                        // consult and no default to look up: for these two the
                        // switch is an opt-OUT, which is the whole reason this
                        // file holds one list and `hooks-state.json` holds two.
                        _ => !state.off.iter().any(|o| o == &id),
                    };
                    items.push(PluginItem {
                        id,
                        surface,
                        name: rel,
                        path: path.to_string_lossy().into_owned(),
                        enabled,
                    });
                }
            }
            Some(Plugin {
                enabled: state.plugin_on(&name),
                name,
                dir: dir.to_string_lossy().into_owned(),
                items,
            })
        })
        .collect()
}

/// One surface's items inside one plugin, as `(name, path)`, name-sorted.
///
/// Each surface's shape is its own: a hook is a `.lua` FILE, a skill is a
/// FOLDER with a `SKILL.md` in it, an extension is a loadable file or a folder
/// with an `index.*`. Discovery here answers the same way the surface itself
/// does, because a switchboard listing something the surface would not load is
/// a switch that does nothing.
fn entries_for(plugin: &Path, surface: Surface) -> Vec<(String, PathBuf)> {
    let dir = plugin.join(surface.dir());
    match surface {
        Surface::Hook => {
            let mut out: Vec<(String, PathBuf)> = read_names(&dir)
                .into_iter()
                .filter(|(_, p)| p.is_file() && p.extension().is_some_and(|e| e == "lua"))
                .collect();
            out.sort();
            out
        }
        Surface::Skill => {
            let mut out: Vec<(String, PathBuf)> = read_names(&dir)
                .into_iter()
                .filter(|(_, p)| p.join("SKILL.md").is_file())
                .collect();
            out.sort();
            out
        }
        Surface::Extension => {
            let mut out: Vec<(String, PathBuf)> = Vec::new();
            for (name, path) in read_names(&dir) {
                if path.is_dir() {
                    if let Some(index) = extension_index(&path) {
                        let rel = format!("{name}/{}", file_name(&index));
                        out.push((rel, index));
                    }
                } else if is_loadable(&path) {
                    out.push((name, path));
                }
            }
            out.sort();
            out
        }
    }
}

/// The loadable file extensions `extensions::files_in` accepts. Stated once
/// here and once there, and the tests hold them together.
const EXTENSION_EXTS: [&str; 4] = ["js", "mjs", "cjs", "ts"];

pub(crate) fn is_loadable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSION_EXTS.contains(&e))
}

/// `<dir>/index.{js,mjs,cjs,ts}`, in that order — a directory extension is its
/// index file.
pub(crate) fn extension_index(dir: &Path) -> Option<PathBuf> {
    EXTENSION_EXTS.into_iter().find_map(|ext| {
        let idx = dir.join(format!("index.{ext}"));
        idx.is_file().then_some(idx)
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_names(dir: &Path) -> Vec<(String, PathBuf)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-plug-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// A plugin with one of each surface, plus the shapes that are NOT items:
    /// a loose file in `skills/`, a folder without a SKILL.md, a `.md` beside
    /// the hooks.
    fn fixture(root: &Path, plugin: &str) {
        let dir = root.join(plugin);
        std::fs::create_dir_all(dir.join("hooks")).unwrap();
        std::fs::create_dir_all(dir.join("skills/review")).unwrap();
        std::fs::create_dir_all(dir.join("skills/draft")).unwrap();
        std::fs::create_dir_all(dir.join("extensions/big")).unwrap();
        std::fs::write(dir.join("hooks/guard.lua"), "").unwrap();
        std::fs::write(dir.join("hooks/notes.md"), "").unwrap();
        std::fs::write(dir.join("skills/review/SKILL.md"), "").unwrap();
        std::fs::write(dir.join("extensions/gh.js"), "").unwrap();
        std::fs::write(dir.join("extensions/big/index.ts"), "").unwrap();
    }

    #[test]
    fn a_plugin_lists_every_surface_and_nothing_that_is_not_one() {
        let root = tmp("list");
        fixture(&root, "acme");
        let plugins = list_in(&root, &PluginState::all_on(), &Default::default());
        assert_eq!(plugins.len(), 1);
        let ids: Vec<&str> = plugins[0].items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "acme/guard.lua",
                "acme/skills/review",
                "acme/extensions/big/index.ts",
                "acme/extensions/gh.js",
            ],
            "hooks, then skills, then extensions; \
             notes.md is not a hook and skills/draft has no SKILL.md"
        );
        // A hook's id is the hook's OWN id, because that is the key its switch
        // is already stored under.
        assert_eq!(plugins[0].items[0].surface, Surface::Hook);
        // Defaults are unchanged by there being a switch: the hook is off, the
        // rest are on.
        assert!(!plugins[0].items[0].enabled, "a plugin's hook arrives off");
        assert!(plugins[0].items[1].enabled, "a skill does not");
        assert!(plugins[0].items[3].enabled, "nor an extension");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_extension_is_switched_by_its_directory_not_its_index() {
        let root = tmp("dirext");
        fixture(&root, "acme");
        let plugins = list_in(&root, &PluginState::all_on(), &Default::default());
        let ext: Vec<&str> = plugins[0]
            .items
            .iter()
            .filter(|i| i.surface == Surface::Extension)
            .map(|i| i.id.as_str())
            .collect();
        assert!(
            ext.contains(&"acme/extensions/big/index.ts"),
            "two directory extensions would share a switch keyed on index.ts: {ext:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_plugin_that_is_off_contributes_nothing_whatever_its_items_say() {
        let root = tmp("off");
        fixture(&root, "acme");
        let state = PluginState {
            off: vec!["acme".into()],
        };
        let plugins = list_in(&root, &state, &Default::default());
        assert!(!plugins[0].enabled);
        // The ITEMS keep their own switches — turning the plugin back on must
        // restore the picture you left, not a blank one.
        assert!(plugins[0]
            .items
            .iter()
            .any(|i| i.surface == Surface::Skill && i.enabled));
        assert!(!state.item_on("acme", "acme/skills/review"));
        assert!(!state.plugin_on("acme"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_items_switch_is_read_from_the_store_that_holds_it() {
        // Skills and extensions: this file. Hooks: `hooks-state.json`, keyed by
        // the same id the hooks panel uses.
        assert!(!is_hook_id("acme/skills/review"));
        assert!(!is_hook_id("acme/extensions/gh.js"));
        assert!(!is_hook_id("acme/extensions/big/index.ts"));
        assert!(is_hook_id("acme/guard.lua"));
        assert!(!is_hook_id("acme"), "a plugin is not a hook");
        // A skill named `skills` is still a skill, because the segment that
        // decides is the surface's, not the item's.
        assert!(!is_hook_id("acme/skills/skills"));
    }

    #[test]
    fn the_plugin_of_an_id_is_everything_before_the_first_slash() {
        assert_eq!(plugin_of("acme"), "acme");
        assert_eq!(plugin_of("acme/guard.lua"), "acme");
        assert_eq!(plugin_of("acme/extensions/big/index.ts"), "acme");
    }

    #[test]
    fn switching_records_only_the_off_and_forgets_it_again() {
        let home = tmp("state");
        crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], || {
            assert!(is_on("acme/skills/review"), "on until said otherwise");
            set_enabled("acme/skills/review", false).unwrap();
            assert!(!is_on("acme/skills/review"));
            assert_eq!(state().off, vec!["acme/skills/review".to_string()]);
            // Back on removes the entry rather than recording an "on": there is
            // no rung here whose default runs the other way, so a second list
            // would be a second thing to keep true.
            set_enabled("acme/skills/review", true).unwrap();
            assert!(state().off.is_empty());
            // The plugin's switch outranks the item's.
            set_enabled("acme", false).unwrap();
            assert!(!is_on("acme/skills/review"));
            assert!(!is_on("acme/guard.lua"));
        });
        let _ = std::fs::remove_dir_all(&home);
    }
}
