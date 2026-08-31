//! Invariant: model-visible ⟺ ledgered (§0.2). A worker's existence, its report and each of its
//! uncited claims are steps in the SPAWNER's chain — the report is EVIDENCE (it carries the
//! report's external cites), a bare claim is a THOUGHT (§10).

use bough_plugin_ledger::{ClassRule, StepTypeDef};

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

/// The three step types this crate owns.
///
/// `worker/report` carries the union of the report's EXTERNAL cites, so it lands as EVIDENCE —
/// the spawner's justification for anything it goes on to say. Its class rule is `Either` and not
/// `Evidence` for one case only: a report with no external cite at all is not evidence about the
/// world, and §10 says so; it lands as a thought rather than being refused or given a fake cite.
/// `worker/started` and `worker/claim` are always THOUGHTS.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![
        StepTypeDef::of::<WorkerStarted>("worker/started", crate::PLUGIN_NAME)
            .class_rule(ClassRule::Thought),
        StepTypeDef::of::<WorkerReport>("worker/report", crate::PLUGIN_NAME)
            .class_rule(ClassRule::Either),
        StepTypeDef::of::<WorkerClaim>("worker/claim", crate::PLUGIN_NAME)
            .class_rule(ClassRule::Thought),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three names and their classes are the seam's contract with §10; a rename here is a
    /// vocabulary change, not a refactor.
    #[test]
    fn the_three_step_types_are_declared_with_their_classes() {
        let defs = step_types();
        let named: Vec<(String, &'static str)> = defs
            .iter()
            .map(|d| (d.name.to_string(), d.class_rule.as_str()))
            .collect();
        assert_eq!(
            named,
            vec![
                ("worker/started".to_string(), "thought"),
                ("worker/report".to_string(), "evidence or thought"),
                ("worker/claim".to_string(), "thought"),
            ]
        );
    }
}
