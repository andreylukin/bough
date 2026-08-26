//! Invariant: model-visible ⟺ ledgered (§0.2). A tool call and its result are the two step types
//! this crate owns; `tool/result.outcome == "unknown"` is `TOOL_OUTCOME_UNKNOWN`, the value crash
//! repair synthesises and the one outcome no live pipeline can produce.

use bough_plugin_llm::{ToolCallId, ToolName};

use crate::tool::{AttachedContext, RenderIntent};

/// `tool/call` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ToolCallBody {
    pub call: ToolCallId,
    pub name: ToolName,
    pub args: serde_json::Value,
    pub render: RenderIntent,
    pub step_index: u32,
}

/// What became of a call. `Unknown` is written by crash repair only.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeKind {
    Ok,
    Error,
    Denied,
    Blocked,
    Unknown,
}

/// `tool/result` — EITHER class (P2-D26): the tool decides by supplying cites, and the ledger's
/// evidence-requires-cites rule does the rest.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ToolResultBody {
    pub call: ToolCallId,
    pub name: ToolName,
    pub outcome: ToolOutcomeKind,
    pub content: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub attached: Vec<AttachedContext>,
    #[serde(default)]
    pub concludes_wake: bool,
}

/// The two step types this crate owns, for `declare_step_types`.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    const OWNER: &str = crate::PLUGIN_NAME;
    vec![
        StepTypeDef::of::<ToolCallBody>("tool/call", OWNER).class_rule(ClassRule::Thought),
        // EITHER (P2-D26): the tool decides by supplying cites, and the ledger's
        // evidence-requires-cites rule does the rest.
        StepTypeDef::of::<ToolResultBody>("tool/result", OWNER).class_rule(ClassRule::Either),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_types_are_declared_with_their_class_rules() {
        let defs = step_types();
        let names: Vec<String> = defs.iter().map(|d| d.name.to_string()).collect();
        assert_eq!(
            names,
            vec!["tool/call".to_string(), "tool/result".to_string()]
        );
        assert_eq!(defs[0].class_rule, bough_plugin_ledger::ClassRule::Thought);
        assert_eq!(defs[1].class_rule, bough_plugin_ledger::ClassRule::Either);
        assert!(defs.iter().all(|d| d.owner == crate::PLUGIN_NAME));
    }

    #[test]
    fn a_result_body_validates_against_its_own_schema() {
        let defs = step_types();
        let body = serde_json::to_value(ToolResultBody {
            call: bough_plugin_llm::ToolCallId::new("c1"),
            name: bough_plugin_llm::ToolName::new("bash"),
            outcome: ToolOutcomeKind::Ok,
            content: "ok".into(),
            value: None,
            attached: vec![],
            concludes_wake: false,
        })
        .unwrap();
        defs[1].validate_body(&body).unwrap();
    }
}
