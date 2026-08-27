//! §10 — the `fork` tool. What this file pins:
//!
//! * `fork` does nothing but translate its argument into a `StartWorker` of kind `Fork` on the
//!   SEAM, so the bounds, the seal and the durable chain cannot be bypassed by calling a tool;
//! * with no fork Provider mounted, the model is told the door does not exist here — a NotFound,
//!   not a retryable block, and never a silent spawn instead.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, Agents, AgentsHandle, Attach,
    CancelCause, CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tool_workers::{ForkArgs, ForkTool};
use bough_plugin_tools::{FailureClass, Tool, ToolCall, ToolCallId, ToolCx, ToolName};
use bough_plugin_workers::{
    Bounds, StartWorker, WorkerKind, WorkerOutcome, WorkerProvider, WorkerResult, WorkerRun,
    Workers, WorkersHandle,
};
use parking_lot::Mutex;

/// A driver that does nothing: these cases are about the TOOL, and a live agent is only needed so
/// the spawner's branded `AgentId` can be looked up the way §2 requires.
struct Idle;

#[async_trait::async_trait]
impl AgentDriver for Idle {
    fn driver(&self) -> &'static str {
        "idle"
    }
    async fn notify(&self, _r: &InboxReceipt, _m: &Message) {}
    async fn cancel(&self, _c: CancelCause, _keep: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _k: WakeKind, _c: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
}

struct IdleFactory;

#[async_trait::async_trait]
impl AgentFactory for IdleFactory {
    fn driver(&self) -> &'static str {
        "idle"
    }
    async fn attach(
        &self,
        _cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        Ok(Arc::new(Idle))
    }
}

/// A Provider that records the request and reports nothing. It is the seam's own extension point,
/// so what it sees IS what the tool asked for.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<StartWorker>>,
}

#[async_trait::async_trait]
impl WorkerProvider for Recorder {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Fork]
    }
    async fn start(
        &self,
        req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, bough_plugin_workers::WorkerError> {
        self.seen.lock().push((*req).clone());
        Ok(WorkerResult {
            worker: run.id().clone(),
            outcome: WorkerOutcome::Done,
            report: None,
            steps: 0,
            usage: Default::default(),
            report_step: None,
        })
    }
}

fn bounds() -> Bounds {
    Bounds {
        max_in_flight: 4,
        max_depth: 3,
        per_wake_spawn_cap: 4,
    }
}

fn call() -> Arc<ToolCall> {
    Arc::new(ToolCall {
        id: ToolCallId::new("c1"),
        name: ToolName::new("fork"),
        args: serde_json::to_value(ForkArgs {
            task: "read the failing test and say why it fails".to_string(),
        })
        .expect("args serialise"),
        agent: AgentName::new("sol"),
        wake: WakeId::new("wk1"),
        step_index: 3,
    })
}

/// A context with `agents` and `workers` bound, and one live agent named `sol`.
async fn mounted(workers: WorkersHandle) -> (Context, Agent, bough_plugin_agents::AgentDisposer) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = AgentsHandle::new(ctx.clone(), ledger);
    agents
        .set_factory(&ctx, Arc::new(IdleFactory))
        .await
        .expect("the slot is free");
    let (agent, disposer) = agents
        .create(CreateAgent::resident(
            AgentName::new("sol"),
            TrajId::new("lane/sol"),
            chrono::Utc::now(),
        ))
        .await
        .expect("the creation transaction commits");
    std::mem::forget(ctx.provide::<Agents>(agents).await.expect("agents"));
    std::mem::forget(ctx.provide::<Workers>(workers).await.expect("workers"));
    (ctx, agent, disposer)
}

fn cx(ctx: &Context) -> ToolCx {
    ToolCx {
        ctx: ctx.clone(),
        cancel: Default::default(),
        deadline: None,
        initiator: None,
    }
}

#[tokio::test]
async fn fork_starts_a_fork_kind_worker() {
    let workers = WorkersHandle::new(bounds());
    let (ctx, _agent, _d) = mounted(workers.clone()).await;
    let recorder = Arc::new(Recorder::default());
    workers
        .provider(&ctx, recorder.clone() as Arc<dyn WorkerProvider>)
        .await
        .expect("the provider mounts");

    ForkTool
        .call(call(), cx(&ctx))
        .await
        .expect("the tool reaches the seam");

    let seen = recorder.seen.lock().clone();
    assert_eq!(seen.len(), 1, "one call, one start");
    let req = &seen[0];
    assert_eq!(req.kind, WorkerKind::Fork, "a fork, never a spawn");
    assert_eq!(req.spawner.as_str(), "sol");
    assert_eq!(
        req.wake,
        WakeId::new("wk1"),
        "the caller's wake, so the per-wake cap counts it"
    );
    assert_eq!(
        req.task, "read the failing test and say why it fails",
        "the task reaches the seam unedited"
    );
    assert_eq!(req.depth, 1, "a fork of a resident is depth 1");
    assert_eq!(req.seal.name, bough_plugin_workers::SealSpec::report().name);
    assert!(
        req.tools.is_none(),
        "a fork continues the parent's work with the parent's tools"
    );
    assert_eq!(
        req.step.as_str(),
        "toolcall:c1",
        "the triggering step is named, so §7's idem/cite formula has one"
    );
}

#[tokio::test]
async fn fork_is_refused_when_no_fork_provider_is_mounted() {
    let workers = WorkersHandle::new(bounds());
    let (ctx, _agent, _d) = mounted(workers).await;

    let err = ForkTool
        .call(call(), cx(&ctx))
        .await
        .expect_err("there is no fork provider here");
    assert_eq!(
        err.kind,
        FailureClass::NotFound,
        "a composition without the row is a fact, not a transient block"
    );
    assert!(
        err.message.to_lowercase().contains("fork"),
        "the refusal names the kind: {}",
        err.message
    );
}
