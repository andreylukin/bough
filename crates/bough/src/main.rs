//! `bough` — subcommand dispatch (port of `scripts/bough`'s command surface,
//! reduced to what the Rust binary owns per ARCHITECTURE §1). Bare `bough` →
//! TUI; `start` → server; `exec` is ported (see `exec.rs`); `mcp` (rows 3.6),
//! `sync-mcp` (3.6), `tags` (3.18) and `patterns` (3.19) are ported in their own
//! modules. No clap — the grammar is tiny and USAGE text is product surface,
//! ported verbatim.
//!
//! `BOUGH_PORT` stays an env var, never a flag (see `TUI_USAGE`): the API
//! client is bound before a flag could be read, and a `--port` that parsed
//! and did nothing would be the bug `args.ts` fixed.
//!
//! WAVE-1 NOTE: the TUI flag parser below is `src/tui/args.ts::parseTuiArgs`
//! verbatim; its spec home is `bough-tui::args` (row 1.32, in flight) — it
//! lives here only until that port lands, because a silently-ignored `-w` is
//! the worst failure this flag can have and the dispatch must not ship it.

// Each subcommand takes its process edges (env reads, path resolution, the
// embedding layer) as `Arc<dyn Fn(..)>` fields on a `Deps` struct so the tests
// can drive them without a real process — the same injection seam bough-core
// uses, and the same reason not to hide it behind an alias.
#![allow(clippy::type_complexity)]

mod acp;
mod config;
mod exec;
mod hooks;
mod mcp;
mod notes;
mod patterns;
mod sync_mcp;
mod tags;

use std::process::ExitCode;

use bough_tui::app::TuiOptions;

/// src/tui/args.ts::USAGE, verbatim (a multi-line literal — `\n\`
/// continuations would strip the indentation, which is part of the text).
const TUI_USAGE: &str = "usage: bough [-w DIR] [-r]

  -w, --workspace DIR   where new conversations start (default: the cwd)
  -r, --resume          reopen this workspace\'s last conversation
  -s, --session ID      open one specific conversation (from a board, a link…)
  -h, --help            this message

  the server port comes from BOUGH_PORT (default 4321). It is an env var and
  not a flag because the API client is bound at import, before a flag could be
  read — a --port that parsed and did nothing would be the bug this file fixes.

programs run as you, with your authority — there is no sandbox.";

enum TuiArgs {
    Args {
        workspace: Option<String>,
        /// `--resume` / `-r`: reopen this workspace's last conversation.
        resume: bool,
        /// `--session ID` / `-s`: open that conversation.
        session: Option<String>,
    },
    Help,
    UsageError(String),
}

/// src/tui/args.ts::parseTuiArgs — an unknown flag is an error; a positional
/// argument is refused with a pointer at `bough exec`.
fn parse_tui_args(argv: &[String]) -> TuiArgs {
    let mut workspace: Option<String> = None;
    let mut resume = false;
    let mut session: Option<String> = None;
    let mut i = 0usize;
    while i < argv.len() {
        let token = &argv[i];
        if token == "--help" || token == "-h" {
            return TuiArgs::Help;
        }
        let (name, inline): (String, Option<String>) = if let Some(body) = token.strip_prefix("--")
        {
            match body.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (body.to_string(), None),
            }
        } else if token.starts_with('-') && token.len() > 1 {
            let body = &token[1..];
            let (short, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            match short {
                "w" => ("workspace".to_string(), inline),
                "r" => ("resume".to_string(), inline),
                "s" => ("session".to_string(), inline),
                _ => {
                    return TuiArgs::UsageError(format!("unknown flag -{short}\n{TUI_USAGE}"));
                }
            }
        } else {
            // The TUI takes no positional argument — it is not `bough exec`,
            // and a stray prompt here would otherwise vanish into a screen
            // that ignores it.
            return TuiArgs::UsageError(format!(
                "bough takes no positional argument (got \"{token}\").\nDid you mean: bough exec \"{token}\"?\n{TUI_USAGE}"
            ));
        };
        // A boolean flag takes no value; everything else does.
        if name == "resume" {
            resume = true;
            i += 1;
            continue;
        }
        if name != "workspace" && name != "session" {
            return TuiArgs::UsageError(format!("unknown flag --{name}\n{TUI_USAGE}"));
        }
        let value = match inline {
            Some(v) => v,
            None => {
                if i + 1 >= argv.len() {
                    return TuiArgs::UsageError(format!("--{name} needs a value\n{TUI_USAGE}"));
                }
                i += 1;
                argv[i].clone()
            }
        };
        if name == "session" { session = Some(value); } else { workspace = Some(value); }
        i += 1;
    }
    if let Some(ws) = &workspace {
        if ws.trim().is_empty() {
            return TuiArgs::UsageError(format!("--workspace needs a path\n{TUI_USAGE}"));
        }
    }
    TuiArgs::Args { workspace, resume, session }
}

