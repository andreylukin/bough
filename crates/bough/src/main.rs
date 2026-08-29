//! Invariant: the launcher composes and tears down, and does nothing else (§0.1 item 2). `main` is
//! argument parsing, one runtime, and one call into `boot`.
//!
//! One thing more, and it is a launcher detail rather than a behaviour switch (P3-D3): when this
//! process is about to OWN A TERMINAL — stdout is a TTY and no subcommand was given, so the `tui`
//! profile is what boots — tracing goes to `~/.bough/bough.log` instead of stderr. A log line
//! written into the alt screen corrupts the display and every shell-use assertion, and the
//! subscriber is installed before anything is composed, so the shell cannot redirect it later.
//! `--check`, `bough exec` and a piped stdout all keep stderr.

use bough::{boot, cli};
use clap::Parser;

fn main() -> std::process::ExitCode {
    let mut cli = cli::Cli::parse();
    // A subcommand implies `--no-watch`: the process exits when its row is done (§0.1 item 2).
    cli.normalize();
    let cli = cli;
    // `$BOUGH_HOME/env` loads BEFORE tracing and before anything composes, so a key written
    // there reaches the compose-time `!!expr env(...)` snapshot, the call-time provider reads,
    // and even RUST_LOG. The process environment wins over the file (envfile.rs).
    let loaded = bough::envfile::load(&bough_util::bough_path("env"));
    let _log = init_tracing(&cli);
    if !loaded.is_empty() {
        tracing::info!(target: "bough", names = %loaded.join(", "), "loaded from $BOUGH_HOME/env");
    }

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bough: could not start the runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(boot::boot(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("bough: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Whether this invocation is about to take the terminal over (P3-D3). PURE in its inputs so the
/// rule is testable without a TTY.
pub fn owns_a_terminal(has_subcommand: bool, check: bool, dump_config: bool, tty: bool) -> bool {
    tty && !has_subcommand && !check && !dump_config
}

/// Install the tracing subscriber. Returns the worker guard when the log went to a file, which the
/// caller holds for the length of the process so buffered lines are flushed.
fn init_tracing(cli: &cli::Cli) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };

    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    if !owns_a_terminal(cli.command.is_some(), cli.check, cli.dump_config, tty) {
        tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_writer(std::io::stderr)
            .init();
        return None;
    }

    let path = bough_util::bough_path("bough.log");
    let Some(dir) = path.parent() else {
        tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_writer(std::io::stderr)
            .init();
        return None;
    };
    // A log file we cannot open is not a reason to refuse to boot: fall back to stderr and say so
    // once, before the alt screen exists.
    if let Err(e) = bough_util::ensure_dir(dir) {
        eprintln!("bough: could not create {}: {e}", dir.display());
        tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_writer(std::io::stderr)
            .init();
        return None;
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("bough: could not open {}: {e}", path.display());
            tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_writer(std::io::stderr)
                .init();
            return None;
        }
    };
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_ansi(false)
        .with_writer(writer)
        .init();
    Some(guard)
}

#[cfg(test)]
mod tests {
    use super::owns_a_terminal;

    #[test]
    fn a_bare_bough_on_a_tty_owns_the_terminal() {
        assert!(owns_a_terminal(false, false, false, true));
    }

    #[test]
    fn a_subcommand_a_check_a_dump_or_a_pipe_keeps_stderr() {
        assert!(!owns_a_terminal(true, false, false, true), "bough exec");
        assert!(!owns_a_terminal(false, true, false, true), "--check");
        assert!(!owns_a_terminal(false, false, true, true), "--dump-config");
        assert!(!owns_a_terminal(false, false, false, false), "piped stdout");
    }
}
