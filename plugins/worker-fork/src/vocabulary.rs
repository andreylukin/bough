//! Invariant: `fork/prefix` is a THOUGHT, not evidence. It records what the harness did to build a
//! request — the pin's reconstruction anchor — and asserts nothing about the world.

use bough_plugin_ledger::{AgentName, Seq};

/// The owner string the step type is registered under.
pub const OWNER: &str = "worker-fork";

/// `fork/prefix` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ForkPrefix {
    pub of_agent: AgentName,
    pub as_of: Seq,
}

/// Declare the step type on the bound ledger. Called once, from `apply`.
pub fn declare(_ledger: &bough_plugin_ledger::LedgerHandle) -> Result<(), anyhow::Error> {
    todo!("WP-6: declare fork/prefix as a Thought")
}
