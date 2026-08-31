//! Invariant: `fork/prefix` is a THOUGHT, not evidence. It records what the harness did to build a
//! request — the pin's reconstruction anchor — and asserts nothing about the world.

use bough_plugin_ledger::{AgentName, ClassRule, Seq, StepTypeDef};

/// The owner string the step type is registered under.
pub const OWNER: &str = "worker-fork";

/// The step type's name. Read by name elsewhere (P3-D11).
pub const FORK_PREFIX: &str = "fork/prefix";

/// `fork/prefix` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ForkPrefix {
    pub of_agent: AgentName,
    pub as_of: Seq,
}

/// The one step type this crate owns.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![StepTypeDef::of::<ForkPrefix>(FORK_PREFIX, OWNER).class_rule(ClassRule::Thought)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_prefix_is_a_thought() {
        let defs = step_types();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name.as_str(), FORK_PREFIX);
        assert_eq!(defs[0].class_rule.as_str(), "thought");
    }
}
