//! Invariant: `bough exec` adds NO behaviour to the launcher (§0.1 item 2). It selects the
//! headless profile and produces ONE synthetic patch layer that writes the `exec` row's config;
//! everything that then happens is the `exec` row's, running in the ordinary tree.
//!
//! The layer is synthesised as a `Patch` rather than as YAML text so a shell-quoted task can
//! never be re-parsed as YAML — a task containing `{`, a tab or a leading `-` is data, not
//! structure.

use std::collections::BTreeMap;

use bough_kernel::{EntryId, LayerId, Patch};

use crate::cli::{Command, ExecArgs};
use crate::compose::{Layer, LayerSource};

/// The row `bough exec` writes. It lives in `bundles/bough-headless.yml`; naming it here is the
/// one place the launcher and that bundle have to agree, and `exec_row_is_in_the_headless_bundle`
/// pins the agreement.
pub const EXEC_ROW: &str = "exec";

/// The profile `bough exec` forces.
pub const EXEC_PROFILE: &str = "headless";

/// The layer id, so a `--dump-config` reader can see where the task came from.
pub const EXEC_LAYER: &str = "exec";

/// The synthetic layer for one `bough exec` invocation.
///
/// Only the fields the invocation actually names are written, on top of the bundle's config —
/// which a whole-config REPLACEMENT could not do (§0.5: there is no deep merge). So this writes a
/// per-field patch through `EntryPatch::config` only when a field is given, and otherwise leaves
/// the bundle's value: the task is always written, the rest is optional.
pub fn exec_patch(args: &ExecArgs, base: &serde_yaml::Value) -> Patch {
    let mut config = base.clone();
    let map = match &mut config {
        serde_yaml::Value::Mapping(m) => m,
        _ => {
            config = serde_yaml::Value::Mapping(Default::default());
            match &mut config {
                serde_yaml::Value::Mapping(m) => m,
                _ => unreachable!(),
            }
        }
    };
    let mut put = |k: &str, v: serde_yaml::Value| {
        map.insert(serde_yaml::Value::String(k.to_string()), v);
    };
    put("task", serde_yaml::Value::String(args.task.clone()));
    if let Some(a) = &args.agent {
        put("agent", serde_yaml::Value::String(a.clone()));
    }
    if let Some(t) = &args.traj {
        put("traj", serde_yaml::Value::String(t.clone()));
    }
    if let Some(p) = args.print {
        put("print", serde_yaml::Value::String(p.as_str().to_string()));
    }
    put(
        "exit_when_idle",
        serde_yaml::Value::Bool(!args.keep_running),
    );

    let mut entries = BTreeMap::new();
    entries.insert(
        EntryId::new(EXEC_ROW),
        bough_kernel::EntryPatch {
            config: Some(config),
            ..Default::default()
        },
    );
    Patch {
        entries,
        insert: Vec::new(),
        remove: Vec::new(),
    }
}

/// The layer, ready to append last.
pub fn exec_layer(args: &ExecArgs, base: &serde_yaml::Value) -> Layer {
    Layer {
        id: LayerId::new(EXEC_LAYER),
        base: bough_util::bough_home(),
        source: LayerSource::Synthetic(exec_patch(args, base)),
    }
}

