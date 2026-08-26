//! §2.5 / P3-D16: `AgentDriver::wake_now` is part of the SEAM, so BOTH loop Providers implement
//! it and both honour the same two-sided contract — nothing to do is `Nothing` and no ledger row;
//! something to do is exactly ONE wake, reported by id.
//!
//! This suite lives here because this is the one crate that can see both drivers at once.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agent_loop::wake::LoopDeps;
use bough_plugin_agent_loop::{LoopConfig, LoopFactory};
use bough_plugin_agent_loop_scripted::{
    resolve_script, ReplayEnv, ScriptedConfig, ScriptedFactory,
};
use bough_plugin_agents::{
    AgentFactory, AgentsHandle, CreateAgent, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::{
    AgentName, Append, Cite, Class, LedgerHandle, Ref, Step, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_llm::{
    AdapterName, AdapterSpec, Chunk, LlmAdapter, LlmHandle, LlmRequest, LlmStream, ModelMatch,
    StopReason,
};
use bough_plugin_projection::ProjectionHandle;
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use bough_plugin_tools::ToolsHandle;
use tokio_util::sync::CancellationToken;

const SCRIPT: &str = r#"
wakes:
  - steps:
      - chunks:
          - { chunk: text, text: "caught up" }
          - { chunk: end, stop: end_turn }
"#;

fn traj() -> TrajId {
    TrajId::new("lane/sol")
}
fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

/// An adapter that answers anything with one sentence: the live driver needs SOMETHING to call.
struct OneLiner;

#[async_trait::async_trait]
impl LlmAdapter for OneLiner {
    fn name(&self) -> AdapterName {
        AdapterName::new("one-liner")
    }
    async fn start(&self, _req: Arc<LlmRequest>, _cancel: CancellationToken) -> LlmStream {
        Box::pin(futures::stream::iter(vec![
            Chunk::TextDelta {
                text: "caught up".into(),
            },
            Chunk::End {
                stop: StopReason::EndTurn,
            },
        ]))
    }
}

struct Tree {
    ctx: Context,
    ledger: LedgerHandle,
    agents: AgentsHandle,
}

/// One kernel, one in-memory ledger, one `agents` seam — and whichever factory the case names.
async fn tree(which: &str) -> Tree {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    for def in bough_plugin_agents::vocabulary::step_types()
        .into_iter()
        .chain(bough_plugin_tools::vocabulary::step_types())
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
    llm.adapter(
        &ctx,
        AdapterSpec {
            name: AdapterName::new("one-liner"),
            matches: ModelMatch::Any,
            adapter: Arc::new(OneLiner) as Arc<dyn LlmAdapter>,
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
    let tools = ToolsHandle::with_limits(8, 5_000);
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());

    let factory: Arc<dyn AgentFactory> = match which {
        "agent-loop" => {
            let cfg = LoopConfig {
                drain_debounce_ms: 20,
                grace_deadline_ms: 200,
                default_max_tokens: 256,
                prompt_ver: "p3-test".into(),
                text_flush_ms: 0,
                repair_on_boot: false,
                status_drain_ms: 1000,
            };
            Arc::new(LoopFactory::new(
                Arc::new(cfg.clone()),
                LoopDeps {
                    ctx: ctx.clone(),
                    ledger: ledger.clone(),
                    projection,
                    llm,
                    tools,
                    composition: "test".into(),
                    cfg: Arc::new(cfg),
                },
            ))
        }
        _ => {
            let cfg = Arc::new(ScriptedConfig {
                transcript: None,
                wakes: Some(
                    serde_yaml::from_str::<serde_json::Value>(SCRIPT).unwrap()["wakes"].clone(),
                ),
                strict: true,
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
                    strict: true,
                    prompt_ver: "p3-test".into(),
                    composition: "test".into(),
                    default_max_tokens: 256,
                    recorder: None,
                    tools: Some(tools),
                },
            ))
        }
    };
    agents
        .set_factory(&ctx, factory)
        .await
        .expect("the slot is free");
    Tree {
        ctx,
        ledger,
        agents,
    }
}

async fn steps(t: &Tree) -> Vec<Step> {
    t.ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            ..Default::default()
        })
        .await
        .expect("a read")
}

async fn wake_starts(t: &Tree) -> Vec<Step> {
    steps(t)
        .await
        .into_iter()
        .filter(|s| s.kind.as_str() == "wake/start")
        .collect()
}

/// Unconsumed ordinary mail, delivered before this process started: the state a restart leaves.
async fn seed_queued_mail(t: &Tree) {
    t.ledger
        .0
        .append(Append {
            traj: traj(),
            wake: WakeId::new("wake:outside"),
            kind: StepType::new("mail/delivered"),
            class: Class::Evidence,
            body: serde_json::json!({
                "class": "ordinary",
                "from": "collector:github",
                "subject": "CI is red",
                "summary": "the delegate test failed again",
            }),
            cites: vec![Cite {
                r#ref: Ref::new("gh:bough/bough#12"),
                url: None,
            }],
            at: now(),
            id: None,
        })
        .await
        .expect("the seed appends");
}

#[tokio::test]
async fn both_drivers_implement_wake_now() {
    for which in ["agent-loop", "agent-loop-scripted"] {
        // ---- nothing to do -------------------------------------------------------------
        let t = tree(which).await;
        let (agent, _d) = t
            .agents
            .create(CreateAgent::resident(AgentName::new("sol"), traj(), now()))
            .await
            .expect("the transaction commits");
        let before = steps(&t).await.len();
        let req = agent
            .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
            .await;
        assert_eq!(
            req,
            WakeRequest::Nothing,
            "{which}: an agent with nothing queued has nothing to catch up on"
        );
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(
            steps(&t).await.len(),
            before,
            "{which}: and nothing was appended for asking"
        );
        drop(_d);
        drop(t);

        // ---- something to do -----------------------------------------------------------
        let t = tree(which).await;
        seed_queued_mail(&t).await;
        let (agent, _d) = t
            .agents
            .create(CreateAgent::resident(AgentName::new("sol"), traj(), now()))
            .await
            .expect("the transaction commits");
        let req = agent
            .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
            .await;
        let wake = match req {
            WakeRequest::Started(w) => w,
            WakeRequest::Nothing => panic!("{which}: queued mail is something to catch up on"),
        };
        for _ in 0..200 {
            if !wake_starts(&t).await.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let starts = wake_starts(&t).await;
        assert_eq!(starts.len(), 1, "{which}: exactly one wake, not two");
        assert_eq!(
            starts[0].wake, wake,
            "{which}: and it is the wake whose id was reported"
        );
        agent.when_idle().await;
        drop(_d);
        let _ = t.ctx;
    }
}
