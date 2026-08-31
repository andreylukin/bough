//! Invariant (§5): the per-agent scope is minted BY THE LOOP at creation, shadowing is
//! most-specific-wins (the kernel's `create_scope` already does this), and `tools.restrict`
//! composes as an INTERSECTION — a scope can narrow what an agent may do and never widen it.

use bough_kernel::ScopeKey;
use bough_plugin_ledger::AgentName;

/// The scope key for one agent. The one place the spelling lives.
pub fn scope_key(name: &AgentName) -> ScopeKey {
    ScopeKey::new(format!("agent:{name}"))
}

/// `tools.restrict` composes as an INTERSECTION (§5): a scope narrows what an agent may do and
/// can never widen it. The one place the loop states that, so a caller cannot get it backwards.
pub fn restrict_intersection(
    base: &bough_plugin_tools::Restrict,
    scoped: &bough_plugin_tools::Restrict,
) -> bough_plugin_tools::Restrict {
    base.intersect(scoped)
}
