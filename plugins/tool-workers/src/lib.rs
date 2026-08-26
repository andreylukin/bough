//! Invariant (§9, §10): the model's natural path to a worker is the SEAM's path. Both tools do
//! nothing but translate arguments into a `StartWorker` / an `ask`, so the bounds, the seal and
//! the durable chain cannot be bypassed by calling a tool instead of the handle.
//!
//! Two catalog rows in one crate (`tool-spawn_worker`, `tool-ask`): they share one argument
//! vocabulary and neither has a life without the other.

pub mod invariant;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};
use bough_plugin_workers::{
    AskAnswer, Restrict, SealSpec, StartWorker, WorkerKind, WorkerOutcome, Workers,
};

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

fn args_of<T: serde::de::DeserializeOwned>(call: &ToolCall) -> Result<T, ToolFailure> {
    serde_json::from_value(call.args.clone()).map_err(|e| ToolFailure {
        kind: FailureClass::Error,
        message: format!("bad arguments for `{}`: {e}", call.name),
    })
}

/// The `spawn_worker` tool.
pub struct SpawnWorkerTool;

impl SpawnWorkerTool {
    /// The registration. Global: any agent that is allowed to spawn calls the same tool, and
    /// `tools.restrict` is what takes it away from one that is not.
    pub fn spec() -> ToolSpec {
        ToolSpec {
            name: ToolName::new("spawn_worker"),
            description: "Start a worker on a self-contained task and wait for its sealed report. \
                          The worker gets a fresh context: it sees the task and nothing of this \
                          conversation, so say everything it needs."
                .to_string(),
            input_schema: schemars::SchemaGenerator::default().into_root_schema_for::<SpawnArgs>(),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(SpawnWorkerTool),
        }
    }
}

#[async_trait::async_trait]
impl Tool for SpawnWorkerTool {
    /// Two workers at once is exactly what the seam's `max_in_flight` bound exists to govern, so
    /// the tool itself does not serialise them.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    async fn call(&self, call: Arc<ToolCall>, cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let args: SpawnArgs = args_of(&call)?;
        let workers = cx.ctx.get::<Workers>().map_err(|e| ToolFailure {
            kind: FailureClass::NotFound,
            message: format!("workers seam unavailable: {e}"),
        })?;
        // §2: an opaque id is a BRAND, not a spelling. `AgentId` is a uuidv7 minted by
        // `AgentsHandle`, so it is looked up from the live registry by name; stuffing the NAME
        // into the brand made the type look authoritative while carrying a value no registry
        // lookup could ever match.
        let agents = cx
            .ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(|e| ToolFailure {
                kind: FailureClass::NotFound,
                message: format!("agents registry unavailable: {e}"),
            })?;
        let spawner_id = agents
            .by_name(&call.agent)
            .map(|a| a.id().clone())
            .ok_or_else(|| ToolFailure {
                kind: FailureClass::NotFound,
                message: format!("no live agent named `{}` to spawn a worker for", call.agent),
            })?;
        let req = StartWorker {
            kind: WorkerKind::Spawn,
            spawner: call.agent.clone(),
            spawner_id,
            wake: call.wake.clone(),
            // §7's idem/cite formula wants the TRIGGERING step. A `ToolCall` carries no step id
            // (P2 seam gap), so the call id names the trigger; it is unique per step and is what
            // the durable `tool/call` row is keyed by.
            step: bough_plugin_ledger::StepId::new(format!("toolcall:{}", call.id)),
            depth: workers.depth_of(&call.agent).saturating_add(1),
            task: args.task,
            seal: SealSpec::report(),
            tools: args.tools.map(|names| Restrict {
                allow: Some(
                    names
                        .into_iter()
                        .map(ToolName::new)
                        .collect::<BTreeSet<_>>(),
                ),
                deny: BTreeSet::new(),
            }),
            ask_mode: workers.default_ask_mode(),
            at: chrono::Utc::now(),
        };
        let result = workers.start(&cx.ctx, req).await.map_err(|e| ToolFailure {
            kind: FailureClass::Blocked,
            message: e.to_string(),
        })?;
        Ok(render(&result))
    }
}

/// What the model is shown for a finished run. The report's own citations travel with the
/// outcome, so the spawner's next claim can cite them (§10).
fn render(result: &bough_plugin_workers::WorkerResult) -> ToolOutcome {
    let cites = result
        .report
        .as_ref()
        .map(|r| bough_plugin_workers::external_cites_of(&result.worker, r))
        .unwrap_or_default();
    let content = match &result.outcome {
        WorkerOutcome::Done => {
            // A `WorkerProvider` is the seam's extension point, so `Done` with no report is a
            // boundary case and not an internal invariant: it is REPORTED, never panicked on
            // inside a model-facing tool call.
            match result.report.as_ref() {
                None => "the worker finished but filed no report".to_string(),
                Some(r) => {
                    let mut s = r.summary.clone();
                    for claim in &r.claims {
                        s.push_str("\n- ");
                        s.push_str(&claim.text);
                        if !claim.is_externally_cited(&result.worker) {
                            s.push_str("  (uncited: recorded as a thought)");
                        }
                    }
                    s
                }
            }
        }
        WorkerOutcome::Asked { question, .. } => {
            format!("the worker stopped and asks: {question}")
        }
        WorkerOutcome::Failed(why) => format!("the worker failed: {why}"),
        WorkerOutcome::Cancelled => "the worker was cancelled".to_string(),
    };
    ToolOutcome {
        content,
        value: None,
        cites,
        concludes_wake: false,
    }
}

