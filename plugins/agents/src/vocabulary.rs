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

/// `wake/resumed` — Thought. Opens the next wake of ANY kind after a preemption.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeResumed {
    pub from_jot: StepId,
    pub of_wake: WakeId,
}

/// The four step types this crate owns, for `declare_step_types`.
///
/// WP-2.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    todo!("WP-2: thought/text, thought/reasoning, wake/jot, wake/resumed — all Thought")
}
