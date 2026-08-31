//! Invariant (§2): there is ONE loop. This crate drives no loop of its own — a private one-shot
//! loop inside a worker Provider would put loop code in a second crate. It forks the ledger,
//! assembles the PARENT at the fork seq, pins that prefix on the child, and hands the child to the
//! ordinary agent machinery; the parent's message history reaches it through `transcript::rebuild`
//! over the forked chain, which is the other half of "keeps the parent's history" (§10).

pub mod invariant;
pub mod point;
pub mod prefix;
pub mod vocabulary;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{
    Agent, AgentError, AgentKind, AgentSetup, Agents, AgentsHandle, CancelCause, CreateAgent,
    MailClass, Message, MessageId, Sender, Target,
};
use bough_plugin_ledger::{Fork, Ledger, LedgerHandle, Order, StepQuery, TrajId, WakeId};
use bough_plugin_llm::Usage;
use bough_plugin_projection::{
    AssembleRequest, Assembled, PrefixSource, Projection, ProjectionHandle,
};
use bough_plugin_tools::{Restrict, Tools, ToolsHandle};
use bough_plugin_worker_spawn::{ReportSlot, ReportTool};
use bough_plugin_workers::{
    AskMode, StartWorker, WorkerError, WorkerId, WorkerKind, WorkerOutcome, WorkerProvider,
    WorkerResult, WorkerRun, Workers, WorkersHandle,
};

pub use point::fork_point;
pub use prefix::prefix_append;
pub use vocabulary::{ForkPrefix, FORK_PREFIX, OWNER};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "worker-fork";

/// The trajectory one fork child gets. A fork is a REAL ledger fork: the child's chain begins at
/// the `fork/end-seed` marker below the parent's prefix.
pub fn fork_traj(id: &WorkerId) -> TrajId {
    TrajId::new(format!("worker-fork-{id}"))
}

/// The provider for [`WorkerKind::Fork`].
pub struct ForkProvider {
    cfg: Arc<ForkConfig>,
    /// The row's own context: `WorkerProvider::start` takes none, and forking, assembling and
    /// creating an agent all need one (`worker-spawn`'s precedent).
    ctx: Option<Context>,
}

impl ForkProvider {
    /// A provider with no context: only the pure surface works.
    pub fn new(cfg: Arc<ForkConfig>) -> ForkProvider {
        ForkProvider { cfg, ctx: None }
    }

    /// The provider the row mounts.
    pub fn with_ctx(ctx: Context, cfg: Arc<ForkConfig>) -> ForkProvider {
        ForkProvider {
            cfg,
            ctx: Some(ctx),
        }
    }

    fn ctx(&self) -> Result<&Context, WorkerError> {
        self.ctx
            .as_ref()
            .ok_or_else(|| WorkerError::Agent(AgentError::NoFactory))
    }
}

/// What the creation transaction does inside the fork child's own scope: pin the parent's prefix,
/// record where the pin came from, restrict its tools, give it the report door and the step
/// budget. Every one of them registers through `agent.ctx()`, so every one unwinds with the child
/// and leaves nothing behind (§0.2) — the pin included.
struct ForkSetup {
    ledger: LedgerHandle,
    /// The fork ROW's projection handle, carried in rather than resolved off the child's context:
    /// see `prefix::pin`.
    projection: ProjectionHandle,
    tools: ToolsHandle,
    restrict: Option<Restrict>,
    spec: parking_lot::Mutex<Option<bough_plugin_tools::ToolSpec>>,
    max_steps: u32,
    prefix: parking_lot::Mutex<Option<Assembled>>,
    source: PrefixSource,
    traj: TrajId,
    at: chrono::DateTime<chrono::Utc>,
}

