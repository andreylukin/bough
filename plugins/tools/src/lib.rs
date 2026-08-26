//! Invariant: this crate is the tools SERVICE DEFINITION (§9). It owns the `tools` key, the
//! scoped registry, the three-stage guarded pipeline and the two step types — and no tool. The
//! executor refuses a tool that is not in the calling agent's scope, so the set the model is
//! shown and the set it can call are the same set, by construction.
//!
//! P2-D1: it owns live state (the tool map), so it IS a catalog row and provides its own key.

pub mod approval;
pub mod error;
pub mod invariant;
pub mod pipeline;
pub mod registry;
pub mod tool;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_ledger::AgentName;

pub use approval::{Approval, ApprovalHandle, ApprovalOutcome, Approver};
pub use error::ToolsError;
pub use pipeline::{
    Decision, Execution, PostExecute, PreExecute, ToolsExecute, ToolsPostExecute, ToolsPreExecute,
    ToolsResult,
};
pub use registry::Restrict;
pub use tool::{
    AttachedContext, FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome,
    ToolResult, ToolScope, ToolSpec,
};
pub use vocabulary::{ToolCallBody, ToolOutcomeKind, ToolResultBody};

/// The tool identifiers, re-exported: §9 spells them here, `plugins/llm` declares them because
/// the chunk vocabulary names them and `tools` depends on `llm` for `LlmToolDef`.
pub use bough_plugin_llm::{LlmToolDef, ToolCallId, ToolName};

/// Attribution only, never authorization (§2). `ToolCx` carries it, so this crate takes a
/// dependency on `agents` for the id type alone (the direction is acyclic: agents never depends
/// on tools).
pub use bough_plugin_agents::AgentId;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tools";

/// The `tools` service key.
pub struct Tools;

impl ServiceKey for Tools {
    type Value = ToolsHandle;
    const NAME: &'static str = "tools";
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct ToolsHandle(pub Arc<ToolsInner>);

/// The seam's live state: the tool map and the per-agent restrictions.
pub struct ToolsInner {
    /// WP-3 fills these in.
    _tools: parking_lot::Mutex<Vec<ToolSpec>>,
    _restrict: parking_lot::Mutex<Vec<(AgentName, Restrict)>>,
}

impl ToolsHandle {
    /// An empty registry. WP-3.
    pub fn new() -> ToolsHandle {
        ToolsHandle(Arc::new(ToolsInner {
            _tools: parking_lot::Mutex::new(Vec::new()),
            _restrict: parking_lot::Mutex::new(Vec::new()),
        }))
    }

    /// Register a tool. Registration is an effect (§0.2). WP-3.
    pub async fn register(
        &self,
        _ctx: &Context,
        _spec: ToolSpec,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-3: register, with the inverse that removes it")
    }

    /// §5: an INTERSECTION filter over the global set, registered in the agent's scope. WP-3.
    pub async fn restrict(
        &self,
        _ctx: &Context,
        _agent: &AgentName,
        _r: Restrict,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-3: compose as an intersection with whatever is already in scope")
    }

    /// EXACTLY what the prompt shows. Scoped tools shadow same-named globals for that agent
    /// alone. WP-3.
    pub fn schemas(&self, _agent: &AgentName) -> Vec<LlmToolDef> {
        todo!("WP-3: the visible set, as tool defs, in a stable order")
    }

    /// The visible names, for a surface and for error messages. WP-3.
    pub fn visible(&self, _agent: &AgentName) -> Vec<ToolName> {
        todo!("WP-3")
    }

    /// A filtered-away tool answers `NotFound`, indistinguishably from a nonexistent one (§9).
    /// WP-3.
    pub fn resolve(
        &self,
        _agent: &AgentName,
        _name: &ToolName,
    ) -> Result<Arc<dyn Tool>, ToolsError> {
        todo!("WP-3: scoped lookup, then the global one")
    }

    /// The guarded pipeline: `tools/pre-execute` → `tools/execute` → `tools/post-execute` →
    /// `tools/result`. Concurrency-safe calls dispatch in parallel, everything else forms a
    /// barrier; only DISPATCH overlaps — the returned results are in the MODEL's call order.
    ///
    /// WP-3.
    pub async fn execute(&self, _ctx: &Context, _calls: Vec<ToolCall>) -> Vec<ToolResult> {
        todo!("WP-3: the three-stage pipeline with the barrier dispatcher")
    }

    /// The approver, if a row mounted one. `None` in Phase 2, so `ask` degrades to deny. WP-3.
    pub fn approval(&self) -> Option<ApprovalHandle> {
        todo!("WP-3")
    }
}

impl Default for ToolsHandle {
    fn default() -> Self {
        ToolsHandle::new()
    }
}

/// The row's config. Both values vary by deployment, which is why neither is a `const`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// The deadline a call gets when nothing wraps it. Wrappers may only shorten it.
    pub default_deadline_ms: u64,
    /// How many concurrency-safe calls may dispatch at once.
    pub max_parallel: usize,
}

/// The Service Definition row.
pub struct ToolsPlugin;

#[async_trait::async_trait]
impl Plugin for ToolsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ToolsConfig;

    fn inject() -> bough_kernel::Inject {
        // `approval` is OPTIONAL: absent, `ask` degrades to deny (§9).
        bough_kernel::Inject::required(["ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.max_parallel == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "max_parallel must be at least 1".to_string(),
            });
        }
        Ok(())
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-3: declare the two step types, provide::<Tools>, record the invariant stream")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::calls_and_results_pair_within_a_step()]
    }
}

bough_kernel::register_plugin!(ToolsPlugin);
