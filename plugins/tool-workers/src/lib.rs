//! Invariant (§9, §10): the model's natural path to a worker is the SEAM's path. Both tools do
//! nothing but translate arguments into a `StartWorker` / an `ask`, so the bounds, the seal and
//! the durable chain cannot be bypassed by calling a tool instead of the handle.
//!
//! Two catalog rows in one crate (`tool-spawn_worker`, `tool-ask`): they share one argument
//! vocabulary and neither has a life without the other.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

/// What `spawn_worker` takes from the model.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SpawnArgs {
    /// The whole task. The spawner prepends the write-boundary block; the model cannot.
    pub task: String,
    /// Tool names the worker may use. Composed as an INTERSECTION with what the spawner has.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

/// What `ask` takes from the model.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AskArgs {
    pub question: String,
}

/// The `spawn_worker` tool.
pub struct SpawnWorkerTool;

#[async_trait::async_trait]
impl Tool for SpawnWorkerTool {
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-6: parse SpawnArgs, workers.start(..), render the report")
    }
}

/// The `ask` tool: a worker's question to its spawner.
pub struct AskTool;

#[async_trait::async_trait]
impl Tool for AskTool {
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-6: run.ask(question) — wake-class mail on the spawner's lane")
    }
}

/// No configuration: everything that varies is on the `workers` row.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolWorkersConfig {}

/// The `tool-spawn_worker` row.
pub struct SpawnWorkerToolPlugin;

#[async_trait::async_trait]
impl Plugin for SpawnWorkerToolPlugin {
    const NAME: &'static str = "tool-spawn_worker";
    type Config = ToolWorkersConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "workers"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: tools.register(spawn_worker, Generic)")
    }
}

/// The `tool-ask` row.
pub struct AskToolPlugin;

#[async_trait::async_trait]
impl Plugin for AskToolPlugin {
    const NAME: &'static str = "tool-ask";
    type Config = ToolWorkersConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "workers"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: tools.register(ask, Generic)")
    }
}

bough_kernel::register_plugin!(SpawnWorkerToolPlugin);
bough_kernel::register_plugin!(AskToolPlugin);