/// Stop the running server and start a fresh one.
///
/// The point is not the process — it is that boot RECOVERS: every turn left
/// `running` by the old process is closed as orphaned before anything can
/// talk to it, so a restart is also how a session wedged mid-turn becomes
/// usable again. That recovery already existed; this is the verb that reaches
/// it without a manual kill.
async fn restart_server() -> Result<String, String> {
    let base = bough_core::hostfn::artifact::server_base_url();
    let was = bough_server::boot::stop_running_server().await;
    // Started DETACHED, because this command returns and the server must not.
    let exe = std::env::current_exe().map_err(|e| format!("bough restart: {e}"))?;
    std::process::Command::new(exe)
        .arg("start")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("bough restart: could not start the server: {e}"))?;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if reqwest::get(format!("{base}/sessions")).await.is_ok() {
            return Ok(match was {
                true => format!("restarted · {base}"),
                false => format!("started · {base} (nothing was running)"),
            });
        }
    }
    Err(format!(
        "bough restart: the new server did not answer at {base}"
    ))
}

/// What `--version` prints: `bough 0.1.0 (abc123def)`, or `bough 0.1.0` where
/// the build had no git to ask (a tarball, a vendored build).
///
/// The sha is what makes a bug report actionable — bough installs by building
/// `main`, so the crate version alone is the same string on every install ever
/// made. `build.rs` stamps it, including the `-dirty` marker.
fn version_line() -> String {
    match option_env!("BOUGH_BUILD_REV") {
        Some(rev) => format!("bough {} ({rev})", env!("CARGO_PKG_VERSION")),
        None => format!("bough {}", env!("CARGO_PKG_VERSION")),
    }
}

fn usage() -> &'static str {
    // The launchd/systemd manager verbs (setup/kill/restart/update/status/
    // logs/run/purge) stay in the bash wrapper; this binary owns the rest.
    "usage: bough [tui|start|restart|exec|acp|hooks|config|mcp|sync-mcp|tags|notes|patterns]
  (no args) open the terminal UI (bough [-w DIR] [-r], -h for flags)
  --version print the version and exit (-V)
  start    run the server in the foreground
  restart  stop the running server and start a fresh one
  exec     headless one-shot turn
  acp      speak the Agent Client Protocol on stdio
  hooks    install and inspect hook sources
  config   every hook, skill and extension — and the switch on each
  mcp      inspect and repair the MCP registry
  sync-mcp adopt Claude Code's MCP servers
  tags     what the command memory knows
  notes    what it MEANT — prose keyed on the same tags
  patterns compress a log into its distinct statements"
}

