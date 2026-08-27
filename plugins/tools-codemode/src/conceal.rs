//! Invariant: concealment is VISIBILITY, never authority. Every tool the agent could call before
//! the row mounted is still callable from inside a program, through the SAME pipeline; all that
//! changes is which names the request's tool list carries.

use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{ToolSpec, ToolsHandle};

/// How the row hides everything but `run` from the request's tool list.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Debug,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ConcealMode {
    /// This branch's interim (§0.1): `Restrict { allow: {run} }` on the real handle, plus a
    /// MIRROR `ToolsHandle` holding the snapshot, which is what inner calls execute against.
    #[default]
    Mirror,
    /// Post-merge: `ToolsHandle::conceal`, one handle, no mirror.
    Seam,
    /// No concealment: `run` sits alongside the typed tools. Test and bench-control arm only.
    None,
}

/// The snapshot a program executes against.
pub struct Mirror {
    /// The specs visible to the agent at the moment `run` was called.
    pub specs: Vec<ToolSpec>,
    /// The handle the inner calls go through — the same pipeline, a private registry.
    pub tools: ToolsHandle,
}

/// Snapshot `agent`'s visible tools and build the mirror handle they execute against.
///
/// WP-2 owns the body.
pub fn snapshot(_tools: &ToolsHandle, _agent: &AgentName) -> Mirror {
    todo!("WP-2: snapshot the agent's ToolSpecs under lock and rebuild them into a mirror handle")
}
