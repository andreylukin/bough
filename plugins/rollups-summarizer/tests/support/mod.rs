// ---- the fixture ---------------------------------------------------------------------------
//
// Offline by construction: `ledger-memory` for the store, `llm-replay` for the model. The only
// thing the summarizer is given that a real composition would not give it is the policy listener,
// which in the tree is `model-policy`'s prepend hop (P4-D3).

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    ActionId, Append, Cite, Class, LedgerHandle, Order, Ref, Rollup, RollupQuery, Seq, Step,
    StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_llm::{AdapterName, AdapterSpec, LlmAdapter, LlmHandle, ModelMatch};
use bough_plugin_llm_replay::{ReplayAdapter, ReplayConfig};
use bough_plugin_rollups::{Attribution, SealRequest, Summarizer};
use bough_plugin_rollups_summarizer::{
    bundle_config, step_types, RecapSummarizer, SummarizerConfig, SummarizerInner,
};
use chrono::{DateTime, Duration, TimeZone, Utc};

/// A model the vendored pricing catalog knows, so the bench can put dollars on the same numbers.
pub const MODEL: &str = "claude-haiku-4-5-20251001";

pub fn traj() -> TrajId {
    TrajId::new("lane/sol")
}

pub fn agent() -> bough_plugin_ledger::AgentName {
    bough_plugin_ledger::AgentName::new("sol")
}

pub fn base() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap()
}

/// The row's values, tightened for a test: a short lag and a generous call budget, so a suite can
/// seal a small trajectory in one pass without the bundle's production caution.
pub fn cfg() -> SummarizerConfig {
    SummarizerConfig {
        seal_lag_steps: 2,
        max_calls_per_pass: 64,
        ..bundle_config()
    }
}

pub struct Fx {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub llm: LlmHandle,
    pub summarizer: RecapSummarizer,
    pub cfg: SummarizerConfig,
}

/// `rounds` identical recap answers, each carrying provider-shaped usage (P4-D10).
pub fn transcript(rounds: usize) -> serde_json::Value {
    serde_json::Value::Array(
        (0..rounds)
            .map(|i| {
                serde_json::json!({
                    "chunks": [
                        { "type": "text", "text": format!(
                            "Recap {i}: the episode's work, and what it was for.\n\
                             ## Open question\nWhether the next step holds.") },
                        { "type": "usage", "input_tokens": 1_200, "output_tokens": 180 },
                        { "type": "end", "stop": "end_turn" }
                    ]
                })
            })
            .collect(),
    )
}

pub async fn fx(cfg: SummarizerConfig, rounds: usize) -> Fx {
    fx_with(cfg, transcript(rounds)).await
}

pub async fn fx_with(cfg: SummarizerConfig, rounds: serde_json::Value) -> Fx {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    for def in bough_plugin_agents::vocabulary::step_types()
        .into_iter()
        .chain(step_types())
    {
        ledger.0.register_step_type(def).expect("a fresh step type");
    }
    let llm = LlmHandle::new();
    let replay_cfg = Arc::new(ReplayConfig {
        transcript: None,
        rounds: Some(rounds),
        strict: true,
        models: "*".to_string(),
        delay_ms: 0,
    });
    let parsed = ReplayAdapter::load(&replay_cfg).expect("the inline transcript parses");
    llm.adapter(
        &ctx,
        AdapterSpec {
            name: AdapterName::new("llm-replay"),
            matches: ModelMatch::Any,
            adapter: Arc::new(ReplayAdapter::new(replay_cfg, parsed)) as Arc<dyn LlmAdapter>,
        },
    )
    .await
    .expect("the adapter registers");
    // The stand-in for `model-policy`: the summarizer deliberately leaves `call.model` empty, so
    // SOMETHING must choose. In the tree that is the policy row, and terra is what it chooses for
    // an unattended wake (P4-D3).
    ctx.on_waterfall::<bough_plugin_llm::AgentRequest, _, _>(|mut call, next| async move {
        assert!(
            !call.facts.answers_andrey,
            "a governance pass must never present itself as answering Andrey"
        );
        assert_eq!(call.facts.wake_kind, bough_plugin_llm::WakeKind::Scheduled);
        if call.call.model.is_empty() {
            call.call.model = MODEL.to_string();
        }
        next.run(call).await
    })
    .await
    .expect("the policy listener registers");

    Fx::over(ctx, ledger, llm, cfg)
}

