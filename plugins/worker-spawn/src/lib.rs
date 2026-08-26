//! Invariant (§10): a worker gets a FRESH TASK-ONLY CONTEXT. Its agent is created through the
//! agent factory on its own trajectory, seeded with exactly the write-boundary block and the
//! task, with `tools.restrict` applied in its own scope — and with NO projection of the
//! spawner's history. What comes back is the report, sealed; what lands in the spawner's chain is
//! cited evidence plus one thought per uncited claim.

pub mod ask;
pub mod boundary;
pub mod chain;
pub mod invariant;
pub mod report_tool;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_agents::{
    Agent, AgentError, AgentKind, AgentSetup, Agents, AgentsHandle, CancelCause, CreateAgent,
    MailClass, Message, MessageId, Sender, Target,
};
use bough_plugin_ledger::{Ledger, LedgerHandle, TrajId};
use bough_plugin_llm::Usage;
use bough_plugin_tools::{Restrict, Tools, ToolsHandle};
use bough_plugin_workers::{
    AskMode, StartWorker, WorkerError, WorkerKind, WorkerOutcome, WorkerProvider, WorkerResult,
    WorkerRun, Workers, WorkersHandle,
};
use chrono::Utc;

pub use ask::{AgentsAskSink, RecordingAskSink};
pub use boundary::WRITE_BOUNDARY;
pub use report_tool::{ReportSlot, ReportTool};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "worker-spawn";

/// The row's config. The boundary block is NOT here (P2-D21).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnConfig {
    /// What `ask()` does to the worker when the spawner does not say.
    pub ask_mode: AskMode,
    /// How many steps one worker may spend before it is ended with a failure.
    pub max_steps: u32,
}

/// The provider.
pub struct SpawnProvider {
    cfg: Arc<SpawnConfig>,
    /// The row's own context. `WorkerProvider::start` takes no `Context`, and creating an agent
    /// needs one, so the row hands its own in at `apply`. `None` in the pure tests, which only
    /// exercise [`SpawnProvider::seed_task`].
    ctx: Option<Context>,
}

impl SpawnProvider {
    /// A provider with no context: only the pure surface works.
    pub fn new(cfg: Arc<SpawnConfig>) -> SpawnProvider {
        SpawnProvider { cfg, ctx: None }
    }

    /// The provider the row mounts.
    pub fn with_ctx(ctx: Context, cfg: Arc<SpawnConfig>) -> SpawnProvider {
        SpawnProvider {
            cfg,
            ctx: Some(ctx),
        }
    }

    /// The seeded task: the boundary block FIRST, then the task.
    ///
    /// Pure, so the ordering can be pinned without a runtime — the roundtrip test still asserts
    /// on the recorded request, because this function being right does not prove it is what the
    /// adapter saw.
    pub fn seed_task(task: &str) -> String {
        format!("{WRITE_BOUNDARY}\n\nYour task:\n\n{task}")
    }

    fn ctx(&self) -> Result<&Context, WorkerError> {
        self.ctx
            .as_ref()
            .ok_or_else(|| WorkerError::Agent(AgentError::NoFactory))
    }
}

/// What the creation transaction does inside the worker's own scope: restrict its tools and give
/// it the one door its report goes through. Both register through `agent.ctx()`, so both unwind
/// with the agent and leave nothing behind (§0.2).
struct WorkerSetup {
    tools: ToolsHandle,
    restrict: Option<Restrict>,
    spec: parking_lot::Mutex<Option<bough_plugin_tools::ToolSpec>>,
    /// The step budget of §10's bounds' little brother: a worker that never reports is ended
    /// rather than left to spend the spawner's money. Counted off the DURABLE `step/end` stream
    /// of the worker's own trajectory, so a loop that forgets to tell anyone still trips it.
    max_steps: u32,
}