/// `bough exec` forces `--profile headless`, whatever `--profile` said.
///
/// It is not a silent override: a `--profile` that disagrees is reported, because a flag that
/// looks obeyed and is not is exactly the misconfiguration §0.2 refuses to swallow.
pub fn force_profile(cli: &mut crate::cli::Cli) {
    if !matches!(cli.command, Some(Command::Exec(_))) {
        return;
    }
    // `--profile`'s clap default is `tui`, which is indistinguishable from an explicit `--profile
    // tui`; the default is not a choice, so only a profile the user could only have typed is
    // worth a word.
    if cli.profile != EXEC_PROFILE && cli.profile != crate::cli::DEFAULT_PROFILE {
        eprintln!(
            "bough: `exec` runs under the `{EXEC_PROFILE}` profile; ignoring --profile {}",
            cli.profile
        );
    }
    cli.profile = EXEC_PROFILE.to_string();
    // The task ends when the agent goes idle; a patch watch would only outlive it.
    cli.no_watch = true;
    // A headless run has no terminal to hand back, and a teardown cut short by the interactive
    // budget leaves `ledger.db-wal` next to the chain it was still writing. Only the untouched
    // default is raised: a `--shutdown-ms` the user typed is theirs.
    if cli.shutdown_ms == crate::cli::DEFAULT_SHUTDOWN_MS {
        cli.shutdown_ms = crate::cli::HEADLESS_SHUTDOWN_MS;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PrintFormat;

    fn args(task: &str) -> ExecArgs {
        ExecArgs {
            task: task.to_string(),
            agent: None,
            traj: None,
            print: None,
            keep_running: false,
        }
    }

    fn base() -> serde_yaml::Value {
        serde_yaml::from_str(
            "task: \"\"\nagent: sol\ntraj: lane/sol\nprint: text\nexit_when_idle: true\n",
        )
        .unwrap()
    }

    fn config_of(p: &Patch) -> serde_yaml::Value {
        p.entries
            .get(&EntryId::new(EXEC_ROW))
            .unwrap()
            .config
            .clone()
            .unwrap()
    }

    #[test]
    fn the_task_is_written_as_data_never_reparsed_as_yaml() {
        let hostile = "- fix: {a: b}\n\tand: [c]";
        let cfg = config_of(&exec_patch(&args(hostile), &base()));
        assert_eq!(cfg["task"].as_str().unwrap(), hostile);
    }

    #[test]
    fn unnamed_fields_keep_the_bundles_values() {
        let cfg = config_of(&exec_patch(&args("hi"), &base()));
        assert_eq!(cfg["agent"].as_str().unwrap(), "sol");
        assert_eq!(cfg["traj"].as_str().unwrap(), "lane/sol");
        assert_eq!(cfg["print"].as_str().unwrap(), "text");
    }

    #[test]
    fn named_fields_overwrite_them() {
        let mut a = args("hi");
        a.agent = Some("terra".into());
        a.traj = Some("lane/terra".into());
        a.print = Some(PrintFormat::Json);
        let cfg = config_of(&exec_patch(&a, &base()));
        assert_eq!(cfg["agent"].as_str().unwrap(), "terra");
        assert_eq!(cfg["traj"].as_str().unwrap(), "lane/terra");
        assert_eq!(cfg["print"].as_str().unwrap(), "json");
    }

    #[test]
    fn keep_running_is_the_negation_of_exit_when_idle() {
        let mut a = args("hi");
        assert_eq!(
            config_of(&exec_patch(&a, &base()))["exit_when_idle"],
            serde_yaml::Value::Bool(true)
        );
        a.keep_running = true;
        assert_eq!(
            config_of(&exec_patch(&a, &base()))["exit_when_idle"],
            serde_yaml::Value::Bool(false)
        );
    }

    #[test]
    fn the_layer_patches_only_the_exec_row() {
        let p = exec_patch(&args("hi"), &base());
        assert_eq!(p.insert.len(), 0);
        assert_eq!(p.remove.len(), 0);
        assert_eq!(p.entries.len(), 1);
        assert!(p.entries.contains_key(&EntryId::new(EXEC_ROW)));
    }

    /// The headless budget replaces the interactive one, and only when it was the default.
    #[test]
    fn exec_loosens_only_the_default_teardown_budget() {
        let mut cli = crate::cli::Cli {
            profile: "tui".into(),
            patches: Vec::new(),
            dump_config: false,
            dump_format: crate::cli::DumpFormat::Yaml,
            check: false,
            no_watch: false,
            local: false,
            resident: false,
            shutdown_ms: crate::cli::DEFAULT_SHUTDOWN_MS,
            root: None,
            command: Some(Command::Exec(args("hi"))),
        };
        force_profile(&mut cli);
        assert_eq!(cli.shutdown_ms, crate::cli::HEADLESS_SHUTDOWN_MS);

        let mut typed = crate::cli::Cli {
            profile: "tui".into(),
            patches: Vec::new(),
            dump_config: false,
            dump_format: crate::cli::DumpFormat::Yaml,
            check: false,
            no_watch: false,
            local: false,
            resident: false,
            shutdown_ms: 750,
            root: None,
            command: Some(Command::Exec(args("hi"))),
        };
        force_profile(&mut typed);
        assert_eq!(
            typed.shutdown_ms, 750,
            "a typed --shutdown-ms is the user's"
        );
    }

    #[test]
    fn exec_forces_the_headless_profile_and_stops_the_watch() {
        let mut cli = crate::cli::Cli {
            profile: "tui".into(),
            patches: Vec::new(),
            dump_config: false,
            dump_format: crate::cli::DumpFormat::Yaml,
            check: false,
            no_watch: false,
            local: false,
            resident: false,
            shutdown_ms: 2000,
            root: None,
            command: Some(Command::Exec(args("hi"))),
        };
        force_profile(&mut cli);
        assert_eq!(cli.profile, EXEC_PROFILE);
        assert!(cli.no_watch);
    }

    #[test]
    fn without_the_subcommand_the_profile_is_left_alone() {
        let mut cli = crate::cli::Cli {
            profile: "tui".into(),
            patches: Vec::new(),
            dump_config: false,
            dump_format: crate::cli::DumpFormat::Yaml,
            check: false,
            no_watch: false,
            local: false,
            resident: false,
            shutdown_ms: 2000,
            root: None,
            command: None,
        };
        force_profile(&mut cli);
        assert_eq!(cli.profile, "tui");
        assert!(!cli.no_watch);
    }
}