#[async_trait::async_trait]
impl AgentSetup for ForkSetup {
    async fn setup(&self, agent: &Agent) -> Result<(), AgentError> {
        let failed = |detail: String| AgentError::SetupFailed {
            name: agent.name().clone(),
            detail,
        };
        // (a) The pin, FIRST: the child must never assemble a projection of its own, not even
        //     once, or the request it sends is not the parent's.
        let prefix = self
            .prefix
            .lock()
            .take()
            .ok_or_else(|| AgentError::SetupFailed {
                name: agent.name().clone(),
                detail: "the fork prefix was already consumed: setup runs once".to_string(),
            })?;
        prefix::pin(
            agent.ctx(),
            &self.projection,
            agent.name(),
            prefix,
            self.source.clone(),
        )
        .await
        .map_err(|e| failed(format!("pin_prefix: {e}")))?;
        // (b) The anchor that keeps §0.2 true through the pin.
        self.ledger
            .0
            .append(prefix_append(
                &self.traj,
                &WakeId::seed(&self.traj),
                &self.source.of_agent,
                self.source.as_of,
                self.at,
            ))
            .await
            .map_err(|e| failed(format!("fork/prefix: {e}")))?;

        if let Some(r) = &self.restrict {
            self.tools
                .restrict(agent.ctx(), agent.name(), r.clone())
                .await
                .map_err(|e| failed(format!("tools.restrict: {e}")))?;
        }
        // The same budget `worker-spawn` gives a spawned child, counted off the DURABLE
        // `step/end` stream: a fork is not a way around the bound.
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
            .map_err(|e| failed(format!("step budget: {e}")))?;

        let spec = self
            .spec
            .lock()
            .take()
            .ok_or_else(|| AgentError::SetupFailed {
                name: agent.name().clone(),
                detail: "the fork spec was already consumed: setup runs once".to_string(),
            })?;
        self.tools
            .register(agent.ctx(), spec)
            .await
            .map_err(|e| failed(format!("report tool: {e}")))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl WorkerProvider for ForkProvider {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Fork]
    }

    /// In order: [`fork_point`] → `ledger.fork(parent → worker-fork-<id>)` → assemble the PARENT
    /// at that seq → `agents.create(CreateAgent { kind: Fork, traj: <the forked child>, setup })`
    /// where `setup` pins the prefix, appends `fork/prefix`, and registers the report tool and the
    /// step budget exactly as `worker-spawn` does. The seed message is the task.
    async fn start(
        &self,
        req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError> {
        let ctx = self.ctx()?;
        let seam = |e: bough_kernel::KernelError| WorkerError::Seam(e.to_string());
        let agents: Arc<AgentsHandle> = ctx.get::<Agents>().map_err(seam)?;
        let tools: Arc<ToolsHandle> = ctx.get::<Tools>().map_err(seam)?;
        let ledger: Arc<LedgerHandle> = ctx.get::<Ledger>().map_err(seam)?;
        let projection: Arc<ProjectionHandle> = ctx.get::<Projection>().map_err(seam)?;

        let id = run.id().clone();
        let name = WorkersHandle::worker_agent_name(&req.spawner, &id);

        // 1. The parent's chain, newest-first, and the seq a fork may branch at (P5-D7).
        let parent = ledger
            .0
            .agent(&req.spawner)
            .await?
            .ok_or_else(|| {
                WorkerError::Seam(format!("`{}` has no agents row to fork from", req.spawner))
            })?
            .traj;
        let steps_desc = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![parent.clone()],
                kinds: crate::point::WAKE_KINDS
                    .iter()
                    .map(bough_plugin_ledger::StepType::new)
                    .collect(),
                order: Order::SeqDesc,
                ..Default::default()
            })
            .await?;
        let head = ledger
            .0
            .head_seq(&parent)
            .await?
            .unwrap_or(bough_plugin_ledger::Seq(0));
        let at_seq = fork_point(head, &steps_desc).ok_or_else(|| {
            WorkerError::Seam(format!(
                "`{parent}` has no closed prefix to fork at: a fork is never clipped into an \
                 open wake"
            ))
        })?;

        // 2. The REAL ledger fork. The edge and the end-seed marker are one transaction.
        let child_traj = fork_traj(&id);
        ledger
            .0
            .fork(Fork {
                parent: parent.clone(),
                child: child_traj.clone(),
                at_seq,
                at: req.at,
            })
            .await?;

        // 3. The PARENT's prefix at that seq — the bytes §10 asks for, byte-identical.
        let prefix = projection
            .0
            .assemble(&AssembleRequest {
                agent: req.spawner.clone(),
                wake: None,
                at: req.at,
                budget: None,
                as_of: Some(at_seq),
            })
            .await
            .map_err(|e| WorkerError::Seam(e.to_string()))?;