#[async_trait::async_trait]
impl AgentSetup for WorkerSetup {
    async fn setup(&self, agent: &Agent) -> Result<(), AgentError> {
        if let Some(r) = &self.restrict {
            self.tools
                .restrict(agent.ctx(), agent.name(), r.clone())
                .await
                .map_err(|e| AgentError::SetupFailed {
                    name: agent.name().clone(),
                    detail: format!("tools.restrict: {e}"),
                })?;
        }
        let (traj, budget, victim) = (agent.traj().clone(), self.max_steps, agent.clone());
        let spent = Arc::new(std::sync::atomic::AtomicU32::new(0));
        agent
            .ctx()
            .on::<bough_plugin_ledger::LedgerStep, _, _>(move |step| {
                let (traj, victim, spent) = (traj.clone(), victim.clone(), spent.clone());
                async move {
                    if step.traj != traj || step.kind.as_str() != "step/end" {
                        return;
                    }
                    if spent.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 >= budget {
                        victim.cancel(CancelCause::Parent, false).await;
                    }
                }
            })
            .await
            .map_err(|e| AgentError::SetupFailed {
                name: agent.name().clone(),
                detail: format!("step budget: {e}"),
            })?;

        let spec = self.spec.lock().take().expect("setup runs once");
        self.tools
            .register(agent.ctx(), spec)
            .await
            .map_err(|e| AgentError::SetupFailed {
                name: agent.name().clone(),
                detail: format!("report tool: {e}"),
            })?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl WorkerProvider for SpawnProvider {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Spawn]
    }

