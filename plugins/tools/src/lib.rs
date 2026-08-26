//! Invariant: this crate is the tools SERVICE DEFINITION (§9). It owns the `tools` key, the
//! scoped registry, the three-stage guarded pipeline and the two step types — and no tool. The
//! executor refuses a tool that is not in the calling agent's scope, so the set the model is
//! shown and the set it can call are the same set, by construction.
//!
//! P2-D1: it owns live state (the tool map), so it IS a catalog row and provides its own key.

pub mod approval;
pub mod error;
mod exec;
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
    /// Registered tools, each with the registration id its disposer removes.
    tools: parking_lot::Mutex<Vec<(u64, ToolSpec)>>,
    /// Live restrictions. Several may be in force for one agent; they compose as an INTERSECTION.
    restrict: parking_lot::Mutex<Vec<(u64, AgentName, Restrict)>>,
    /// The approver, if a row mounted one. `None` in Phase 2, so `ask` degrades to deny.
    approval: parking_lot::Mutex<Option<ApprovalHandle>>,
    next_id: std::sync::atomic::AtomicU64,
    /// From `ToolsConfig`. Neither is a `const`: both vary by deployment.
    max_parallel: usize,
    default_deadline_ms: u64,
}

impl ToolsHandle {
    /// An empty registry with explicit limits. `ToolsPlugin::apply` builds the live one from
    /// `ToolsConfig`; tests build small ones.
    ///
    /// There is no `new()` and no `Default`: the deadline and the parallelism are
    /// deployment-varying values and `ToolsConfig` is their one source (§0.2). A second
    /// constructor carrying literals would be a second, invisible one.
    ///
    /// `max_parallel` is clamped to at least 1: zero is not "no limit", it is a batch of nothing,
    /// and the dispatcher would spin on it forever.
    pub fn with_limits(max_parallel: usize, default_deadline_ms: u64) -> ToolsHandle {
        let max_parallel = max_parallel.max(1);
        ToolsHandle(Arc::new(ToolsInner {
            tools: parking_lot::Mutex::new(Vec::new()),
            restrict: parking_lot::Mutex::new(Vec::new()),
            approval: parking_lot::Mutex::new(None),
            next_id: std::sync::atomic::AtomicU64::new(1),
            max_parallel,
            default_deadline_ms,
        }))
    }

