//! Invariant: model-visible ⟺ ledgered (§0.2). A worker's existence, its report and each of its
//! uncited claims are steps in the SPAWNER's chain — the report is EVIDENCE (it carries the
//! report's external cites), a bare claim is a THOUGHT (§10).

use bough_plugin_ledger::StepTypeDef;

use crate::ids::WorkerId;
use crate::seal::ReportClaim;
use crate::start::WorkerKind;

/// `worker/started` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WorkerStarted {
    pub worker: WorkerId,
    pub kind: WorkerKind,
    pub task: String,
    pub depth: u8,
    pub seal: String,
}

/// `worker/report` — EVIDENCE. Cites are the union of the report's external cites.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WorkerReport {
    pub worker: WorkerId,
    pub summary: String,
    pub claims: Vec<ReportClaim>,
    pub steps: u32,
}

/// `worker/claim` — Thought. One per claim whose only citation is the worker's own report.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WorkerClaim {
    pub worker: WorkerId,
    pub text: String,
}

/// The three step types this crate owns. WP-6.
pub fn step_types() -> Vec<StepTypeDef> {
    todo!("WP-6: worker/started Thought, worker/report Evidence, worker/claim Thought")
}
