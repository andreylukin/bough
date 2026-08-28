//! Invariant: model-visible ⟺ ledgered (§0.2), from the OTHER side. Inner calls are ledgered but
//! NOT model-visible — the model sees console output — so they carry their own step kinds rather
//! than `tool/call`/`tool/result`, which `agent-loop`'s `transcript::rebuild` folds into the
//! request wholesale. Distinct kinds keep the reconstruction honest with zero edits to the loop.

use bough_plugin_js::JsError;
use bough_plugin_tools::{AttachedContext, RenderIntent, ToolCallId, ToolName, ToolOutcomeKind};

/// `program/call` — Thought. One inner tool call made from inside a program.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProgramCallBody {
    /// The `run` call this is under — the nesting anchor. There is no nesting column: sub-steps
    /// land in seq order between the `run` call's `tool/call` and its `tool/result`.
    pub program: ToolCallId,
    /// 0-based, in issue order within the program.
    pub index: u32,
    /// `{program}.{index}` — DETERMINISTIC, so replay reproduces it.
    pub call: ToolCallId,
    pub name: ToolName,
    pub args: serde_json::Value,
    pub render: RenderIntent,
    /// `bash`/`sh` only.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The wake step the `run` call belongs to.
    pub step_index: u32,
}

/// `program/result` — Either (cites decide the class, as `tool/result` does).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProgramResultBody {
    pub program: ToolCallId,
    pub index: u32,
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
    pub step_index: u32,
    pub ms: u64,
}

/// `program/console` — Thought. One flush of console output, appended AS PRODUCED so the TUI
/// streams it. The ordered concatenation of a program's chunks IS the `run` call's
/// `tool/result` content.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProgramConsoleBody {
    pub program: ToolCallId,
    pub chunk: u32,
    pub text: String,
    /// `> 0` on the truncation notice chunk.
    #[serde(default)]
    pub dropped_bytes: usize,
}

/// `program/error` — Thought. The one terminal error a program can end with.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProgramErrorBody {
    pub program: ToolCallId,
    pub error: JsError,
    pub ops: u64,
    pub ms: u64,
}

/// The four step types this crate owns, for `declare_step_types`.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    const OWNER: &str = crate::PLUGIN_NAME;
    vec![
        StepTypeDef::of::<ProgramCallBody>("program/call", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<ProgramResultBody>("program/result", OWNER).class_rule(ClassRule::Either),
        StepTypeDef::of::<ProgramConsoleBody>("program/console", OWNER)
            .class_rule(ClassRule::Thought),
        StepTypeDef::of::<ProgramErrorBody>("program/error", OWNER).class_rule(ClassRule::Thought),
    ]
}
