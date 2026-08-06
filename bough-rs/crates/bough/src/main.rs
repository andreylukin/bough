//! `bough` — subcommand dispatch (port of `scripts/bough`'s command surface,
//! reduced to what the Rust binary owns per ARCHITECTURE §1). Bare `bough` →
//! TUI; `start` → server; `exec`/`mcp`/`tags`/`sync-mcp` port later;
//! `patterns` is a stub (exit 2). No clap — the grammar is tiny and USAGE
//! text is product surface, ported verbatim.
//!
//! `BOUGH_PORT` stays an env var, never a flag (see `TUI_USAGE`): the API
//! client is bound before a flag could be read, and a `--port` that parsed
//! and did nothing would be the bug `args.ts` fixed.
//!
//! WAVE-1 NOTE: the TUI flag parser below is `src/tui/args.ts::parseTuiArgs`
//! verbatim; its spec home is `bough-tui::args` (row 1.32, in flight) — it
//! lives here only until that port lands, because a silently-ignored `-w` is
//! the worst failure this flag can have and the dispatch must not ship it.

use std::process::ExitCode;

use bough_tui::app::TuiOptions;

/// src/tui/args.ts::USAGE, verbatim (a multi-line literal — `\n\`
/// continuations would strip the indentation, which is part of the text).
const TUI_USAGE: &str = "usage: bough [-w DIR]

  -w, --workspace DIR   where new conversations start (default: the cwd)
  -h, --help            this message

  the server port comes from BOUGH_PORT (default 4321). It is an env var and
  not a flag because the API client is bound at import, before a flag could be
  read — a --port that parsed and did nothing would be the bug this file fixes.

programs run as you, with your authority — there is no sandbox.";

enum TuiArgs {
    Args { workspace: Option<String> },
    Help,
    UsageError(String),
}

/// src/tui/args.ts::parseTuiArgs — an unknown flag is an error; a positional
/// argument is refused with a pointer at `bough exec`.
fn parse_tui_args(argv: &[String]) -> TuiArgs {
    let mut workspace: Option<String> = None;
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
        if name != "workspace" {
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
        workspace = Some(value);
        i += 1;
    }
    if let Some(ws) = &workspace {
        if ws.trim().is_empty() {
            return TuiArgs::UsageError(format!("--workspace needs a path\n{TUI_USAGE}"));
        }
    }
    TuiArgs::Args { workspace }
}

fn usage() -> &'static str {
    // The launchd/systemd manager verbs (setup/kill/restart/update/status/
    // logs/run/purge) stay in the bash wrapper; this binary owns the rest.
    "usage: bough [tui|start|exec|mcp|sync-mcp|tags|patterns]
  (no args) open the terminal UI (bough [-w DIR], -h for flags)
  start    run the server in the foreground
  exec     headless one-shot turn (not yet ported)
  mcp      inspect and repair the MCP registry (not yet ported)
  sync-mcp adopt Claude Code's MCP servers (not yet ported)
  tags     what the command memory knows (not yet ported)
  patterns compress a log into its distinct statements (not yet ported)"
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
        TuiArgs::Args { workspace } => {
            let workspace = workspace.or_else(|| {
                std::env::var("BOUGH_TUI_CWD").ok().filter(|s| !s.is_empty()).or_else(|| {
                    std::env::current_dir()
                        .ok()
                        .map(|p| p.to_string_lossy().into_owned())
                })
            });
            match bough_tui::run(TuiOptions { workspace }) {
                Ok(()) => ExitCode::SUCCESS,
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
        Some("help") => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Some(cmd @ ("exec" | "mcp" | "sync-mcp" | "tags" | "patterns")) => {
            eprintln!("bough {cmd}: not yet ported");
            ExitCode::from(2)
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
                TuiArgs::Args { workspace } => assert_eq!(workspace.as_deref(), Some("/tmp/x")),
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
                assert!(msg.contains("Did you mean: bough exec \"fix the tests\"?"), "{msg}");
            }
            _ => panic!("expected usage error"),
        }
    }

    // args.test.ts: "--help is answered, and the usage states the posture"
    #[test]
    fn help_is_answered_and_the_usage_states_the_posture() {
        assert!(matches!(parse_tui_args(&argv(&["--help"])), TuiArgs::Help));
        assert!(matches!(parse_tui_args(&argv(&["-h"])), TuiArgs::Help));
        assert!(TUI_USAGE.contains("programs run as you, with your authority — there is no sandbox"));
        assert!(TUI_USAGE.contains("BOUGH_PORT (default 4321)"));
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
