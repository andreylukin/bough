//! Invariant (§9): a tool declares its render intent UP FRONT, and `is_concurrency_safe(args)`
//! returning EXACTLY `true` is the only thing that permits parallel dispatch — everything else is
//! exclusive and forms a barrier.

use std::sync::Arc;
use std::time::Instant;

use bough_kernel::Context;
use bough_plugin_ledger::{AgentName, Cite, WakeId};
use bough_plugin_llm::{ToolCallId, ToolName};
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::AgentId;

/// How a surface should render this tool's call and result. Decided up front (§9).
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum RenderIntent {
    Generic,
    Terminal,
    Diff,
}

/// Where a tool is visible.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolScope {
    /// Every agent, unless a scoped tool of the same name shadows it.
    Global,
    /// One agent only. Registered in that agent's scope, so it unwinds with the agent.
    Agent(AgentName),
}

/// One registered tool.
#[derive(Clone)]
pub struct ToolSpec {
    pub name: ToolName,
    pub description: String,
    pub input_schema: schemars::Schema,
    pub render: RenderIntent,
    pub scope: ToolScope,
    pub tool: Arc<dyn Tool>,
}

/// What a tool does.
#[async_trait::async_trait]
pub trait Tool: Send + Sync + 'static {
    /// EXACTLY `true` permits parallel dispatch; everything else is exclusive (§9).
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, cx: ToolCx) -> Result<ToolOutcome, ToolFailure>;
}

/// One call, as the model asked for it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: ToolName,
    pub args: serde_json::Value,
    pub agent: AgentName,
    pub wake: WakeId,
    pub step_index: u32,
}

impl ToolCall {
    /// A stable digest of the call, so the executor can tell whether a `tools/execute` wrapper
    /// edited it (P2-D13: §9 does not offer input rewrite).
    ///
    /// WP-3.
    pub fn digest(&self) -> String {
        todo!("WP-3: sha256 over id, name and canonical args")
    }
}

/// What the tool is handed at dispatch.
pub struct ToolCx {
    pub ctx: Context,
    pub cancel: CancellationToken,
    pub deadline: Option<Instant>,
    /// Attribution only, never authorization (§2).
    pub initiator: Option<AgentId>,
}

/// What a tool returns on success.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ToolOutcome {
    pub content: String,
    pub value: Option<serde_json::Value>,
    /// Supplying cites is what makes the durable `tool/result` EVIDENCE rather than a thought
    /// (P2-D26).
    pub cites: Vec<Cite>,
    /// `true` ends the wake at this step (§5).
    pub concludes_wake: bool,
}

/// What a tool returns on failure.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolFailure {
    pub kind: FailureClass,
    pub message: String,
}

/// The failure taxonomy the model sees. A filtered-away tool answers `NotFound`,
/// indistinguishably from a nonexistent one (§9).
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    NotFound,
    Denied,
    Blocked,
    Timeout,
    Cancelled,
    /// Crash repair's synthesised outcome, and the one no live pipeline can produce.
    Unknown,
    Error,
}

/// Extra context a `tools/post-execute` listener attached to a result.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AttachedContext {
    pub id: String,
    pub text: String,
}

/// The durable, model-ordered outcome of one call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub call: ToolCallId,
    pub name: ToolName,
    pub ok: bool,
    pub content: String,
    /// `accept` replaces content OR value, never both (§9); `block` yields a VALUELESS failure.
    pub value: Option<serde_json::Value>,
    pub attached: Vec<AttachedContext>,
    pub cites: Vec<Cite>,
    pub concludes_wake: bool,
    pub failure: Option<ToolFailure>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}
