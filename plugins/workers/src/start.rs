//! Invariant (§10): a worker starts from a REQUEST that names its spawner, the triggering step
//! and its depth. Bounds are decided from this request in the Definition, so every provider obeys
//! the same numbers (§7).

use bough_plugin_agents::{AgentId, MessageId};
use bough_plugin_ledger::{AgentName, StepId, WakeId};
use bough_plugin_llm::Usage;
use bough_plugin_tools::Restrict;
use chrono::{DateTime, Utc};

use crate::ids::WorkerId;
use crate::seal::{Report, SealSpec};

/// Spawn (Phase 2) or fork (Phase 5).
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum WorkerKind {
    Spawn,
    Fork,
}

/// What `ask()` does to the worker.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AskMode {
    /// The worker waits for the spawner's answer.
    Block,
    /// The worker ends, and the question is the spawner's to pick up.
    End,
}

/// One start request.
#[derive(Clone)]
pub struct StartWorker {
    pub kind: WorkerKind,
    pub spawner: AgentName,
    pub spawner_id: AgentId,
    pub wake: WakeId,
    /// The triggering step: bounds and cites both need it.
    pub step: StepId,
    pub depth: u8,
    pub task: String,
    pub seal: SealSpec,
    pub tools: Option<Restrict>,
    pub ask_mode: AskMode,
    pub at: DateTime<Utc>,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkerOutcome {
    Done,
    /// Mode `end`: the worker stopped on a question, which is now mail on the spawner's lane.
    Asked {
        question: String,
        message: MessageId,
    },
    Failed(String),
    Cancelled,
}

/// What a finished run gives the spawner.
#[derive(Clone, Debug)]
pub struct WorkerResult {
    pub worker: WorkerId,
    pub outcome: WorkerOutcome,
    pub report: Option<Report>,
    pub steps: u32,
    pub usage: Usage,
    /// The `worker/report` step in the SPAWNER's chain, so the spawner's next claim can cite it.
    pub report_step: Option<StepId>,
}

/// The three bounds of §7, enforced in the Definition.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Bounds {
    /// Default 8.
    pub max_in_flight: usize,
    /// Default 3: a worker's worker's worker is the last generation.
    pub max_depth: u8,
    /// How many a single wake may start.
    pub per_wake_spawn_cap: usize,
}
