//! Invariant (§5, §9): `tools.restrict` is an INTERSECTION filter over the global set, never a
//! way to add a tool, and an agent-scoped tool shadows its same-named global twin for that agent
//! alone. `schemas()` is the single source of truth for what the prompt shows.

use std::collections::BTreeSet;

use bough_plugin_llm::ToolName;

/// One restriction. Registered in an agent's scope, so it unwinds with the agent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Restrict {
    /// `None` ⇒ everything the deny list admits. `Some` ⇒ only these.
    pub allow: Option<BTreeSet<ToolName>>,
    pub deny: BTreeSet<ToolName>,
}

impl Restrict {
    /// The composition rule (§5): two restrictions compose as an INTERSECTION, so a second one
    /// can only narrow.
    ///
    /// WP-3.
    pub fn intersect(&self, _other: &Restrict) -> Restrict {
        todo!("WP-3: allow = intersection of allows, deny = union of denies")
    }

    /// Whether `name` survives this restriction. WP-3.
    pub fn admits(&self, _name: &ToolName) -> bool {
        todo!("WP-3")
    }
}