/// The same fixture with the REAL Anthropic adapter. Used by the one `#[ignore]`d live case.
pub async fn fx_live(cfg: SummarizerConfig) -> Fx {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    for def in bough_plugin_agents::vocabulary::step_types()
        .into_iter()
        .chain(step_types())
    {
        ledger.0.register_step_type(def).expect("a fresh step type");
    }
    let llm = LlmHandle::new();
    llm.adapter(
        &ctx,
        AdapterSpec {
            name: AdapterName::new("llm-anthropic"),
            matches: ModelMatch::parse("claude-*"),
            adapter: Arc::new(bough_plugin_llm_anthropic::AnthropicAdapter::new(Arc::new(
                bough_plugin_llm_anthropic::AnthropicConfig {
                    models: "claude-*".into(),
                    api_key_env: "ANTHROPIC_API_KEY".into(),
                    base_url: None,
                    request_timeout_ms: 60_000,
                },
            ))) as Arc<dyn LlmAdapter>,
        },
    )
    .await
    .expect("the adapter registers");
    ctx.on_waterfall::<bough_plugin_llm::AgentRequest, _, _>(|mut call, next| async move {
        if call.call.model.is_empty() {
            call.call.model = MODEL.to_string();
        }
        next.run(call).await
    })
    .await
    .expect("the policy listener registers");
    Fx::over(ctx, ledger, llm, cfg)
}

impl Fx {
    fn over(ctx: Context, ledger: LedgerHandle, llm: LlmHandle, cfg: SummarizerConfig) -> Fx {
        let summarizer = RecapSummarizer(Arc::new(SummarizerInner {
            ctx: ctx.clone(),
            cfg: Arc::new(cfg.clone()),
            ledger: ledger.clone(),
            llm: llm.clone(),
            agents: None,
            composition: "test-composition".into(),
        }));
        Fx {
            ctx,
            ledger,
            llm,
            summarizer,
            cfg,
        }
    }

    /// A SECOND provider over the same ledger and the same replay adapter, with a different row
    /// config. This is what a `prompt_ver` bump looks like from the outside: a new composition of
    /// the same row over a store that already holds sealed blocks.
    pub fn reconfigured(&self, cfg: SummarizerConfig) -> RecapSummarizer {
        RecapSummarizer(Arc::new(SummarizerInner {
            ctx: self.ctx.clone(),
            cfg: Arc::new(cfg),
            ledger: self.ledger.clone(),
            llm: self.llm.clone(),
            agents: None,
            composition: "test-composition".into(),
        }))
    }

    pub fn request(&self, at: DateTime<Utc>) -> SealRequest {
        SealRequest {
            agent: agent(),
            traj: traj(),
            at,
            upto: None,
            max_calls: None,
            attribution: Attribution::System,
        }
    }

    pub async fn seal(&self) -> bough_plugin_rollups::SealReport {
        self.summarizer
            .seal(&self.request(base() + Duration::days(1)))
            .await
            .expect("a pass runs")
    }

    /// The `agents` row identity renders from (§3). A digest rebuild repoints it.
    pub async fn put_agent(&self) {
        self.ledger
            .0
            .put_agent(bough_plugin_ledger::AgentRow {
                name: agent(),
                traj: traj(),
                routing_refs: Default::default(),
                wake_classes: Default::default(),
                model_override: None,
                tick_floor: None,
                digest_rollup: None,
            })
            .await
            .expect("the agent row writes");
    }

    pub async fn steps(&self) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj()],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("a read")
    }

    pub async fn rollups(&self) -> Vec<Rollup> {
        self.ledger
            .0
            .rollups(&RollupQuery {
                trajs: vec![traj()],
                include_superseded: true,
                ..Default::default()
            })
            .await
            .expect("a read")
    }

    pub async fn head(&self) -> Seq {
        self.ledger
            .0
            .head_seq(&traj())
            .await
            .expect("a read")
            .unwrap_or(Seq(0))
    }

    /// `wakes` episodes of `per_wake` steps: minutes apart inside an episode, `gap_minutes` hours
    /// apart between them, so the episode cut lands on the wake boundary the way a real day does.
    pub async fn seed(&self, wakes: usize, per_wake: usize) {
        for w in 0..wakes {
            let start = base() + Duration::minutes((w as i64) * 600);
            for i in 0..per_wake {
                let at = start + Duration::minutes(i as i64);
                if i % 3 == 2 {
                    // Evidence, so the block has domain refs to be notable for.
                    self.append(
                        w,
                        at,
                        "action/done",
                        Class::Evidence,
                        serde_json::json!({
                            "action": ActionId::new(format!("a{w}-{i}")),
                            "status": "done",
                            "artifact": null
                        }),
                        vec![Cite {
                            r#ref: Ref::new(format!("gh:o/r#{w}")),
                            url: None,
                        }],
                    )
                    .await;
                } else {
                    self.append(
                        w,
                        at,
                        "thought/text",
                        Class::Thought,
                        serde_json::json!({
                            "text": format!("episode {w} step {i}: reading and deciding"),
                            "step_index": 0
                        }),
                        vec![],
                    )
                    .await;
                }
            }
        }
    }

    pub async fn append(
        &self,
        wake: usize,
        at: DateTime<Utc>,
        kind: &str,
        class: Class,
        body: serde_json::Value,
        cites: Vec<Cite>,
    ) -> Step {
        self.ledger
            .0
            .append(Append {
                traj: traj(),
                wake: WakeId::new(format!("w{wake}")),
                kind: StepType::new(kind),
                class,
                body,
                cites,
                at,
                id: None,
            })
            .await
            .expect("the step appends")
    }
}