    fn mint(&self) -> u64 {
        self.0
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Register a tool. Registration is an effect (§0.2): the disposer removes it.
    pub async fn register(
        &self,
        ctx: &Context,
        spec: ToolSpec,
    ) -> Result<EffectHandle, PluginError> {
        {
            let tools = self.0.tools.lock();
            if tools
                .iter()
                .any(|(_, s)| s.name == spec.name && s.scope == spec.scope)
            {
                let scope = match &spec.scope {
                    ToolScope::Global => "globally".to_string(),
                    ToolScope::Agent(a) => format!("for agent `{a}`"),
                };
                return Err(PluginError::new(
                    ctx.entry_id().clone(),
                    ToolsError::Duplicate {
                        name: spec.name.clone(),
                        scope,
                    },
                ));
            }
        }
        let id = self.mint();
        self.0.tools.lock().push((id, spec));
        let inner = self.0.clone();
        ctx.effect(move |e| async move {
            e.defer_sync(move || inner.tools.lock().retain(|(i, _)| *i != id));
            Ok(())
        })
        .await
    }

    /// §5: an INTERSECTION filter over the global set, registered in the agent's scope.
    ///
    /// Several restrictions may be in force at once; [`Restrict::intersect`] composes them, so a
    /// second one can only narrow.
    pub async fn restrict(
        &self,
        ctx: &Context,
        agent: &AgentName,
        r: Restrict,
    ) -> Result<EffectHandle, PluginError> {
        let id = self.mint();
        self.0.restrict.lock().push((id, agent.clone(), r));
        let inner = self.0.clone();
        ctx.effect(move |e| async move {
            e.defer_sync(move || inner.restrict.lock().retain(|(i, _, _)| *i != id));
            Ok(())
        })
        .await
    }

    /// Mount an approver. Phase 2 mounts none in production, so `ask` degrades to deny; a surface
    /// (Phase 3) and the pipeline tests mount one through here.
    pub async fn mount_approval(
        &self,
        ctx: &Context,
        approver: ApprovalHandle,
    ) -> Result<EffectHandle, PluginError> {
        *self.0.approval.lock() = Some(approver);
        let inner = self.0.clone();
        ctx.effect(move |e| async move {
            e.defer_sync(move || *inner.approval.lock() = None);
            Ok(())
        })
        .await
    }

    /// The effective restriction for `agent`: the intersection of everything in force.
    fn effective_restrict(&self, agent: &AgentName) -> Restrict {
        self.0
            .restrict
            .lock()
            .iter()
            .filter(|(_, a, _)| a == agent)
            .fold(Restrict::default(), |acc, (_, _, r)| acc.intersect(r))
    }

    /// The specs visible to `agent`, in NAME order. An agent-scoped tool SHADOWS its same-named
    /// global twin, for that agent alone.
    fn visible_specs(&self, agent: &AgentName) -> Vec<ToolSpec> {
        let restrict = self.effective_restrict(agent);
        let tools = self.0.tools.lock();
        let mut by_name: std::collections::BTreeMap<ToolName, &ToolSpec> =
            std::collections::BTreeMap::new();
        for (_, spec) in tools.iter() {
            match &spec.scope {
                ToolScope::Global => {
                    by_name.entry(spec.name.clone()).or_insert(spec);
                }
                ToolScope::Agent(a) if a == agent => {
                    // Most specific wins.
                    by_name.insert(spec.name.clone(), spec);
                }
                ToolScope::Agent(_) => {}
            }
        }
        by_name
            .into_values()
            .filter(|s| restrict.admits(&s.name))
            .cloned()
            .collect()
    }

    /// EXACTLY what the prompt shows. Scoped tools shadow same-named globals for that agent
    /// alone; a restricted tool is simply absent.
    pub fn schemas(&self, agent: &AgentName) -> Vec<LlmToolDef> {
        self.visible_specs(agent)
            .into_iter()
            .map(|s| LlmToolDef {
                name: s.name.to_string(),
                description: s.description.clone(),
                input_schema: s.input_schema.as_value().clone(),
            })
            .collect()
    }

    /// The visible names, for a surface and for error messages.
    pub fn visible(&self, agent: &AgentName) -> Vec<ToolName> {
        self.visible_specs(agent)
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    /// The DECLARED render intent for a tool (§9), which is what a surface draws its call with.
    ///
    /// A name the agent cannot see answers `Generic`: the intent is presentation, so an unknown
    /// tool must degrade to the neutral shape rather than fail a wake that is otherwise fine.
    pub fn render_intent(&self, agent: &AgentName, name: &ToolName) -> RenderIntent {
        self.visible_specs(agent)
            .into_iter()
            .find(|s| &s.name == name)
            .map(|s| s.render)
            .unwrap_or(RenderIntent::Generic)
    }

    /// A filtered-away tool answers `NotFound`, indistinguishably from a nonexistent one (§9).
    pub fn resolve(&self, agent: &AgentName, name: &ToolName) -> Result<Arc<dyn Tool>, ToolsError> {
        self.visible_specs(agent)
            .into_iter()
            .find(|s| &s.name == name)
            .map(|s| s.tool)
            .ok_or_else(|| ToolsError::NotFound {
                name: name.clone(),
                agent: agent.clone(),
            })
    }

    /// The guarded pipeline: `tools/pre-execute` → `tools/execute` → `tools/post-execute` →
    /// `tools/result`. Concurrency-safe calls dispatch in parallel, everything else forms a
    /// barrier; only DISPATCH overlaps — the returned results are in the MODEL's call order.
    pub async fn execute(&self, ctx: &Context, calls: Vec<ToolCall>) -> Vec<ToolResult> {
        self.execute_under(ctx, calls, tokio_util::sync::CancellationToken::new())
            .await
    }

    /// The same pipeline, under the CALLER's cancellation signal.
    ///
    /// §5 says an interrupt or a cancel stops the wake producing, and §9 says a `tools/execute`
    /// wrapper replaces the cancellation signal — both of which need a root signal the caller
    /// holds. Minting a fresh token per call, which is what [`ToolsHandle::execute`] does and all
    /// this crate's tests want, meant that once a step entered tool execution neither Andrey's
    /// preemption nor `cancel(User | Disposed)` could reach the running tool: the wake blocked
    /// until the tool returned or hit its deadline.
    pub async fn execute_under(
        &self,
        ctx: &Context,
        calls: Vec<ToolCall>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Vec<ToolResult> {
        exec::execute(self, ctx, calls, cancel).await
    }

    /// The approver, if a row mounted one. `None` in Phase 2, so `ask` degrades to deny.
    pub fn approval(&self) -> Option<ApprovalHandle> {
        self.0.approval.lock().clone()
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

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<bough_plugin_ledger::Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // The two step types this crate owns. Declaration is an effect, so unloading the row
        // leaves the step-type map as if it had never mounted (§0.2).
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

        // The invariant's stream is per-LIFE: a reload keeps the `FiberUid`, so this fiber's
        // observations are forgotten when it unloads.
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(mine));
            Ok(())
        })
        .await?;

        // Record `tool/call` / `tool/result` off the DURABLE ledger stream, never off the live
        // pipeline: the invariant is about what was committed (P2-D25).
        let fiber = ctx.fiber_uid();
        ctx.on::<bough_plugin_ledger::LedgerStep, _, _>(move |step| async move {
            let kind = step.kind.as_str();
            // `wake/end` is observed too: it is the moment "no call is left unanswered" becomes
            // checkable, and without it a dangling `tool/call` passes forever.
            if kind != "tool/call" && kind != "tool/result" && kind != "wake/end" {
                return;
            }
            let call = step
                .body
                .get("call")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let step_index = step
                .body
                .get("step_index")
                .and_then(|v| v.as_u64())
                .unwrap_or_default() as u32;
            invariant::record(invariant::Obs {
                fiber,
                wake: step.wake.clone(),
                kind: step.kind.clone(),
                call,
                step_index,
            });
        })
        .await?;

        ctx.provide::<Tools>(ToolsHandle::with_limits(
            cfg.max_parallel,
            cfg.default_deadline_ms,
        ))
        .await
        .map_err(|e| PluginError::new(entry, e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::calls_and_results_pair_within_a_step()]
    }
}

bough_kernel::register_plugin!(ToolsPlugin);
