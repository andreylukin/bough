//! Invariant: every variant names the ROW and the RULE it broke — a refusal a caller cannot act on
//! is a bug (§0.5: misconfiguration and rule violations fail loud).

use crate::id::Seq;
use crate::id::{RollupId, StepId, StepType, TrajId, WakeId};

/// Everything the ledger refuses.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("step type `{kind}` is not registered (append refused)")]
    UnknownStepTypeOnAppend { kind: StepType },
    #[error("step `{step}` in trajectory `{traj}` has type `{kind}`, unknown to this binary and not ignorable")]
    UnknownStepTypeOnRead {
        step: StepId,
        traj: TrajId,
        kind: StepType,
    },
    #[error("step type `{kind}` is already registered by plugin `{owner}`")]
    DuplicateStepType { kind: StepType, owner: &'static str },
    #[error(
        "an evidence step of type `{kind}` was appended with no cites; evidence requires citations"
    )]
    EvidenceWithoutCites { kind: StepType },
    #[error("step type `{kind}` may only be appended as {expected}, not {got}")]
    ClassRuleViolated {
        kind: StepType,
        expected: &'static str,
        got: &'static str,
    },
    #[error("body of `{kind}` does not match its schema: {detail}")]
    BodySchema { kind: StepType, detail: String },
    #[error("fork of `{parent}` at seq {at_seq:?} lies inside wake `{wake}`, opened at seq {opened_at:?} and never closed")]
    ForkInsideOpenWake {
        parent: TrajId,
        at_seq: Seq,
        wake: WakeId,
        opened_at: Seq,
    },
    #[error("rollup `{0}` is already superseded by `{1}`; superseded_by is set once")]
    AlreadySuperseded(RollupId, RollupId),
    #[error("ledger at `{path}` has format version {found}, this binary speaks {expected}")]
    FormatVersion {
        path: String,
        found: u32,
        expected: u32,
    },
    #[error("no such agent `{0}`; agents are mutable config and a row can be deleted")]
    NoSuchAgent(crate::id::AgentName),
    #[error("no such trajectory `{0}`")]
    NoSuchTrajectory(TrajId),
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}
