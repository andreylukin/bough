//! The shared fixture: one kernel, one in-memory ledger, the `agents` seam, the SCRIPTED loop
//! Provider (P5-D1: both Providers dispatch `agent/wake-request`, so dormancy is proved against a
//! real driver rather than a stub) and the `dormancy` admission listener.
//!
//! Offline and hermetic: no model, no network, no clock the test does not pass in.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agent_loop_scripted::{
    resolve_script, ReplayEnv, ScriptedConfig, ScriptedFactory,
};
use bough_plugin_agents::{
    Agent, AgentFactory, AgentsHandle, CreateAgent, Delivery, MailClass, Message, Sender,
};
use bough_plugin_dormancy::{DormancyHandle, SleepRequest};
use bough_plugin_ledger::{AgentName, AgentRow, Cite, LedgerHandle, Ref, Step, StepQuery, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_projection::ProjectionHandle;
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use bough_plugin_rollups::Attribution;
use bough_plugin_tools::ToolsHandle;

/// Enough wakes for any case here; strict mode is off, so running out of script ends a wake
/// rather than failing a test about something else.
const SCRIPT: &str = r#"
wakes:
  - steps:
      - chunks:
          - { chunk: text, text: "caught up" }
          - { chunk: end, stop: end_turn }
  - steps:
      - chunks:
          - { chunk: text, text: "caught up" }
          - { chunk: end, stop: end_turn }
  - steps:
      - chunks:
          - { chunk: text, text: "caught up" }
          - { chunk: end, stop: end_turn }
  - steps:
      - chunks:
          - { chunk: text, text: "caught up" }
          - { chunk: end, stop: end_turn }
"#;

pub struct Tree {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub dormancy: DormancyHandle,
}

pub fn traj() -> TrajId {
    TrajId::new("lane/sol")
}
pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}
pub fn name(n: &str) -> AgentName {
    AgentName::new(n)
}
pub fn refs(rs: &[&str]) -> BTreeSet<Ref> {
    rs.iter().map(|r| Ref::new(*r)).collect()
}
pub fn cite(r: &str) -> Cite {
    Cite {
        r#ref: Ref::new(r),
        url: None,
    }
}

/// An adapter that answers anything with one sentence: the live driver needs SOMETHING to call.
struct OneLiner;

