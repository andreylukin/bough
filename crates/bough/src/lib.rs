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

pub mod boot;
pub mod cli;
pub mod compose;
pub mod exec;
pub mod profile;
pub mod watch;
