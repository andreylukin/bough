//! The crate's integration tests, as ONE target (`autotests = false` in Cargo.toml).
//! Every `tests/*.rs` file is a module here — `scripts/check-test-mods.sh` fails the
//! gate when a file is missing. One target means one link instead of one per file; test
//! isolation comes from nextest running every test in its own process (`make test`).

mod about;
mod catchup;
mod reconcile;
