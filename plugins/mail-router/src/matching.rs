//! Invariant: this module is PURE and it is the ONE place §3's fan-out rule is written — every
//! agent whose `routing_refs` intersect the envelope's refs, in NAME order. Never "the best
//! match"; never one winner. A router that picks a winner strands the true owner of an event, and
//! that failure is invisible until someone notices work that never happened.

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, AgentRow, Ref};

/// Every matching agent, name-ordered and deduplicated. An agent with empty `routing_refs`
/// matches nothing (an empty intersection is empty, and a lane that asked for nothing gets
/// nothing).
pub fn recipients(_refs: &BTreeSet<Ref>, _rows: &[AgentRow]) -> Vec<AgentName> {
    todo!("WP-1: intersect routing_refs, return name-ordered unique matches")
}

/// The wake classes an envelope carries: its refs in the `class:` namespace, stripped of the
/// prefix (P5-D3). A ref outside the namespace is not a class and is ignored here.
pub fn wake_classes_of(_refs: &BTreeSet<Ref>) -> BTreeSet<String> {
    todo!("WP-1: read the `class:` namespace out of the refs")
}

/// The namespace P5-D3 spells a wake class in.
pub const CLASS_NAMESPACE: &str = "class:";
