//! ONE switchboard for everything the harness injects.
//!
//! WHY ONE FILE. Hooks, skills and extensions arrived at different times and
//! each brought its own store: `hooks-state.json` held two lists, then
//! `plugins-state.json` held one more, and `plugins::set_enabled` already had
//! to route an id back into the hooks store to answer a question about a
//! plugin's `guard.lua`. Two files that both claim to know whether something
//! is on is the bug that routing was working around. There is one namespace of
//! ids, so there is one file: `~/.bough/switches.json`.
//!
//! TWO LISTS, because the default is not the same everywhere. A hook you wrote
//! is on until you say otherwise; a hook that arrived from a repo or a plugin
//! is off until you say otherwise (`hooks::sources` carries the argument). A
//! store with only an off-list cannot express "on, and I mean it" — and that
//! is exactly what has to survive an upgrade that changes its mind about a
//! default.
//!
//! EVERY SWITCH IS RECORDED EXPLICITLY, on either way. The plugin store used to
//! record only the offs and forget an id when it went back on, which is fine
//! until the default under it moves. One rule for every id costs one line in a
//! JSON file and removes the only case where the answer depends on code rather
//! than on what you said.
//!
//! READ PER CALL, never cached: a switch flipped mid-session applies on the
//! next turn, and a switchboard that needed a restart is a bug report.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::bough_path;

/// The state file. Everything the panel can toggle keys into this one.
pub fn path() -> PathBuf {
    bough_path(&["switches.json"])
}

/// The stores this one replaces, read only to migrate off. Folded at READ
/// time rather than rewritten, so a downgrade still finds its own file where
/// it left it and nothing is destroyed by merely launching a new binary.
fn legacy_hooks_state() -> PathBuf {
    bough_path(&["hooks-state.json"])
}

fn legacy_plugins_state() -> PathBuf {
    bough_path(&["plugins-state.json"])
}

/// The pre-sources hooks file: bare names, all of them local.
fn legacy_disabled() -> PathBuf {
    bough_path(&["hooks-disabled.json"])
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switches {
    /// Ids turned on explicitly, whatever their default.
    #[serde(default)]
    pub on: Vec<String>,
    /// Ids turned off explicitly, whatever their default.
    #[serde(default)]
    pub off: Vec<String>,
}

impl Switches {
    /// Nothing said about anything — what every caller gets on a fresh machine,
    /// and what a test uses to mean "the switchboard is not the subject".
    pub fn all_on() -> Switches {
        Switches::default()
    }

    /// Is this id on, given what it would be if nothing had been said?
    ///
    /// OFF WINS OVER ON. They cannot both be set by [`set`], but a
    /// hand-edited file can say both, and refusing to run is the answer that
    /// cannot surprise anyone.
    pub fn is_on(&self, id: &str, default_on: bool) -> bool {
        if self.off.iter().any(|o| o == id) {
            return false;
        }
        if self.on.iter().any(|o| o == id) {
            return true;
        }
        default_on
    }

    /// Is this plugin contributing anything at all? On until switched off — a
    /// plugin is the unit you installed, and installing it is the ask.
    pub fn plugin_on(&self, plugin: &str) -> bool {
        self.is_on(plugin, true)
    }

    /// Is one thing inside a plugin live: its plugin is on AND it is on.
    ///
    /// TWO ANSWERS, AND THE PLUGIN'S OUTRANKS. An item under a disabled plugin
    /// keeps its own switch, because turning the plugin back on must restore
    /// the picture you left rather than a blank one.
    pub fn item_on(&self, plugin: &str, id: &str) -> bool {
        self.plugin_on(plugin) && self.is_on(id, true)
    }
}

pub fn read_at(path: &Path) -> Switches {
    let mut state: Switches = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    fold_legacy(&mut state);
    state
}

/// The switchboard as it is on disk right now.
pub fn read() -> Switches {
    read_at(&path())
}

/// Fold the stores this file replaces into a state that has already been read.
///
/// The NEW file wins every id it mentions: a toggle written since the move is
/// the more recent answer, and a legacy entry re-asserting itself over it
/// would be a switch that flipped back on its own.
fn fold_legacy(state: &mut Switches) {
    let mut adopt = |id: String, on: bool| {
        if state.on.contains(&id) || state.off.contains(&id) {
            return;
        }
        if on {
            state.on.push(id);
        } else {
            state.off.push(id);
        }
    };
    if let Some(old) = std::fs::read_to_string(legacy_hooks_state())
        .ok()
        .and_then(|t| serde_json::from_str::<Switches>(&t).ok())
    {
        for id in old.off {
            adopt(id, false);
        }
        for id in old.on {
            adopt(id, true);
        }
    }
    if let Some(old) = std::fs::read_to_string(legacy_plugins_state())
        .ok()
        .and_then(|t| serde_json::from_str::<Switches>(&t).ok())
    {
        for id in old.off {
            adopt(id, false);
        }
    }
    // The oldest file held BARE names and only ever described local hooks.
    if let Some(names) = std::fs::read_to_string(legacy_disabled())
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
    {
        for name in names {
            adopt(format!("local/{name}"), false);
        }
    }
}

pub fn write(path: &Path, state: &Switches) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(state).unwrap_or_default(),
    )
}

