//! The crate's integration tests, as ONE target (`autotests = false` in Cargo.toml).
//! Every `tests/*.rs` file is a module here — `scripts/check-test-mods.sh` fails the
//! gate when a file is missing. One target means one link instead of one per file; test
//! isolation comes from nextest running every test in its own process (`make test`).

mod support;

mod agent_invariants;
mod agent_scripted;
mod bad_patch;
mod boot;
mod dormancy_loops;
mod dump_config;
mod exec_headless;
mod graph_invariants;
mod include;
mod invariants;
mod leader_swap;
mod ledger_invariants;
mod ledger_swap;
mod loop_swap;
mod many_agents;
mod memory_invariants;
mod old_feed_surface;
mod projection_bench;
mod projection_swap;
mod projection_tiers;
mod rollups_swap;
mod swap;
mod token_calibration;
mod tui_boot;
mod tui_config;
mod tui_invariants;
mod tui_swap;
mod watch_broadcast;
mod worker_live;
mod worker_spawn;
