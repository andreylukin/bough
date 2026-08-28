//! Invariant: a live firing is RECONSTRUCTIBLE. `ward/fired` records what the ward returned and
//! what each action did, so `--since` has something to read and a ward's behaviour can be replayed
//! from the ledger rather than from a log.
//!
//! `ClassRule::Thought`, `ignorable: true`: a ward firing is the harness's own reasoning, and a
//! binary that does not know about wards may skip these rows safely.

use bough_plugin_ledger::{ClassRule, Seq, StepTypeDef};
use bough_plugin_runtime_actions::RuntimeAction;

/// `ward/fired`.
pub const WARD_FIRED: &str = "ward/fired";

/// The `ward/fired` body.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WardFired {
    pub ward: String,
    /// The step seq it fired on.
    pub on: Seq,
    pub actions: Vec<RuntimeAction>,
    /// One line per action, in order: what it did or why it was refused.
    pub outcomes: Vec<String>,
    pub ops: u64,
    pub ms: u64,
}

/// The step type this crate owns, for `declare_step_types`.
///
/// `ignorable: true`: a binary that does not know about wards may SKIP these rows on read (§3).
pub fn step_types() -> Vec<StepTypeDef> {
    vec![StepTypeDef::of::<WardFired>(WARD_FIRED, crate::PLUGIN_NAME)
        .class_rule(ClassRule::Thought)
        .ignorable(true)]
}