/// Record one id's switch. `Ok(false)` means it was already there and nothing
/// was written — the caller's cue to skip an interpreter rebuild.
pub fn set(id: &str, enabled: bool, default_on: bool) -> std::io::Result<bool> {
    let at = path();
    let mut state = read_at(&at);
    if state.is_on(id, default_on) == enabled {
        return Ok(false);
    }
    state.on.retain(|n| n != id);
    state.off.retain(|n| n != id);
    if enabled {
        state.on.push(id.to_string());
        state.on.sort();
    } else {
        state.off.push(id.to_string());
        state.off.sort();
    }
    write(&at, &state)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_decides_only_what_nothing_has_been_said_about() {
        let state = Switches {
            on: vec!["bundled/adapter.lua".into()],
            off: vec!["local/noisy.lua".into()],
        };
        assert!(state.is_on("bundled/adapter.lua", false), "an explicit on");
        assert!(!state.is_on("local/noisy.lua", true), "an explicit off");
        assert!(
            state.is_on("local/quiet.lua", true),
            "nothing said: default"
        );
        assert!(!state.is_on("someone-repo/guard.lua", false));
    }

    #[test]
    fn off_wins_over_on_in_a_hand_edited_file() {
        let both = Switches {
            on: vec!["acme/guard.lua".into()],
            off: vec!["acme/guard.lua".into()],
        };
        assert!(!both.is_on("acme/guard.lua", true));
    }

    #[test]
    fn a_plugins_switch_outranks_the_switch_on_the_thing_inside_it() {
        let state = Switches {
            on: vec!["acme/skills/review".into()],
            off: vec!["acme".into()],
        };
        assert!(!state.plugin_on("acme"));
        assert!(
            !state.item_on("acme", "acme/skills/review"),
            "a plugin that is off contributes nothing, whatever its items say"
        );
        assert!(
            state.is_on("acme/skills/review", true),
            "and the item keeps its own switch for when the plugin comes back"
        );
    }

    #[test]
    fn every_switch_is_recorded_explicitly_on_either_way() {
        let home = std::env::temp_dir().join(format!("bough-sw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], || {
            // A no-op writes nothing and says so, which is what spares the
            // interpreter a rebuild it does not need.
            assert!(!set("acme/skills/review", true, true).unwrap());
            assert!(set("acme/skills/review", false, true).unwrap());
            assert_eq!(read().off, vec!["acme/skills/review".to_string()]);
            // Back on RECORDS the on rather than forgetting the id: a default
            // that changes its mind later must not re-decide this.
            assert!(set("acme/skills/review", true, true).unwrap());
            assert_eq!(read().on, vec!["acme/skills/review".to_string()]);
            assert!(read().off.is_empty());
        });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_stores_this_replaces_are_folded_in_and_the_new_file_wins() {
        let home = std::env::temp_dir().join(format!("bough-sw-mig-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], || {
            std::fs::write(
                legacy_hooks_state(),
                r#"{"on":["bundled/claude-code.lua"],"off":["local/noisy.lua"]}"#,
            )
            .unwrap();
            std::fs::write(legacy_plugins_state(), r#"{"off":["acme"]}"#).unwrap();
            std::fs::write(legacy_disabled(), r#"["ancient.lua"]"#).unwrap();
            let state = read();
            assert!(state.is_on("bundled/claude-code.lua", false));
            assert!(!state.is_on("local/noisy.lua", true));
            assert!(!state.plugin_on("acme"));
            assert!(!state.is_on("local/ancient.lua", true));
            // Something switched since the move is the more recent answer.
            write(
                &path(),
                &Switches {
                    on: vec!["acme".into()],
                    off: vec![],
                },
            )
            .unwrap();
            assert!(read().plugin_on("acme"), "the new file wins its own ids");
            assert!(!read().is_on("local/noisy.lua", true), "and only those");
        });
        let _ = std::fs::remove_dir_all(&home);
    }
}
