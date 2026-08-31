//! Invariant: a worker run has one id for its whole life, and the same id names it in the
//! spawner's chain, in the live-run table and in the report seal's cites.

/// One worker run.
///
/// Declared in `plugins/agents` (where `Sender::Worker` names it) and re-exported here, so §10's
/// spelling is unchanged and the dependency direction stays acyclic.
pub use bough_plugin_agents::WorkerId;
