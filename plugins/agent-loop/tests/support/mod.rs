//! The shared fixture: a root kernel context, an in-memory ledger, the real projection
//! assembler, a scripted llm adapter and the REAL loop in the factory slot. Every assertion in
//! these suites is made against the durable ledger, never against the loop's private state.

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agent_loop::wake::LoopDeps;
use bough_plugin_agent_loop::{LoopConfig, LoopFactory};
use bough_plugin_agents::{Agent, AgentsHandle, CreateAgent, Message, Sender};
use bough_plugin_ledger::{AgentName, LedgerHandle, Step, StepQuery, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_llm::{
    AdapterName, AdapterSpec, Chunk, LlmAdapter, LlmHandle, LlmRequest, LlmStream, ModelMatch,
    StopReason,
};
use bough_plugin_projection::ProjectionHandle;
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use bough_plugin_tools::ToolsHandle;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

pub struct Fixture {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub tools: ToolsHandle,
    pub llm: LlmHandle,
    pub adapter: Arc<ScriptedAdapter>,
}

pub fn traj() -> TrajId {
    TrajId::new("lane/sol")
}
pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}
pub fn andrey(text: &str) -> Message {
    let mut m = Message::new(Sender::Andrey, "a question", text, now());
    m.class = bough_plugin_agents::MailClass::Wake;
    m
}
pub fn ordinary(text: &str) -> Message {
    Message::new(Sender::Collector("github".into()), "ci", text, now())
}

/// The config every test uses: no debounce worth waiting for, no flush delay, repair off (the
/// repair suite runs it explicitly).
pub fn config() -> LoopConfig {
    LoopConfig {
        drain_debounce_ms: 20,
        grace_deadline_ms: 200,
        default_max_tokens: 256,
        prompt_ver: "p2.1-test".into(),
        text_flush_ms: 0,
        repair_on_boot: false,
        status_drain_ms: 1000,
    }
}

impl Fixture {
    pub async fn mounted() -> Fixture {
        Fixture::with_config(config()).await
    }

    pub async fn with_config(cfg: LoopConfig) -> Fixture {
        let ctx = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
        // The step types the loop writes are owned by `agents` and `tools`; in the tree their
        // rows declare them, so the fixture declares them by hand.
        for def in bough_plugin_agents::vocabulary::step_types()
            .into_iter()
            .chain(bough_plugin_tools::vocabulary::step_types())
        {
            ledger.0.register_step_type(def).expect("a fresh type");
        }
        let projection = ProjectionHandle(Assembler::new(
            Arc::new(AssemblerConfig {
                budget_tokens: 100_000,
                headroom: 0.6,
                tail_steps: 40,
                tail_floor_steps: 8,
                mail_newest_n: 5,
                max_tiers: 3,
                file_view_dir: std::path::PathBuf::from("/tmp/bough-test-views"),
            }),
            ledger.clone(),
            ctx.clone(),
        ) as Arc<_>);
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
        // The stand-in for `model-policy`: the loop deliberately leaves `call.model` empty, so
        // SOMETHING must choose the model on `agent/request`. In the tree that is the policy row.
        ctx.on_waterfall::<bough_plugin_llm::AgentRequest, _, _>(|mut call, next| async move {
            if call.call.model.is_empty() {
                call.call.model = "scripted-model".into();
            }
            next.run(call).await
        })
        .await
        .expect("the policy listener registers");

        let tools = ToolsHandle::new();
        let agents = AgentsHandle::new(ctx.clone(), ledger.clone());

        let deps = LoopDeps {
            ctx: ctx.clone(),
            ledger: ledger.clone(),
            projection,
            llm: llm.clone(),
            tools: tools.clone(),
            composition: "test-composition".into(),
            cfg: Arc::new(cfg.clone()),
        };
        agents
            .set_factory(&ctx, Arc::new(LoopFactory::new(Arc::new(cfg), deps)))
            .await
            .expect("the slot is free");

        Fixture {
            ctx,
            ledger,
            agents,
            tools,
            llm,
            adapter,
        }
    }

    pub async fn agent(&self, name: &str) -> (Agent, bough_plugin_agents::AgentDisposer) {
        self.agents
            .create(CreateAgent::resident(AgentName::new(name), traj(), now()))
            .await
            .expect("the creation transaction commits")
    }

