//! The fixture the two fork suites share: an in-memory ledger, the REAL projection assembler, the
//! REAL loop in the agent factory, a scripted adapter, and the `workers` seam with the fork
//! provider (and, where a case needs it, the spawn provider) mounted on it.
//!
//! DEVIATION from the WP-6 file list, named on purpose: the plan lists two test files and no
//! support module. Two copies of this harness would be two places for it to drift, and
//! `plugins/agent-loop/tests/support` and `plugins/projection-assembler/tests/support` are the
//! precedents.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agent_loop::wake::LoopDeps;
use bough_plugin_agent_loop::{LoopConfig, LoopFactory};
use bough_plugin_agents::{
    Agent, AgentDisposer, Agents, AgentsHandle, CreateAgent, MailClass, Message, Sender,
};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Class, Ledger, LedgerHandle, Step, StepQuery, StepType, TrajId,
    WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_llm::{
    AdapterName, AdapterSpec, Chunk, LlmAdapter, LlmHandle, LlmRequest, LlmStream, ModelMatch,
    StopReason, ToolCallId, ToolName,
};
use bough_plugin_projection::{Projection, ProjectionHandle};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use bough_plugin_tools::{Tools, ToolsHandle};
use bough_plugin_workers::{Bounds, Workers, WorkersHandle};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

pub fn parent_traj() -> TrajId {
    TrajId::new("lane/sol")
}

pub fn parent() -> AgentName {
    AgentName::new("sol")
}

pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

pub struct Fixture {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub tools: ToolsHandle,
    pub workers: WorkersHandle,
    pub adapter: Arc<ScriptedAdapter>,
    pub assembler: Arc<Assembler>,
}

pub fn assembler_config() -> AssemblerConfig {
    AssemblerConfig {
        budget_tokens: 100_000,
        headroom: 1.0,
        tail_steps: 40,
        tail_floor_steps: 8,
        mail_newest_n: 5,
        max_tiers: 3,
        file_view_dir: std::path::PathBuf::from("/unused-by-these-tests"),
    }
}

impl Fixture {
    pub async fn mounted(bounds: Bounds) -> Fixture {
        let ctx = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
        for def in bough_plugin_agents::vocabulary::step_types()
            .into_iter()
            .chain(bough_plugin_tools::vocabulary::step_types())
            .chain(bough_plugin_workers::vocabulary::step_types())
            .chain(bough_plugin_worker_fork::vocabulary::step_types())
        {
            ledger.0.register_step_type(def).expect("a fresh type");
        }
        let assembler = Assembler::new(Arc::new(assembler_config()), ledger.clone(), ctx.clone());
        let projection = ProjectionHandle(assembler.clone() as Arc<_>);

        let llm = LlmHandle::new();
        let adapter = Arc::new(ScriptedAdapter::default());
        llm.adapter(
            &ctx,
            AdapterSpec {
                name: AdapterName::new("scripted"),
                matches: ModelMatch::Any,
                adapter: adapter.clone() as Arc<dyn LlmAdapter>,
            },
        )
        .await
        .expect("the adapter registers");
        // The stand-in for `model-policy`.
        ctx.on_waterfall::<bough_plugin_llm::AgentRequest, _, _>(|mut call, next| async move {
            if call.call.model.is_empty() {
                call.call.model = "scripted-model".into();
            }
            next.run(call).await
        })
        .await
        .expect("the policy listener registers");

        let tools = ToolsHandle::with_limits(8, 5_000);
        let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
        let cfg = LoopConfig {
            drain_debounce_ms: 20,
            grace_deadline_ms: 200,
            default_max_tokens: 256,
            prompt_ver: "p5-fork-test".into(),
            text_flush_ms: 0,
            repair_on_boot: false,
            status_drain_ms: 1000,
        };
        let deps = LoopDeps {
            ctx: ctx.clone(),
            ledger: ledger.clone(),
            projection: projection.clone(),
            llm: llm.clone(),
            tools: tools.clone(),
            composition: "fork-test".into(),
            cfg: Arc::new(cfg.clone()),
        };
        agents
            .set_factory(&ctx, Arc::new(LoopFactory::new(Arc::new(cfg), deps)))
            .await
            .expect("the slot is free");

        let workers = WorkersHandle::new(bounds);
        // The seam and its dependencies, as the composition provides them: `ForkProvider` reaches
        // all four through its own context.
        std::mem::forget(ctx.provide::<Ledger>(ledger.clone()).await.expect("ledger"));
        std::mem::forget(
            ctx.provide::<Projection>(projection)
                .await
                .expect("projection"),
        );
        std::mem::forget(ctx.provide::<Tools>(tools.clone()).await.expect("tools"));
        std::mem::forget(ctx.provide::<Agents>(agents.clone()).await.expect("agents"));
        std::mem::forget(
            ctx.provide::<Workers>(workers.clone())
                .await
                .expect("workers"),
        );

        Fixture {
            ctx,
            ledger,
            agents,
            tools,
            workers,
            adapter,
            assembler,
        }
    }

