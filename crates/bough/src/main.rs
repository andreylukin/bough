//! Invariant: the launcher composes and tears down, and does nothing else (§0.1 item 2). A
//! behaviour that lives here instead of in a plugin row is a §0.1 violation.
//!
//! SCAFFOLD: this allow exists only while WP-5's bodies are `todo!()`. Delete it when they land.
#![allow(dead_code, unused_variables)]

mod boot;
mod cli;
mod compose;
mod profile;
mod watch;

use clap::Parser;

fn main() -> std::process::ExitCode {
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