    pub async fn steps(&self) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj()],
                ..Default::default()
            })
            .await
            .expect("a read")
    }

    pub async fn kinds(&self) -> Vec<String> {
        self.steps()
            .await
            .into_iter()
            .map(|s| s.kind.as_str().to_string())
            .collect()
    }

    /// Wait until `n` `wake/end` steps exist, or give up. Durable facts only: a wake is over when
    /// the ledger says it is, never when a flag says so.
    pub async fn wait_for_wake_ends(&self, n: usize) -> Vec<Step> {
        for _ in 0..400 {
            let steps = self.steps().await;
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
        panic!(
            "timed out waiting for {n} wake/end step(s); saw {:?}",
            self.kinds().await
        );
    }

    pub async fn wait_for_kind(&self, kind: &str) -> Step {
        for _ in 0..400 {
            if let Some(s) = self
                .steps()
                .await
                .into_iter()
                .find(|s| s.kind.as_str() == kind)
            {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!(
            "timed out waiting for a {kind} step; saw {:?}",
            self.kinds().await
        );
    }
}

/// An adapter that answers from a script: one `Vec<Chunk>` per call, in order. The last script
/// repeats, so a test that only cares about the first round says so once.
#[derive(Default)]
pub struct ScriptedAdapter {
    rounds: Mutex<Vec<Vec<Chunk>>>,
    /// Every request the adapter was actually handed: V4's evidence.
    pub seen: Mutex<Vec<LlmRequest>>,
    /// Held open to make a round hang until the test releases it.
    pub hold: Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// A message the MODEL sees that the ledger never recorded: V4's planted side channel.
    pub inject: Mutex<Option<bough_plugin_llm::LlmMessage>>,
}

impl ScriptedAdapter {
    pub fn script(&self, rounds: Vec<Vec<Chunk>>) {
        *self.rounds.lock() = rounds;
    }
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.seen.lock().clone()
    }
}

/// The one-round shorthand: some text, then `end_turn`.
pub fn says(text: &str) -> Vec<Chunk> {
    vec![
        Chunk::TextDelta { text: text.into() },
        Chunk::End {
            stop: StopReason::EndTurn,
        },
    ]
}

#[async_trait::async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn name(&self) -> AdapterName {
        AdapterName::new("scripted")
    }
    async fn start(&self, req: Arc<LlmRequest>, _cancel: CancellationToken) -> LlmStream {
        let mut saw = (*req).clone();
        if let Some(extra) = self.inject.lock().clone() {
            saw.messages.push(extra);
        }
        self.seen.lock().push(saw);
        let chunks = {
            let mut rounds = self.rounds.lock();
            if rounds.len() > 1 {
                rounds.remove(0)
            } else {
                rounds.first().cloned().unwrap_or_else(|| says("ok"))
            }
        };
        let hold = self.hold.lock().clone();
        Box::pin(futures::stream::unfold(
            (chunks.into_iter(), hold),
            |(mut it, hold)| async move {
                if let Some(h) = &hold {
                    // The first chunk waits: a test that needs a round to be IN FLIGHT holds it
                    // here and releases it when it has made its point.
                    h.notified().await;
                }
                it.next().map(|c| (c, (it, None)))
            },
        ))
    }
}

/// A tool whose result concludes the wake (§5).
pub fn concluding_tool() -> bough_plugin_tools::ToolSpec {
    use bough_plugin_tools::{
        RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome, ToolScope, ToolSpec,
    };

    struct Finish;
    #[async_trait::async_trait]
    impl Tool for Finish {
        fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
            true
        }
        async fn call(
            &self,
            _call: Arc<ToolCall>,
            _cx: ToolCx,
        ) -> Result<ToolOutcome, ToolFailure> {
            Ok(ToolOutcome {
                content: "finished".into(),
                value: None,
                cites: vec![],
                concludes_wake: true,
            })
        }
    }

    ToolSpec {
        name: bough_plugin_llm::ToolName::new("finish"),
        description: "end the wake".into(),
        input_schema: schemars::json_schema!({ "type": "object" }),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool: Arc::new(Finish),
    }
}

/// A trivial tool: it echoes its argument, so a multi-step wake has something real to fold.
pub fn echo_tool() -> bough_plugin_tools::ToolSpec {
    use bough_plugin_tools::{
        RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome, ToolScope, ToolSpec,
    };

    struct Echo;
    #[async_trait::async_trait]
    impl Tool for Echo {
        fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
            true
        }
        async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
            Ok(ToolOutcome {
                content: call.args["text"].as_str().unwrap_or("").to_string(),
                value: None,
                cites: vec![],
                concludes_wake: false,
            })
        }
    }

    ToolSpec {
        name: bough_plugin_llm::ToolName::new("echo"),
        description: "echo the text back".into(),
        input_schema: schemars::json_schema!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
        }),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool: Arc::new(Echo),
    }
}

/// The recorded requests belonging to THESE steps. The recorder is process-wide (it is read by an
/// invariant that sees the whole tree), and cargo runs the tests of one binary in one process, so
/// a suite filters to its own wakes rather than assuming it is alone.
pub fn recorded_for(steps: &[Step]) -> Vec<bough_plugin_agent_loop::invariant::SentRequest> {
    let wakes: std::collections::BTreeSet<String> =
        steps.iter().map(|s| s.wake.to_string()).collect();
    bough_plugin_agent_loop::invariant::seen()
        .into_iter()
        .filter(|s| wakes.contains(&s.wake.to_string()))
        .collect()
}
