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
///
/// WP-3.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    todo!("WP-3: tool/call (Thought), tool/result (Either)")
}
