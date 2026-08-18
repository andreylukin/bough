//! `bough config` — everything the harness injects, and the switch on each of
//! it.
//!
//! The panel (`^x`) owns switching in the moment, because that is where you are
//! when you decide. This owns the SHELL answer to the same question, for the
//! reason `bough hooks` exists beside the panel: a plugin you just cloned into
//! `~/.bough/plugins` is inspected from the shell you cloned it in, and a
//! machine with no TUI attached still has to be able to turn something off.
//!
//! Every row prints the id, never the bare name. The id is what the switch
//! takes and what the panel shows, and two sources WILL both ship a
//! `guard.lua`.

use bough_core::config::{self, ConfigGroup};
use bough_core::plugins::Surface;

pub const USAGE: &str = "usage: bough config [VERB]

  (none)          every hook, skill and extension, and whether it is on
  enable ID       turn one source, or one thing inside one, on
  disable ID      turn it off

  -h, --help      this

an ID is a source (bundled, local, project, or a plugin's name) or one
thing inside one — acme/guard.lua, local/skills/mine,
acme/extensions/gh.js. A source that is off contributes nothing,
whatever the things inside it say.

also switched in the TUI panel (^x): enter opens a source,
x switches the row under the cursor
exit: 0 done · 1 nothing to do · 2 usage";

/// Injected so the tests assert on text instead of a terminal.
pub struct ConfigDeps {
    pub out: Box<dyn Fn(&str)>,
    pub err: Box<dyn Fn(&str)>,
}

impl Default for ConfigDeps {
    fn default() -> Self {
        ConfigDeps {
            out: Box::new(|line| println!("{line}")),
            err: Box::new(|line| eprintln!("{line}")),
        }
    }
}

/// The workspace whose project tier is listed: the shell's, because that is
/// the checkout you are asking about.
fn workspace() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

pub fn run_config(argv: &[String], deps: &ConfigDeps) -> i32 {
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
fn switch(id: &str, enabled: bool, deps: &ConfigDeps) -> i32 {
    if !config::known(&workspace(), id) {
        (deps.err)(&format!(
            "nothing named {id} is installed. `bough config` lists what is."
        ));
        return 1;
    }
    match config::set_enabled(id, enabled) {
        Ok(()) => {
            (deps.out)(&format!("{id} is {}", if enabled { "on" } else { "off" }));
            0
        }
        Err(e) => {
            (deps.err)(&format!("could not write the switchboard: {e}"));
            1
        }
    }
}

/// What one source is, in the words that let you decide about it.
fn detail(group: &ConfigGroup) -> String {
    match (&group.repo, &group.sha) {
        (Some(repo), Some(sha)) => format!("{repo} · {}", &sha[..sha.len().min(7)]),
        (Some(repo), None) => repo.clone(),
        _ => group.dirs.first().cloned().unwrap_or_default(),
    }
}

/// Every source, grouped, with what each thing is and whether it is on.
fn list(deps: &ConfigDeps) -> i32 {
    let groups = config::list(&workspace());
    if groups.is_empty() {
        (deps.out)("nothing installed");
        return 1;
    }
    for group in &groups {
        (deps.out)(&format!(
            "{} · {}{}",
            group.id,
            detail(group),
            if group.enabled { "" } else { " · OFF" },
        ));
        if group.items.is_empty() {
            (deps.out)("  (nothing in it)");
            continue;
        }
        for item in &group.items {
            // A disabled source's things keep their own switches, and printing
            // them as "on" while nothing runs would be the lie. Say which of
            // the two answers is the one in force.
            let state = match (group.enabled, item.enabled) {
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
    let hooks = groups
        .iter()
        .flat_map(|g| &g.items)
        .filter(|i| i.surface == Surface::Hook && !i.enabled)
        .count();
    if hooks > 0 {
        // Named, because it is the one default that surprises: a hook that
        // arrived rather than being written runs in-process on the next turn,
        // so it arrives off.
        (deps.out)(&format!(
            "\n{hooks} hook{} off by default — Lua you did not write is opt-in. \
             `bough config enable ID` turns one on.",
            if hooks == 1 { "" } else { "s" }
        ));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn collector() -> (ConfigDeps, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let e = err.clone();
        (
            ConfigDeps {
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
        assert_eq!(run_config(&["--help".into()], &deps), 0);
        assert!(lines(&out)[0].starts_with("usage: bough config"));
        assert_eq!(run_config(&["frobnicate".into()], &deps), 2);
        assert!(lines(&err)[0].contains("unknown verb frobnicate"));
    }

    #[test]
    fn a_verb_without_an_id_is_usage_rather_than_a_guess() {
        let (deps, _out, err) = collector();
        assert_eq!(run_config(&["disable".into()], &deps), 2);
        assert!(lines(&err)[0].contains("disable needs an id"));
    }
}
