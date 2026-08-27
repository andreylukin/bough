//! `cargo xtask events [--check] [--write <path>]` — §15 item 7's event catalog gate.
//!
//! `--check` prints the findings and exits non-zero if there are any; `--write` regenerates the
//! committed catalog. With neither, the table goes to stdout.

fn main() -> anyhow::Result<()> {
    todo!("WP-6: parse argv, scan(ROOTS), check(), table(), --check/--write")
}
