//! The crate's integration tests, as ONE target (`autotests = false` in Cargo.toml).
//! Every `tests/*.rs` file is a module here — `scripts/check-test-mods.sh` fails the
//! gate when a file is missing. One target means one link instead of one per file; test
//! isolation comes from nextest running every test in its own process (`make test`).

mod support;

mod actions_boundary_rows;
mod agent_invariants;
mod agent_scripted;
mod bad_patch;
mod boot;
mod boundary_injection;
mod boundary_probe_live;
mod codemode_act;
mod codemode_closed;
mod codemode_conceal_race;
mod codemode_invariants;
mod codemode_shell;
mod codemode_swap;
mod codemode_wake;
mod collector_schedule;
mod docs;
mod dormancy_loops;
mod dump_config;
mod exec_headless;
mod graph_invariants;
mod hooks_journal;
mod include;
mod invariants;
mod leader_swap;
mod ledger_invariants;
mod ledger_swap;
mod loop_swap;
mod many_agents;
mod mcp_call;
mod memory_invariants;
mod old_feed_surface;
mod phase6_swap;
mod projection_bench;
mod projection_swap;
mod projection_tiers;
mod rollups_swap;
mod swap;
mod system_schedules;
mod token_calibration;
mod tui_boot;
mod tui_config;
mod tui_invariants;
mod tui_swap;
mod wards_v9;
mod watch_broadcast;
mod worker_live;
mod worker_pr;
mod worker_spawn;
mod workers_seam;
