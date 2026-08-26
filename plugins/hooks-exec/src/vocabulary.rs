//! Invariant: what a hook did is in the LEDGER, not only in a log. `hook/fired` is `Thought` and
//! `ignorable: true`: it is the harness's own bookkeeping, and a binary that knows nothing of hooks
//! may skip these rows.

use bough_plugin_runtime_actions::RuntimeAction;

/// `hook/fired`.
pub const HOOK_FIRED: &str = "hook/fired";

/// The `hook/fired` body.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct HookFired {
    pub point: String,
    pub exec: String,
    pub actions: Vec<RuntimeAction>,
    /// One line per action, in order.
    pub outcomes: Vec<String>,
    pub ms: u64,
    pub ok: bool,
}