fn run_tui(argv: &[String]) -> ExitCode {
    match parse_tui_args(argv) {
        TuiArgs::Help => {
            // Help is stdout + exit 0, distinct from a usage error (stderr + 2).
            println!("{TUI_USAGE}");
            ExitCode::SUCCESS
        }
        TuiArgs::UsageError(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        TuiArgs::Args { workspace, resume, session } => {
            let workspace = workspace.or_else(|| {
                std::env::var("BOUGH_TUI_CWD")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .map(|p| p.to_string_lossy().into_owned())
                    })
            });
            // NOT A TERMINAL: refuse in one sentence rather than panicking.
            // ratatui's init() returns Err on a pipe ("Device not configured"),
            // and unwrapping it printed a vendored crates.io path plus a
            // RUST_BACKTRACE hint — the only subcommand in this binary that
            // could panic on ordinary misuse, and the least actionable output
            // it produces. The TS TUI renders into the pipe instead, but escape
            // soup down a redirect is not worth reproducing; what both must
            // share is not crashing. Exit 2 is the same code the unreachable-
            // server preflight uses: "this cannot run here", not "it failed".
            if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                eprintln!(
                    "bough tui: not a terminal — the TUI needs one.\n\
                     Run it directly, or use `bough exec \"…\"` for a headless turn."
                );
                return ExitCode::from(2);
            }
            let workspace_for_restart = workspace.clone();
            match bough_tui::run(TuiOptions { workspace, resume, session }) {
                // `/restart`: the terminal is already restored, so this is the
                // one place that can cleanly stop the server and hand the
                // process over. EXEC, not spawn — the shell that launched
                // bough should be waiting on the new one, not on a parent that
                // is only waiting on a child.
                Ok(true) => {
                    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                    if let Err(message) = rt.block_on(restart_server()) {
                        eprintln!("{message}");
                        return ExitCode::from(2);
                    }
                    let exe = std::env::current_exe().expect("current exe");
                    let mut cmd = std::process::Command::new(exe);
                    cmd.arg("--resume");
                    if let Some(ws) = workspace_for_restart {
                        cmd.arg("-w").arg(ws);
                    }
                    let err = std::os::unix::process::CommandExt::exec(&mut cmd);
                    eprintln!("bough: could not restart: {err}");
                    ExitCode::from(2)
                }
                Ok(false) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("{err}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // Bare `bough` (or flags only) opens the terminal UI.
        None => run_tui(&[]),
        // Before the flags-open-the-TUI arm: every bug report starts with a
        // version, and `--version` reaching the TUI parser answered with a
        // usage error.
        Some("--version" | "-V" | "version") => {
            println!("{}", version_line());
            ExitCode::SUCCESS
        }
        Some(first) if first.starts_with('-') => run_tui(&args),
        Some("tui") => run_tui(&args[1..]),
        Some("start") => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            match rt.block_on(bough_server::boot::start()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("{err}");
                    ExitCode::from(2)
                }
            }
        }
        Some("restart") => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            match rt.block_on(restart_server()) {
                Ok(message) => {
                    println!("{message}");
                    ExitCode::SUCCESS
                }
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::from(2)
                }
            }
        }
        Some("help") => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Some("exec") => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let deps = exec::real_deps();
            let code = rt.block_on(exec::run_exec(&args[1..], &deps));
            ExitCode::from(code as u8)
        }
        Some("acp") => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let deps = acp::real_deps();
            let code = rt.block_on(acp::run_acp(&args[1..], &deps));
            ExitCode::from(code as u8)
        }
        Some("hooks") => {
            let code = hooks::run_hooks(&args[1..], &hooks::HooksDeps::default());
            ExitCode::from(code as u8)
        }
        // `plugins` is the retired name, still landing here: the listing
        // absorbed it along with the panel tab, and a command in somebody's
        // shell history must not stop working over a merge.
        Some("config") | Some("plugins") => {
            let code = config::run_config(&args[1..], &config::ConfigDeps::default());
            ExitCode::from(code as u8)
        }
        Some("mcp") => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let deps = mcp::real_deps();
            let code = rt.block_on(mcp::run_mcp(&args[1..], &deps));
            ExitCode::from(code as u8)
        }
        Some("sync-mcp") => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let deps = sync_mcp::SyncDeps::default();
            let code = rt.block_on(sync_mcp::run_sync_mcp(&args[1..], &deps));
            ExitCode::from(code as u8)
        }
        Some("notes") => {
            let deps = notes::NotesDeps::real();
            ExitCode::from(notes::run_notes(&args[1..], &deps) as u8)
        }
        Some("tags") => {
            // FIRST, before anything opens a Database. `enable_sqlite_extensions`
            // is a one-shot swap that must happen ahead of the first open — it
            // was missing in the TS once, and that made `bough tags similar`
            // structurally dead on every machine: writes worked, reads could not.
            bough_core::db::extensions::enable_sqlite_extensions();
            let deps = tags::TagsDeps::real();
            ExitCode::from(tags::run_tags(&args[1..], &deps) as u8)
        }
        Some("patterns") => {
            // Fully synchronous: the pipeline has no tokio anywhere and the
            // reader is a plain `BufReader`, so no runtime is started.
            let code = patterns::run_patterns(&args[1..], &patterns::RealDeps);
            ExitCode::from(code as u8)
        }
        Some(other) => {
            eprintln!("error: unknown command '{other}'\n{}", usage());
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    // args.test.ts: "-w and --workspace both name where a new conversation starts"
    #[test]
    fn w_and_workspace_both_name_the_workspace() {
        for cli in [
            argv(&["-w", "/tmp/x"]),
            argv(&["--workspace", "/tmp/x"]),
            argv(&["--workspace=/tmp/x"]),
            argv(&["-w=/tmp/x"]),
        ] {
            match parse_tui_args(&cli) {
                TuiArgs::Args { workspace, .. } => assert_eq!(workspace.as_deref(), Some("/tmp/x")),
                _ => panic!("expected args for {cli:?}"),
            }
        }
    }

    // args.test.ts: "an unknown flag stops, rather than starting anyway"
    #[test]
    fn unknown_flag_stops_rather_than_starting_anyway() {
        match parse_tui_args(&argv(&["--port", "9999"])) {
            TuiArgs::UsageError(msg) => {
                assert!(msg.contains("unknown flag --port"), "{msg}");
                assert!(msg.contains("usage: bough"), "{msg}");
            }
            _ => panic!("expected usage error"),
        }
        match parse_tui_args(&argv(&["-x"])) {
            TuiArgs::UsageError(msg) => assert!(msg.contains("unknown flag -x"), "{msg}"),
            _ => panic!("expected usage error"),
        }
    }

    // args.test.ts: "a positional argument is refused, and points at bough exec"
    #[test]
    fn positional_is_refused_and_points_at_bough_exec() {
        match parse_tui_args(&argv(&["fix the tests"])) {
            TuiArgs::UsageError(msg) => {
                assert!(
                    msg.contains("bough takes no positional argument (got \"fix the tests\")"),
                    "{msg}"
                );
                assert!(
                    msg.contains("Did you mean: bough exec \"fix the tests\"?"),
                    "{msg}"
                );
            }
            _ => panic!("expected usage error"),
        }
    }

    // args.test.ts: "--help is answered, and the usage states the posture"
    #[test]
    fn help_is_answered_and_the_usage_states_the_posture() {
        assert!(matches!(parse_tui_args(&argv(&["--help"])), TuiArgs::Help));
        assert!(matches!(parse_tui_args(&argv(&["-h"])), TuiArgs::Help));
        assert!(
            TUI_USAGE.contains("programs run as you, with your authority — there is no sandbox")
        );
        assert!(TUI_USAGE.contains("BOUGH_PORT (default 4321)"));
    }

    /// `--version` is dispatched in `main` ahead of the arm that hands every
    /// leading-dash argument to the TUI; the flag parser has never known it,
    /// which is exactly how it used to answer a version request with a usage
    /// error. The parser staying ignorant is the point — this pins the reason.
    #[test]
    fn version_is_not_a_tui_flag_and_the_usage_says_so() {
        assert!(matches!(
            parse_tui_args(&argv(&["--version"])),
            TuiArgs::UsageError(_)
        ));
        assert!(usage().contains("--version"));
    }

    /// The version line always leads with the crate version, and carries the
    /// build's sha in parentheses when `build.rs` had a git checkout to ask.
    /// Both shapes are legal — a tarball build has no sha — so this pins the
    /// invariant that holds either way rather than asserting on the sha.
    #[test]
    fn the_version_line_names_the_crate_version_and_the_build() {
        let line = version_line();
        assert!(
            line.starts_with(&format!("bough {}", env!("CARGO_PKG_VERSION"))),
            "{line}"
        );
        if let Some(rev) = option_env!("BOUGH_BUILD_REV") {
            assert_eq!(line, format!("bough {} ({rev})", env!("CARGO_PKG_VERSION")));
            assert!(!rev.is_empty(), "an empty rev must not be stamped");
        }
    }

    #[test]
    fn empty_workspace_value_is_a_usage_error() {
        match parse_tui_args(&argv(&["--workspace", "  "])) {
            TuiArgs::UsageError(msg) => assert!(msg.contains("--workspace needs a path"), "{msg}"),
            _ => panic!("expected usage error"),
        }
        match parse_tui_args(&argv(&["-w"])) {
            TuiArgs::UsageError(msg) => assert!(msg.contains("--workspace needs a value"), "{msg}"),
            _ => panic!("expected usage error"),
        }
    }
}
