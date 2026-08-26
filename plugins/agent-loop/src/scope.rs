//! Invariant (§5): the per-agent scope is minted BY THE LOOP at creation, shadowing is
//! most-specific-wins (the kernel's `create_scope` already does this), and `tools.restrict`
//! composes as an INTERSECTION — a scope can narrow what an agent may do and never widen it.

use bough_kernel::ScopeKey;
use bough_plugin_ledger::AgentName;

/// The scope key for one agent. The one place the spelling lives.
pub fn scope_key(name: &AgentName) -> ScopeKey {
    ScopeKey::new(format!("agent:{name}"))
}