        let slot = ReportSlot::new();
        let setup = ForkSetup {
            ledger: ledger.as_ref().clone(),
            projection: projection.as_ref().clone(),
            tools: tools.as_ref().clone(),
            restrict: req.tools.clone(),
            spec: parking_lot::Mutex::new(Some(ReportTool::spec(
                name.clone(),
                req.seal.clone(),
                slot.clone(),
            ))),
            max_steps: self.cfg.max_steps,
            prefix: parking_lot::Mutex::new(Some(prefix)),
            source: PrefixSource {
                of_agent: req.spawner.clone(),
                as_of: at_seq,
            },
            traj: child_traj.clone(),
            at: req.at,
        };

        let seed = Message {
            id: MessageId::new(uuid::Uuid::now_v7().to_string()),
            from: Sender::Agent(req.spawner.clone()),
            class: MailClass::Wake,
            text: req.task.clone(),
            subject: "task".to_string(),
            cites: Vec::new(),
            refs: BTreeSet::new(),
            mail_seq: None,
            at: req.at,
        };

        let (agent, disposer) = agents
            .create(CreateAgent {
                name: name.clone(),
                traj: child_traj,
                kind: AgentKind::Fork,
                scope: None,
                setup: Some(Arc::new(setup)),
                seed: vec![(seed, Target::NextWake)],
                at: req.at,
            })
            .await?;

        // One shot: the child runs its one wake and is disposed — with it, the pin.
        let cancel = run.cancel();
        tokio::select! {
            _ = agent.when_idle() => {}
            _ = cancel.cancelled() => {
                agent.cancel(CancelCause::Parent, false).await;
            }
        }
        let cancelled = cancel.is_cancelled();
        disposer.dispose().await;

        let empty = |outcome: WorkerOutcome| {
            Ok(WorkerResult {
                worker: id.clone(),
                outcome,
                report: None,
                steps: 0,
                usage: Usage::default(),
                report_step: None,
            })
        };
        if let Some(asked) = run.asked() {
            if run.ask_mode() == AskMode::End {
                return empty(WorkerOutcome::Asked {
                    question: asked.question,
                    message: asked.message,
                });
            }
        }
        if cancelled {
            return empty(WorkerOutcome::Cancelled);
        }
        let Some(body) = slot.take() else {
            return empty(WorkerOutcome::Failed(
                "the fork ended without calling `report`".to_string(),
            ));
        };
        // The SEAL is the authority, not the tool: a second door onto the same slot could not
        // skip this.
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

        // The report lands in the SPAWNER's chain through the same path a spawned worker's does
        // (§10): nothing about a fork changes what a report is.
        let report_step =
            bough_plugin_worker_spawn::land_in_spawners_chain(&ledger, &req, &id, &report).await?;
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

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForkConfig {
    /// The step budget one fork child gets. It counts against the SAME spawn bounds as a
    /// `worker-spawn` child: a fork is not a way around the bound.
    pub max_steps: u32,
}

/// The `worker.fork` row.
pub struct WorkerForkPlugin;

#[async_trait::async_trait]
impl Plugin for WorkerForkPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ForkConfig;

    fn inject() -> Inject {
        Inject::required(["workers", "agents", "ledger", "projection", "tools"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let workers = ctx
            .get::<Workers>()
            .map_err(|e| PluginError::new(entry, e))?;
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;
        workers
            .provider(&ctx, Arc::new(ForkProvider::with_ctx(ctx.clone(), cfg)))
            .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::pinned_prefix_reconstructs()]
    }
}

bough_kernel::register_plugin!(WorkerForkPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    /// The child's trajectory NAMES the run, so two forks of one parent never share a chain.
    #[test]
    fn a_forks_trajectory_names_its_worker() {
        assert_eq!(fork_traj(&WorkerId::new("w1")).as_str(), "worker-fork-w1");
    }

    /// A provider with no context refuses rather than panicking: `kinds()` is pure and safe to
    /// ask, `start` is not.
    #[test]
    fn a_contextless_provider_is_a_fork_provider_that_cannot_start() {
        let p = ForkProvider::new(Arc::new(ForkConfig { max_steps: 8 }));
        assert_eq!(p.kinds(), vec![WorkerKind::Fork]);
        assert!(p.ctx().is_err());
    }
}
