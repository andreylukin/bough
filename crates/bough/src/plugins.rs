//! `bough plugins` — what each installed plugin ships, and the switch on every
//! piece of it.
//!
//! The panel owns switching in the moment, because that is where you are when
//! you decide. This owns the SHELL answer to the same question, for the same
//! reason `bough hooks` exists beside the hooks tab: a plugin you just cloned
//! into `~/.bough/plugins` is inspected from the shell you cloned it in, and a
//! machine with no TUI attached still has to be able to turn something off.
//!
//! Every row prints the id, never the bare name. The id is what the switch
//! takes and what the panel shows, and two plugins WILL both ship a
//! `guard.lua`.

use bough_core::plugins::{self, Surface};

pub const USAGE: &str = "usage: bough plugins [VERB]

  (none)          every plugin, everything in it, and whether it is on
  enable ID       turn one plugin, or one thing inside one, on
  disable ID      turn it off

  -h, --help      this

an ID is a plugin (acme) or one of its items — acme/guard.lua,
acme/skills/review, acme/extensions/gh.js. A plugin that is off
contributes nothing, whatever its items say.

plugins are also switched in the TUI panel (meta+p)
exit: 0 done · 1 nothing to do · 2 usage";

/// Injected so the tests assert on text instead of a terminal.
pub struct PluginsDeps {
    pub out: Box<dyn Fn(&str)>,
    pub err: Box<dyn Fn(&str)>,
}

impl Default for PluginsDeps {
    fn default() -> Self {
        PluginsDeps {
            out: Box::new(|line| println!("{line}")),
            err: Box::new(|line| eprintln!("{line}")),
        }
    }
}

pub fn run_plugins(argv: &[String], deps: &PluginsDeps) -> i32 {
    let mut positional: Vec<&str> = Vec::new();
    for arg in argv {
        match arg.as_str() {
            "-h" | "--help" => {
                (deps.out)(USAGE);
                return 0;
            }
            other if other.starts_with('-') => {
                (deps.err)(&format!("unknown option {other}\n{USAGE}"));
                return 2;
            }
            other => positional.push(other),
        }
    }

    match positional.split_first() {
        None => list(deps),
        Some((verb @ (&"enable" | &"disable"), rest)) => {
            let Some(id) = rest.first() else {
                (deps.err)(&format!("{verb} needs an id\n{USAGE}"));
                return 2;
            };
            switch(id, *verb == "enable", deps)
        }
        Some((other, _)) => {
            (deps.err)(&format!("unknown verb {other}\n{USAGE}"));
            2
        }
    }
}

/// Refused unless the id names something installed, for the reason the route
/// gives: a typo written into the state file is inert, but it reads on screen
/// as a switch that was set.
fn switch(id: &str, enabled: bool, deps: &PluginsDeps) -> i32 {
    let installed = plugins::list();
    let known = installed
        .iter()
        .any(|p| p.name == id || p.items.iter().any(|i| i.id == id));
    if !known {
        (deps.err)(&format!(
            "no plugin or item {id} in {}. `bough plugins` lists what is installed.",
            bough_core::paths::plugins_dir().to_string_lossy()
        ));
        return 1;
    }
    match plugins::set_enabled(id, enabled) {
        Ok(()) => {
            (deps.out)(&format!("{id} is {}", if enabled { "on" } else { "off" }));
            0
        }
        Err(e) => {
            (deps.err)(&format!("could not write the plugin state: {e}"));
            1
        }
    }
}

/// Every plugin, grouped, with what each item is and whether it is on.
fn list(deps: &PluginsDeps) -> i32 {
    let installed = plugins::list();
    if installed.is_empty() {
        (deps.out)("no plugins installed");
        (deps.out)(&format!(
            "a plugin is one directory in {} holding hooks/, skills/ and extensions/",
            bough_core::paths::plugins_dir().to_string_lossy()
        ));
        return 1;
    }
    for plugin in &installed {
        (deps.out)(&format!(
            "{} · {}{}",
            plugin.name,
            plugin.dir,
            if plugin.enabled { "" } else { " · OFF" },
        ));
        if plugin.items.is_empty() {
            (deps.out)("  (nothing in it)");
            continue;
        }
        for item in &plugin.items {
            // A disabled plugin's items keep their own switches, and printing
            // them as "on" while nothing runs would be the lie. Say which of
            // the two answers is the one in force.
            let state = match (plugin.enabled, item.enabled) {
                (false, _) => "—  ",
                (true, true) => "on ",
                (true, false) => "off",
            };
            (deps.out)(&format!(
                "  [{state}] {:<9} {}",
                item.surface.name(),
                item.id
            ));
        }
    }
    let hooks = installed
        .iter()
        .flat_map(|p| &p.items)
        .filter(|i| i.surface == Surface::Hook && !i.enabled)
        .count();
    if hooks > 0 {
        // Named, because it is the one default that surprises: a plugin's hook
        // runs in-process on the next turn, so it arrives off.
        (deps.out)(&format!(
            "\n{hooks} hook{} off by default — a plugin's Lua is opt-in. \
             `bough plugins enable ID` turns one on.",
            if hooks == 1 { "" } else { "s" }
        ));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn collector() -> (
        PluginsDeps,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let e = err.clone();
        (
            PluginsDeps {
                out: Box::new(move |line| o.lock().unwrap().push(line.to_string())),
                err: Box::new(move |line| e.lock().unwrap().push(line.to_string())),
            },
            out,
            err,
        )
    }

    fn lines(buf: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        buf.lock().unwrap().clone()
    }

    #[test]
    fn help_is_stdout_and_success_and_an_unknown_verb_is_usage() {
        let (deps, out, err) = collector();
        assert_eq!(run_plugins(&["--help".into()], &deps), 0);
        assert!(lines(&out)[0].starts_with("usage: bough plugins"));
        assert_eq!(run_plugins(&["frobnicate".into()], &deps), 2);
        assert!(lines(&err)[0].contains("unknown verb frobnicate"));
    }

    #[test]
    fn a_verb_without_an_id_is_usage_rather_than_a_guess() {
        let (deps, _out, err) = collector();
        assert_eq!(run_plugins(&["disable".into()], &deps), 2);
        assert!(lines(&err)[0].contains("disable needs an id"));
    }
}