#[async_trait::async_trait]
impl bough_plugin_llm::LlmAdapter for OneLiner {
    fn name(&self) -> bough_plugin_llm::AdapterName {
        bough_plugin_llm::AdapterName::new("one-liner")
    }
    async fn start(
        &self,
        _req: Arc<bough_plugin_llm::LlmRequest>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> bough_plugin_llm::LlmStream {
        Box::pin(futures::stream::iter(vec![
            bough_plugin_llm::Chunk::TextDelta {
                text: "caught up".into(),
            },
            bough_plugin_llm::Chunk::End {
                stop: bough_plugin_llm::StopReason::EndTurn,
            },
        ]))
    }
}

/// A tree on the SCRIPTED loop Provider.
pub async fn tree(routing: &[&str], classes: &[&str]) -> Tree {
    tree_with("agent-loop-scripted", routing, classes).await
}

/// A tree on the LIVE loop Provider, with a one-line adapter behind it. Both Providers dispatch
/// `agent/wake-request` (P5-D1), so the same cases hold for both.
pub async fn live_tree(routing: &[&str], classes: &[&str]) -> Tree {
    tree_with("agent-loop", routing, classes).await
}

/// A tree with a `sol` row already in the ledger, carrying `routing_refs` and `wake_classes`.
pub async fn tree_with(which: &str, routing: &[&str], classes: &[&str]) -> Tree {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    for def in bough_plugin_agents::vocabulary::step_types()
        .into_iter()
        .chain(bough_plugin_tools::vocabulary::step_types())
        .chain(bough_plugin_dormancy::vocabulary::step_types())
    {
        ledger.0.register_step_type(def).expect("a fresh type");
    }
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct ThoughtText {
        text: String,
        step_index: u32,
    }
    drop(
        ledger.0.register_step_type(
            bough_plugin_ledger::StepTypeDef::of::<ThoughtText>("thought/text", "test")
                .class_rule(bough_plugin_ledger::ClassRule::Thought),
        ),
    );
    ledger
        .0
        .put_agent(AgentRow {
            name: name("sol"),
            traj: traj(),
            routing_refs: refs(routing),
            wake_classes: classes.iter().map(|c| c.to_string()).collect(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the row is written");

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
    let tools = ToolsHandle::with_limits(8, 5_000);
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());

    let factory: Arc<dyn AgentFactory> = if which == "agent-loop" {
        let llm = bough_plugin_llm::LlmHandle::new();
        llm.adapter(
            &ctx,
            bough_plugin_llm::AdapterSpec {
                name: bough_plugin_llm::AdapterName::new("one-liner"),
                matches: bough_plugin_llm::ModelMatch::Any,
                adapter: Arc::new(OneLiner) as Arc<dyn bough_plugin_llm::LlmAdapter>,
            },
        )
        .await
        .expect("the adapter registers");
        ctx.on_waterfall::<bough_plugin_llm::AgentRequest, _, _>(|mut call, next| async move {
            if call.call.model.is_empty() {
                call.call.model = "one-liner".into();
            }
            next.run(call).await
        })
        .await
        .expect("the policy stand-in registers");
        let cfg = bough_plugin_agent_loop::LoopConfig {
            drain_debounce_ms: 20,
            grace_deadline_ms: 200,
            default_max_tokens: 256,
            prompt_ver: "p5-test".into(),
            text_flush_ms: 0,
            repair_on_boot: false,
            status_drain_ms: 1000,
        };
        Arc::new(bough_plugin_agent_loop::LoopFactory::new(
            Arc::new(cfg.clone()),
            bough_plugin_agent_loop::wake::LoopDeps {
                ctx: ctx.clone(),
                ledger: ledger.clone(),
                projection,
                llm,
                tools,
                composition: "test".into(),
                cfg: Arc::new(cfg),
            },
        ))
    } else {
        let cfg = Arc::new(ScriptedConfig {
            transcript: None,
            wakes: Some(
                serde_yaml::from_str::<serde_json::Value>(SCRIPT).unwrap()["wakes"].clone(),
            ),
            strict: false,
        });
        let script = Arc::new(resolve_script(&cfg).expect("the script resolves"));
        Arc::new(ScriptedFactory::new(
            cfg,
            script.clone(),
            ReplayEnv {
                ctx: ctx.clone(),
                ledger: ledger.clone(),
                projection: Some(projection),
                script,
                strict: false,
                prompt_ver: "p5-test".into(),
                composition: "test".into(),
                default_max_tokens: 256,
                recorder: None,
                tools: Some(tools),
            },
        ))
    };
    agents
        .set_factory(&ctx, factory)
        .await
        .expect("the slot is free");

    let dormancy = DormancyHandle::new(ledger.clone(), agents.clone());
    bough_plugin_dormancy::register_admission(&ctx, &dormancy)
        .await
        .expect("the admission listener registers");

    Tree {
        ctx,
        ledger,
        agents,
        dormancy,
    }
}

impl Tree {
    /// Resume `sol` (the row already exists), so the scripted driver is attached to it.
    pub async fn sol(&self) -> Agent {
        let (agent, disposer) = self
            .agents
            .resume(bough_plugin_agents::ResumeAgent {
                name: name("sol"),
                setup: None,
                at: now(),
            })
            .await
            .expect("the resume commits");
        // The disposer is deliberately leaked: these tests read the chain of a live agent, and
        // dropping it would tear the driver down mid-assertion.
        std::mem::forget(disposer);
        agent
    }

    /// A fresh agent created from scratch (used where the test wants no prior chain).
    pub async fn create(&self, n: &str, t: TrajId) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(name(n), t, now()))
            .await
            .expect("the transaction commits");
        std::mem::forget(disposer);
        agent
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

    pub async fn steps_of(&self, kind: &str) -> Vec<Step> {
        self.steps()
            .await
            .into_iter()
            .filter(|s| s.kind.as_str() == kind)
            .collect()
    }

    /// Put `sol` to sleep with a cited reason.
    pub async fn sleep(&self) -> bough_plugin_dormancy::DormancyChange {
        self.dormancy
            .sleep(SleepRequest {
                agent: name("sol"),
                reason: "the lane is quiet".to_string(),
                by: Attribution::Andrey,
                cites: vec![cite("gh:o/r#1")],
                at: now(),
            })
            .await
            .expect("sleep commits")
    }
}

/// Ordinary delivered mail, with whatever refs the case needs.
pub fn ordinary(text: &str, rs: &[&str]) -> Delivery {
    Delivery {
        from: Sender::Collector("gh".into()),
        class: MailClass::Ordinary,
        subject: "an ordinary thing".into(),
        summary: text.into(),
        text: text.into(),
        cites: vec![cite("gh:o/r#1")],
        refs: refs(rs),
        at: now(),
    }
}

/// Wake-class delivered mail carrying `class:` refs.
pub fn wake_class(text: &str, rs: &[&str]) -> Delivery {
    Delivery {
        class: MailClass::Wake,
        ..ordinary(text, rs)
    }
}

/// A message from Andrey.
pub fn from_andrey(text: &str) -> Message {
    Message::waking(Sender::Andrey, "a question", text, now())
}

/// Let the driver's spawned tasks run to completion.
pub async fn settle(agent: &Agent) {
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    agent.when_idle().await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
}