/// The `ask` tool: a worker's question to its spawner.
pub struct AskTool;

impl AskTool {
    pub fn spec() -> ToolSpec {
        ToolSpec {
            name: ToolName::new("ask"),
            description: "Ask the agent that started you for a decision you cannot make yourself. \
                          Use this instead of guessing. Depending on how you were started, you \
                          will either wait for an answer or stop here and leave the question with \
                          your spawner."
                .to_string(),
            input_schema: schemars::SchemaGenerator::default().into_root_schema_for::<AskArgs>(),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(AskTool),
        }
    }
}

#[async_trait::async_trait]
impl Tool for AskTool {
    async fn call(&self, call: Arc<ToolCall>, cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let args: AskArgs = args_of(&call)?;
        let workers = cx.ctx.get::<Workers>().map_err(|e| ToolFailure {
            kind: FailureClass::NotFound,
            message: format!("workers seam unavailable: {e}"),
        })?;
        // Only a live worker has a spawner to ask. Anyone else calling `ask` is asking nobody.
        let run = workers
            .run_for_agent(&call.agent)
            .ok_or_else(|| ToolFailure {
                kind: FailureClass::Denied,
                message: "`ask` is a worker's tool: you have no spawner to ask".to_string(),
            })?;
        match run.ask(args.question).await.map_err(|e| ToolFailure {
            kind: FailureClass::Error,
            message: e.to_string(),
        })? {
            AskAnswer::Answered(text) => Ok(ToolOutcome {
                content: text,
                value: None,
                cites: Vec::new(),
                concludes_wake: false,
            }),
            AskAnswer::Ended => Ok(ToolOutcome {
                content: "your question was delivered; stop here and let your spawner answer it"
                    .to_string(),
                value: None,
                cites: Vec::new(),
                concludes_wake: true,
            }),
        }
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

    /// `agents` is required because the spawner's real `AgentId` is looked up from the live
    /// registry rather than spelled from its name (§2's branded ids).
    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "workers", "agents"])
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tools = ctx.get::<Tools>().map_err(|e| PluginError::new(entry, e))?;
        tools.register(&ctx, SpawnWorkerTool::spec()).await?;
        Ok(())
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

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tools = ctx.get::<Tools>().map_err(|e| PluginError::new(entry, e))?;
        tools.register(&ctx, AskTool::spec()).await?;
        Ok(())
    }
}

bough_kernel::register_plugin!(SpawnWorkerToolPlugin);
bough_kernel::register_plugin!(AskToolPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Cite, Ref};
    use bough_plugin_workers::{Report, ReportClaim, WorkerId, WorkerResult};

    fn result(outcome: WorkerOutcome, report: Option<Report>) -> WorkerResult {
        WorkerResult {
            worker: WorkerId::new("w1"),
            outcome,
            report,
            steps: 0,
            usage: Default::default(),
            report_step: None,
        }
    }

    /// The report's EXTERNAL cites travel to the spawner with the outcome, and an uncited claim
    /// is marked as such in the text the model reads — the same split §10 makes in the chain.
    #[test]
    fn a_done_run_renders_its_claims_and_carries_only_external_cites() {
        let r = Report {
            summary: "edited".into(),
            claims: vec![
                ReportClaim {
                    text: "line 3 changed".into(),
                    cites: vec![Cite {
                        r#ref: Ref::new("step:s1"),
                        url: None,
                    }],
                },
                ReportClaim {
                    text: "probably fine".into(),
                    cites: vec![Cite {
                        r#ref: Ref::new("worker:w1"),
                        url: None,
                    }],
                },
            ],
        };
        let out = render(&result(WorkerOutcome::Done, Some(r)));
        assert_eq!(out.cites.len(), 1);
        assert_eq!(out.cites[0].r#ref.as_str(), "step:s1");
        assert!(out.content.contains("line 3 changed"));
        assert!(out.content.contains("uncited"));
        assert!(!out.concludes_wake);
    }

    /// A question the worker stopped on is shown as a question, not as a result.
    #[test]
    fn an_asking_run_renders_the_question() {
        let out = render(&result(
            WorkerOutcome::Asked {
                question: "which branch?".into(),
                message: bough_plugin_agents::MessageId::new("m1"),
            },
            None,
        ));
        assert!(out.content.contains("which branch?"), "{}", out.content);
        assert!(out.cites.is_empty());
    }
}
