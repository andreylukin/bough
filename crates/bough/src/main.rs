//! Invariant: the launcher composes and tears down, and does nothing else (§0.1 item 2). `main` is
//! argument parsing, one runtime, and one call into `boot`.

use bough::{boot, cli};
use clap::Parser;

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = cli::Cli::parse();
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