    async fn start(
        &self,
        req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError> {
        let ctx = self.ctx()?;
        let agents: Arc<AgentsHandle> = ctx
            .get::<Agents>()
            .map_err(|e| WorkerError::Seam(e.to_string()))?;
        let tools: Arc<ToolsHandle> = ctx
            .get::<Tools>()
            .map_err(|e| WorkerError::Seam(e.to_string()))?;
        let ledger: Arc<LedgerHandle> = ctx
            .get::<Ledger>()
            .map_err(|e| WorkerError::Seam(e.to_string()))?;

        let id = run.id().clone();
        let name = WorkersHandle::worker_agent_name(&req.spawner, &id);
        // A FRESH trajectory: the worker sees its task, never the spawner's history (§10).
        let traj = TrajId::new(format!("worker-{id}"));
        let slot = ReportSlot::new();

        let setup = WorkerSetup {
            tools: tools.as_ref().clone(),
            restrict: req.tools.clone(),
            spec: parking_lot::Mutex::new(Some(ReportTool::spec(
                name.clone(),
                req.seal.clone(),
                slot.clone(),
            ))),
            max_steps: self.cfg.max_steps,
        };

        let seed = Message {
            id: MessageId::new(uuid::Uuid::now_v7().to_string()),
            from: Sender::Agent(req.spawner.clone()),
            class: MailClass::Wake,
            text: SpawnProvider::seed_task(&req.task),
            subject: "task".to_string(),
            cites: Vec::new(),
            refs: BTreeSet::new(),
            mail_seq: None,
            at: req.at,
        };

        let (agent, disposer) = agents
            .create(CreateAgent {
                name: name.clone(),
                traj,
                kind: AgentKind::Worker,
                scope: None,
                setup: Some(Arc::new(setup)),
                seed: vec![(seed, Target::NextWake)],
                at: req.at,
            })
            .await?;

        // Wait for the worker to finish its own wake, for the run to be cancelled, or for the
        // spawner to have ended it on a question.
        let cancel = run.cancel();
        tokio::select! {
            _ = agent.when_idle() => {}
            _ = cancel.cancelled() => {
                agent.cancel(CancelCause::Parent, false).await;
            }
        }
        let cancelled = cancel.is_cancelled();
        disposer.dispose().await;

        // An `end`-mode question outranks everything: the worker stopped ON it, and the spawner
        // now owns it.
        if let Some(asked) = run.asked() {
            if run.ask_mode() == AskMode::End {
                return Ok(WorkerResult {
                    worker: id,
                    outcome: WorkerOutcome::Asked {
                        question: asked.question,
                        message: asked.message,
                    },
                    report: None,
                    steps: 0,
                    usage: Usage::default(),
                    report_step: None,
                });
            }
        }
        if cancelled {
            return Ok(WorkerResult {
                worker: id,
                outcome: WorkerOutcome::Cancelled,
                report: None,
                steps: 0,
                usage: Usage::default(),
                report_step: None,
            });
        }

        let Some(body) = slot.take() else {
            return Ok(WorkerResult {
                worker: id,
                outcome: WorkerOutcome::Failed(
                    "the worker ended without calling `report`".to_string(),
                ),
                report: None,
                steps: 0,
                usage: Usage::default(),
                report_step: None,
            });
        };
        // The tool already validated; validating again here is what makes the SEAL the authority
        // rather than the tool — a second door onto the same slot could not skip it.
        req.seal
            .validate(&body)
            .map_err(|detail| WorkerError::SealInvalid {
                seal: req.seal.name.clone(),
                detail,
            })?;
        let report: bough_plugin_workers::Report =
            serde_json::from_value(body).map_err(|e| WorkerError::SealInvalid {
                seal: req.seal.name.clone(),
                detail: e.to_string(),
            })?;

        let report_step = land_in_spawners_chain(&ledger, &req, &id, &report).await?;
        bough_plugin_workers::invariant::record(bough_plugin_workers::invariant::Obs::Reported {
            fiber: ctx.fiber_uid(),
            worker: id.clone(),
        });

        Ok(WorkerResult {
            worker: id,
            outcome: WorkerOutcome::Done,
            report: Some(report),
            steps: 0,
            usage: Usage::default(),
            report_step,
        })
    }
}

/// Append the report and its uncited claims to the SPAWNER's chain, and answer with the
/// `worker/report` step id so the spawner's next claim can cite it.
pub async fn land_in_spawners_chain(
    ledger: &LedgerHandle,
    req: &StartWorker,
    worker: &bough_plugin_workers::WorkerId,
    report: &bough_plugin_workers::Report,
) -> Result<Option<bough_plugin_ledger::StepId>, WorkerError> {
    let Some(row) = ledger.0.agent(&req.spawner).await? else {
        return Ok(None);
    };
    let appends = chain::report_appends(worker, &row.traj, &req.wake, Utc::now(), report, 0);
    let steps = ledger.0.append_batch(appends).await?;
    Ok(steps.first().map(|s| s.id.clone()))
}

/// The provider row.
pub struct SpawnPlugin;

#[async_trait::async_trait]
impl Plugin for SpawnPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SpawnConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["workers", "agents", "ledger", "tools"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let workers = ctx
            .get::<Workers>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry, e))?;
        workers
            .ask_sink(&ctx, Arc::new(AgentsAskSink::new(agents.as_ref().clone())))
            .await?;
        workers.set_default_ask_mode(&ctx, cfg.ask_mode).await?;
        workers
            .provider(&ctx, Arc::new(SpawnProvider::with_ctx(ctx.clone(), cfg)))
            .await?;
        Ok(())
    }
}

bough_kernel::register_plugin!(SpawnPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    /// The normative half of the seeding rule: the block comes FIRST, and the task is not merged
    /// into it.
    #[test]
    fn the_boundary_block_comes_first_and_the_task_after_it() {
        let seeded = SpawnProvider::seed_task("rename `foo` to `bar` in src/lib.rs");
        assert!(
            seeded.starts_with(WRITE_BOUNDARY),
            "the boundary block is not first"
        );
        assert!(seeded.contains("rename `foo` to `bar`"));
        assert!(
            seeded.find("Your task:").unwrap() > WRITE_BOUNDARY.len() - 1,
            "the task label precedes the block"
        );
    }
}
