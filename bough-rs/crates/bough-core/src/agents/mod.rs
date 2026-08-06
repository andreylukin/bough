//! Subagents (port of `src/agents/`): spawn caps and leases, launch/result
//! building, and the wake-note pipeline. Never references the server crate.

pub mod caps;
pub mod notes;
pub mod subagent;
