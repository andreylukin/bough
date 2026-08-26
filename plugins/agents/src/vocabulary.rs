//! Invariant: model-visible ⟺ ledgered (§0.2). These four step types are the model-visible inputs
//! and outputs `agents` owns; each is declared through `LedgerHandle::declare_step_types`, so the
//! map stays merge-extensible and unloading this row leaves no trace of them.

use bough_plugin_ledger::{StepId, WakeId};

/// `thought/text` — Thought. One flush of streamed assistant text.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ThoughtText {
    pub text: String,
    pub step_index: u32,
}

/// `thought/reasoning` — Thought. `meta` is an opaque provider payload, replayed verbatim.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ThoughtReasoning {
    pub text: String,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
    pub step_index: u32,
}

/// `wake/jot` — Thought. The checkpoint a preempted wake resumes from. `synthetic` marks the one
/// the loop builds itself when the grace step fails (P2-D14) — a jot ALWAYS exists.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeJot {
    pub of_wake: WakeId,
    pub state: String,
    pub resume_hint: String,
    #[serde(default)]
    pub synthetic: bool,
}

/// `wake/grace-prompt` — Thought. The instruction the grace step is given (§5's checkpoint). It
/// is a MODEL-VISIBLE input, so §0.2 makes it a step type rather than a string the loop hands the
/// adapter on the side: `transcript::rebuild` folds it back into the same user message, and V4's
/// reconstruction of the grace step then succeeds instead of never seeing it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeGracePrompt {
    pub of_wake: WakeId,
    pub text: String,
    pub step_index: u32,
}

/// `wake/resumed` — Thought. Opens the next wake of ANY kind after a preemption.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeResumed {
    pub from_jot: StepId,
    pub of_wake: WakeId,
}

/// The step types this crate owns, for `declare_step_types`.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    const OWNER: &str = crate::PLUGIN_NAME;
    vec![
        StepTypeDef::of::<ThoughtText>("thought/text", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<ThoughtReasoning>("thought/reasoning", OWNER)
            .class_rule(ClassRule::Thought),
        StepTypeDef::of::<WakeJot>("wake/jot", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<WakeResumed>("wake/resumed", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<WakeGracePrompt>("wake/grace-prompt", OWNER)
            .class_rule(ClassRule::Thought),
    ]
}