    /// Mount the fork provider. Returns nothing: the effect lives as long as the fixture's ctx.
    pub async fn mount_fork(&self, max_steps: u32) {
        self.workers
            .provider(
                &self.ctx,
                Arc::new(bough_plugin_worker_fork::ForkProvider::with_ctx(
                    self.ctx.clone(),
                    Arc::new(bough_plugin_worker_fork::ForkConfig { max_steps }),
                )),
            )
            .await
            .expect("the fork provider mounts");
    }

    /// Mount the SPAWN provider too, so a case can prove a fork and a spawn share one bound.
    pub async fn mount_spawn(&self, max_steps: u32) {
        self.workers
            .provider(
                &self.ctx,
                Arc::new(bough_plugin_worker_spawn::SpawnProvider::with_ctx(
                    self.ctx.clone(),
                    Arc::new(bough_plugin_worker_spawn::SpawnConfig {
                        ask_mode: bough_plugin_workers::AskMode::End,
                        max_steps,
                    }),
                )),
            )
            .await
            .expect("the spawn provider mounts");
    }

    /// The parent: an agents row (the fork provider reads it) and a live resident agent.
    pub async fn parent_agent(&self) -> (Agent, AgentDisposer) {
        self.ledger
            .0
            .put_agent(AgentRow {
                name: parent(),
                traj: parent_traj(),
                routing_refs: BTreeSet::new(),
                wake_classes: BTreeSet::new(),
                model_override: None,
                tick_floor: None,
                digest_rollup: None,
            })
            .await
            .expect("agents is mutable config");
        self.agents
            .create(CreateAgent::resident(parent(), parent_traj(), now()))
            .await
            .expect("the creation transaction commits")
    }

    pub async fn steps_of(&self, traj: &TrajId) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj.clone()],
                ..Default::default()
            })
            .await
            .expect("a read")
    }

    /// Append a row directly, for a case that needs a chain shape the loop would not produce.
    pub async fn append(&self, traj: &TrajId, wake: &str, kind: &str, body: serde_json::Value) {
        self.ledger
            .0
            .append(Append {
                traj: traj.clone(),
                wake: WakeId::new(wake),
                kind: StepType::new(kind),
                class: Class::Thought,
                body,
                cites: Vec::new(),
                at: now(),
                id: None,
            })
            .await
            .unwrap_or_else(|e| panic!("append {kind}: {e}"));
    }

    /// Wake the parent with one message from Andrey and wait for the wake to close.
    pub async fn run_parent_wake(&self, agent: &Agent, text: &str) {
        let mut m = Message::new(Sender::Andrey, "a question", text, now());
        m.class = MailClass::Wake;
        agent.followup(m).await.expect("the message splices");
        self.wait_for_wake_ends(&parent_traj(), 1).await;
    }

    pub async fn wait_for_wake_ends(&self, traj: &TrajId, n: usize) -> Vec<Step> {
        for _ in 0..600 {
            let steps = self.steps_of(traj).await;
            if steps
                .iter()
                .filter(|s| s.kind.as_str() == "wake/end")
                .count()
                >= n
            {
                return steps;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let kinds: Vec<&str> = Vec::new();
        panic!("timed out waiting for {n} wake/end on {traj}; saw {kinds:?}");
    }
}

/// An adapter that answers from a script, one round per call, the last repeating.
#[derive(Default)]
pub struct ScriptedAdapter {
    rounds: Mutex<Vec<Vec<Chunk>>>,
    pub seen: Mutex<Vec<LlmRequest>>,
}

impl ScriptedAdapter {
    pub fn script(&self, rounds: Vec<Vec<Chunk>>) {
        *self.rounds.lock() = rounds;
    }
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.seen.lock().clone()
    }
}

/// One round: some text, then `end_turn`.
pub fn says(text: &str) -> Vec<Chunk> {
    vec![
        Chunk::TextDelta { text: text.into() },
        Chunk::End {
            stop: StopReason::EndTurn,
        },
    ]
}

/// One round: call `report` with this summary and one externally cited claim, then stop.
pub fn reports(summary: &str, claim: &str, cite: &str) -> Vec<Chunk> {
    vec![
        Chunk::ToolCall {
            id: ToolCallId::new("tc-report"),
            name: ToolName::new("report"),
            input: serde_json::json!({
                "summary": summary,
                "claims": [{ "text": claim, "cites": [{ "ref": cite }] }],
            }),
        },
        Chunk::End {
            stop: StopReason::ToolUse,
        },
    ]
}

#[async_trait::async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn name(&self) -> AdapterName {
        AdapterName::new("scripted")
    }
    async fn start(&self, req: Arc<LlmRequest>, _cancel: CancellationToken) -> LlmStream {
        self.seen.lock().push((*req).clone());
        let chunks = {
            let mut rounds = self.rounds.lock();
            if rounds.len() > 1 {
                rounds.remove(0)
            } else {
                rounds.first().cloned().unwrap_or_else(|| says("ok"))
            }
        };
        Box::pin(futures::stream::iter(chunks))
    }
}
