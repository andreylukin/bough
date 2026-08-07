//! `bough hooks` — install, update and inspect hook sources.
//!
//! The panel (`^x`) owns the on/off switches, because that is where you are
//! when you decide; this owns INSTALLATION, because cloning a repository is a
//! thing you do once from a shell and want to see the output of.
//!
//! Every verb prints the commit it landed on. That is the number to compare
//! when asking "did the code I am running change?", and it is the reason a
//! source records a `sha` at all.

use bough_core::hooks::{self, git, SourceKind};

pub const USAGE: &str = "usage: bough hooks [VERB]

  (none)          every hook, where it came from, and whether it is on
  add URL         clone a hook repository — its hooks arrive OFF
  update [NAME]   re-fetch one source, or all of them
  remove NAME     forget a source and delete its clone

  --rev REF       add: branch, tag or commit to check out
  --dir PATH      add: subdirectory holding the .lua files
  -h, --help      this

hooks are turned on and off in the TUI panel (^x)
exit: 0 done · 1 nothing to do · 2 usage";

/// Injected so the tests assert on text instead of a terminal.
pub struct HooksDeps {
    pub out: Box<dyn Fn(&str)>,
    pub err: Box<dyn Fn(&str)>,
}

impl Default for HooksDeps {
    fn default() -> Self {
        HooksDeps {
            out: Box::new(|line| println!("{line}")),
            err: Box::new(|line| eprintln!("{line}")),
        }
    }
}

pub fn run_hooks(argv: &[String], deps: &HooksDeps) -> i32 {
    let mut positional: Vec<&str> = Vec::new();
    let mut rev: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-h" | "--help" => {
                (deps.out)(USAGE);
                return 0;
            }
            "--rev" | "--dir" => {
                let Some(value) = argv.get(i + 1) else {
                    (deps.err)(&format!("{} needs a value\n{USAGE}", argv[i]));
                    return 2;
                };
                if argv[i] == "--rev" {
                    rev = Some(value.clone());
                } else {
                    dir = Some(value.clone());
                }
                i += 2;
            }
            other if other.starts_with('-') => {
                (deps.err)(&format!("unknown option {other}\n{USAGE}"));
                return 2;
            }
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }

    match positional.split_first() {
        None => list(deps),
        Some((&"add", rest)) => {
            let Some(url) = rest.first() else {
                (deps.err)(&format!("add needs a repository URL\n{USAGE}"));
                return 2;
            };
            match git::add(url, rev.as_deref(), dir.as_deref()) {
                Ok(landed) => {
                    (deps.out)(&format!(
                        "cloned {} at {} · {} hook{}, all off",
                        landed.slug,
                        short(&landed.sha),
                        landed.hooks,
                        if landed.hooks == 1 { "" } else { "s" },
                    ));
                    // The next step, named: a clone that runs nothing and does
                    // not say how to change that is a dead end.
                    (deps.out)("turn the ones you want on in the hooks panel (^x)");
                    0
                }
                Err(message) => {
                    (deps.err)(&message);
                    1
                }
            }
        }
        Some((&"update", rest)) => match git::update(rest.first().copied()) {
            Ok(landed) if landed.is_empty() => {
                (deps.out)("no hook sources — `bough hooks add URL` clones one");
                1
            }
            Ok(landed) => {
                for one in &landed {
                    if one.changed() {
                        (deps.out)(&format!(
                            "{}: {} → {}",
                            one.slug,
                            one.was.as_deref().map(short).unwrap_or("(new)"),
                            short(&one.sha),
                        ));
                    } else {
                        (deps.out)(&format!("{}: unchanged at {}", one.slug, short(&one.sha)));
                    }
                }
                0
            }
            Err(message) => {
                (deps.err)(&message);
                1
            }
        },
        Some((&"remove", rest)) => {
            let Some(slug) = rest.first() else {
                (deps.err)(&format!("remove needs a source name\n{USAGE}"));
                return 2;
            };
            match git::remove(slug) {
                Ok(()) => {
                    (deps.out)(&format!("removed {slug}"));
                    0
                }
                Err(message) => {
                    (deps.err)(&message);
                    1
                }
            }
        }
        Some((other, _)) => {
            (deps.err)(&format!("unknown verb {other}\n{USAGE}"));
            2
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Every hook, grouped by source, with the commit each cloned source is on.
fn list(deps: &HooksDeps) -> i32 {
    let rows = hooks::list_hooks();
    if rows.is_empty() {
        (deps.out)("no hooks installed");
        (deps.out)(&format!(
            "write one in {}, or `bough hooks add URL`",
            hooks::hooks_dir().to_string_lossy()
        ));
        return 1;
    }
    let mut current = String::new();
    for row in &rows {
        if row.source != current {
            current = row.source.clone();
            let head = match (&row.repo, &row.sha) {
                (Some(repo), Some(sha)) => format!(
                    "{} · {repo}{} · {}",
                    row.source,
                    row.rev
                        .as_ref()
                        .map(|r| format!(" @{r}"))
                        .unwrap_or_default(),
                    short(sha),
                ),
                _ => match row.kind {
                    SourceKind::Bundled => format!("{} · shipped with bough", row.source),
                    SourceKind::Local => {
                        format!("{} · {}", row.source, hooks::hooks_dir().to_string_lossy())
                    }
                    SourceKind::Git => row.source.clone(),
                },
            };
            (deps.out)(&head);
        }
        let state = if row.enabled { "on " } else { "off" };
        let detail = match (&row.error, row.enabled, row.autocmds) {
            (Some(err), _, _) => format!("  {}", err.lines().next().unwrap_or_default()),
            (None, false, _) => String::new(),
            (None, true, n) => format!("  {n} listener{}", if n == 1 { "" } else { "s" }),
        };
        (deps.out)(&format!("  [{state}] {}{detail}", row.name));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn collector() -> (HooksDeps, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let e = err.clone();
        (
            HooksDeps {
                out: Box::new(move |line| o.lock().unwrap().push(line.to_string())),
                err: Box::new(move |line| e.lock().unwrap().push(line.to_string())),
            },
            out,
            err,
        )
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn help_is_stdout_and_zero_a_bad_verb_is_stderr_and_two() {
        let (deps, out, err) = collector();
        assert_eq!(run_hooks(&argv(&["--help"]), &deps), 0);
        assert!(out.lock().unwrap()[0].starts_with("usage: bough hooks"));
        assert!(err.lock().unwrap().is_empty());

        let (deps, out, err) = collector();
        assert_eq!(run_hooks(&argv(&["frobnicate"]), &deps), 2);
        assert!(out.lock().unwrap().is_empty());
        assert!(err.lock().unwrap()[0].contains("unknown verb frobnicate"));
    }

    #[test]
    fn add_without_a_url_and_options_without_values_are_usage_errors() {
        for args in [vec!["add"], vec!["add", "--rev"], vec!["remove"]] {
            let (deps, _out, err) = collector();
            assert_eq!(run_hooks(&argv(&args), &deps), 2, "{args:?}");
            assert!(!err.lock().unwrap().is_empty(), "{args:?}");
        }
    }

    #[test]
    fn the_usage_names_the_panel_because_this_verb_does_not_toggle_anything() {
        // Installing and switching on are different acts in different places;
        // the CLI that does the first has to name the second or the hooks it
        // installs look broken.
        assert!(USAGE.contains("^x"));
        assert!(USAGE.contains("arrive OFF"));
    }
}
