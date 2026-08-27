//! Invariant: the launcher composes and tears down, and does nothing else (§0.1 item 2). A
//! behaviour that lives here instead of in a plugin row is a §0.1 violation.
//!
//! The crate is a library AND a binary for one reason: `crates/bough/tests/*` must call the very
//! same `compose_for` the binary calls, because V6's claim is an IDENTITY between the dump and
//! what boots — a test that reimplemented the layer stack could not check it.

// Linking, not naming: `inventory` only sees a plugin crate the linker kept, and an unreferenced
// dependency can be dropped. This `as _` import is the whole of the launcher's relationship with
// every plugin crate — it never names a plugin type (§0.1 item 2).
use bough_plugin_hello as _;
use bough_plugin_ledger_memory as _;
use bough_plugin_ledger_sqlite as _;
use bough_plugin_projection_assembler as _;
use bough_plugin_projection_probe as _;
// Phase 2's rows (§17 Phase 2). Same relationship as above: linked, never named.
use bough_plugin_about_line as _;
use bough_plugin_actions as _;
use bough_plugin_agent_loop as _;
use bough_plugin_agent_loop_scripted as _;
use bough_plugin_agents as _;
use bough_plugin_exec_headless as _;
use bough_plugin_llm as _;
use bough_plugin_llm_anthropic as _;
use bough_plugin_llm_replay as _;
use bough_plugin_llm_retry as _;
use bough_plugin_model_policy as _;
use bough_plugin_tool_actions as _;
use bough_plugin_tool_workers as _;
use bough_plugin_tools as _;
use bough_plugin_tools_baseline as _;
use bough_plugin_worker_spawn as _;
use bough_plugin_workers as _;
// Phase 3's rows (§17 Phase 3). `tui-probe` is a FIXTURE: linked into the catalog, named by no
// bundle, mounted only by a test's or a script's own `--patch`.
use bough_plugin_commands as _;
use bough_plugin_old_feed_adapter as _;
use bough_plugin_residents as _;
use bough_plugin_tui_focus as _;
use bough_plugin_tui_probe as _;
use bough_plugin_tui_search as _;
use bough_plugin_tui_shell as _;
use bough_plugin_tui_strip as _;
// Phase 4's rows (§17 Phase 4). `rollups-none` is a FIXTURE provider: linked into the catalog,
// named by no bundle, selected only by a swap patch.
use bough_plugin_drift_watch as _;
use bough_plugin_reconsolidation as _;
use bough_plugin_rollups_none as _;
use bough_plugin_rollups_summarizer as _;

// Phase 5's rows (§17 Phase 5): many agents, the leader, graph ops. Linked for the one reason
// every row above is — so `inventory::submit!` lands in the binary's catalog.
use bough_plugin_claims as _;
use bough_plugin_dormancy as _;
use bough_plugin_graph_ops as _;
use bough_plugin_lane_scope as _;
use bough_plugin_leader as _;
use bough_plugin_mail_router as _;
use bough_plugin_tool_leader as _;
use bough_plugin_worker_fork as _;
// Phase ux1's one new row (§17 Phase 3 / phase ux1 §2.13): the status line.
use bough_plugin_tui_status as _;

pub mod boot;
pub mod cli;
pub mod compose;
pub mod exec;
pub mod profile;
pub mod watch;
