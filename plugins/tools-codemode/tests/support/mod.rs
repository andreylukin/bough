//! Shared fixtures for the `tools-codemode` integration tests.
#![allow(dead_code, unused_imports)]

// ---- fixtures -------------------------------------------------------------------------------
//
// The engine here is a SCRIPT interpreter, not JavaScript: one line per step
// (`log <text>` / `call <global> <json-args>` / `throw <message>`). It exercises the consumer —
// the mirror, the sub-steps, the console tee and the outcome mapping — without tying this crate's
// tests to the QuickJS provider, which has its own.

use std::collections::BTreeMap;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_js::{Caps, JsEngine, JsError, JsHandle, Program, Run as JsRun};
use bough_plugin_ledger::{
    AgentName, AgentRow, Class, LedgerHandle, Order, Step, StepQuery, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tools::{
    RenderIntent, Tool, ToolCall, ToolCallId, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, ToolsHandle,
};
use bough_plugin_tools_codemode::conceal::Concealment;
use bough_plugin_tools_codemode::{CodemodeConfig, ConcealMode};

/// Every preflight this binary has run, as `(src, bound)`. `Run::call` must hand the engine the
/// names it is about to inject; before it did, the engine's shadowed-binding diagnostic was
/// unreachable. It is a LOG and not a single slot because the test binary runs its cases
/// concurrently: a "last one wins" cell records whichever program happened to finish last.
pub static PREFLIGHTS: parking_lot::Mutex<Vec<(String, Vec<String>)>> =
    parking_lot::Mutex::new(Vec::new());

/// The roster the preflight of `src` was given. Panics if that program was never preflighted.
pub fn preflighted_with(src: &str) -> Vec<String> {
    let log = PREFLIGHTS.lock();
    log.iter()
        .find(|(s, _)| s == src)
        .map(|(_, b)| b.clone())
        .unwrap_or_else(|| panic!("`{src}` was never preflighted; log: {log:?}"))
}

pub struct ScriptEngine;

#[async_trait::async_trait]
impl JsEngine for ScriptEngine {
    fn name(&self) -> &'static str {
        "script"
    }

    async fn check(&self, src: &str, _caps: bough_plugin_js::Caps) -> Result<(), JsError> {
        if src.contains("!!syntax") {
            return Err(JsError::Syntax {
                message: "unterminated string".to_string(),
                line: Some(1),
                col: None,
            });
        }
        Ok(())
    }

    /// Records the roster the consumer preflighted with, and names a shadowed binding — which is
    /// what the real engine's `preflight::syntax_error_message` does with `bound`.
    async fn check_bound(
        &self,
        src: &str,
        caps: bough_plugin_js::Caps,
        bound: &[String],
    ) -> Result<(), JsError> {
        PREFLIGHTS.lock().push((src.to_string(), bound.to_vec()));
        if let Some(rest) = src.split("!!shadow ").nth(1) {
            let name = rest.lines().next().unwrap_or_default().trim().to_string();
            if bound.contains(&name) {
                return Err(JsError::Syntax {
                    message: format!("`{name}` is already bound in every program's scope"),
                    line: Some(1),
                    col: None,
                });
            }
        }
        self.check(src, caps).await
    }

    async fn run(&self, p: Program) -> Result<JsRun, JsError> {
        // `took <ms> <ops>` makes the engine report a cost, so a test can tell a real measurement
        // from a zero.
        let mut ms = 0u64;
        let mut ops = 1u64;
        let by_name: BTreeMap<String, usize> = p
            .host
            .iter()
            .enumerate()
            .map(|(i, h)| (h.name.clone(), i))
            .collect();
        for line in p.source.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (op, rest) = line.split_once(' ').unwrap_or((line, ""));
            match op {
                "log" => p.console.write(rest),
                "throw" => {
                    return Err(JsError::Thrown {
                        message: rest.to_string(),
                        stack: None,
                    })
                }
                "call" => {
                    let (name, args) = rest.split_once(' ').unwrap_or((rest, "[]"));
                    let Some(i) = by_name.get(name) else {
                        // What a sandbox does with a name it was never given.
                        p.console.write(&format!("undefined {name}"));
                        continue;
                    };
                    let args: Vec<serde_json::Value> =
                        serde_json::from_str(args).expect("test scripts carry legal JSON");
                    match p.host[*i].body.call(args).await {
                        Ok(v) => p.console.write(&format!("ok {v}")),
                        Err(e) => p.console.write(&format!("err {:?} {}", e.kind, e.message)),
                    }
                }
                // A host call the engine DETACHES: handed to the runtime and never awaited,
                // which is what `js-quickjs`'s `select!` leaves behind when the wall clock or a
                // cancel wins while a call is in flight.
                "detach" => {
                    let (name, args) = rest.split_once(' ').unwrap_or((rest, "[]"));
                    if let Some(i) = by_name.get(name) {
                        let body = p.host[*i].body.clone();
                        let args: Vec<serde_json::Value> =
                            serde_json::from_str(args).expect("test scripts carry legal JSON");
                        tokio::spawn(async move {
                            let _ = body.call(args).await;
                        });
                        // Let the detached call reach the tool before the program returns, so it
                        // is genuinely IN FLIGHT when the round closes.
                        for _ in 0..20 {
                            tokio::task::yield_now().await;
                        }
                    }
                }
                "took" => {
                    let (a, b) = rest.split_once(' ').unwrap_or((rest, "1"));
                    ms = a.parse().unwrap_or(0);
                    ops = b.parse().unwrap_or(1);
                }
                _ => {}
            }
        }
        Ok(JsRun {
            console: String::new(),
            console_bytes_dropped: 0,
            ops,
            ms,
            value: None,
        })
    }
}

/// A tool that answers with its own name, optionally concluding the wake.
pub struct Echo {
    pub concludes: bool,
}

#[async_trait::async_trait]
impl Tool for Echo {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        Ok(ToolOutcome {
            content: format!("{} said {}", call.name, call.args),
            value: None,
            cites: vec![],
            concludes_wake: self.concludes,
        })
    }
}

pub fn agent() -> AgentName {
    AgentName::new("lane")
}

pub fn traj() -> TrajId {
    TrajId::new("t1")
}

pub fn spec(name: &str, tool: Arc<dyn Tool>) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        description: format!("the {name} tool"),
        input_schema: schemars::Schema::try_from(serde_json::json!({"type": "object"})).unwrap(),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool,
    }
}

pub fn config() -> CodemodeConfig {
    CodemodeConfig {
        caps: Some(Caps {
            ops: 1_000_000,
            memory_bytes: 1 << 20,
            stack_bytes: 1 << 16,
            wall_ms: 5_000,
            console_bytes: 4096,
        }),
        conceal: ConcealMode::Mirror,
        aliases: BTreeMap::new(),
        namespaces: BTreeMap::new(),
        hide: Default::default(),
        shell_tools: ["bash".to_string()].into_iter().collect(),
        shell_content_result: ["bash".to_string()].into_iter().collect(),
        tags_min: 3,
        tags_max: 5,
        inner_deadline_ms: None,
        max_parallel_calls: 8,
        max_console_bytes: 4096,
        max_calls_per_program: 16,
        tags_required: false,
        surface_section: false,
    }
}

/// Everything one case needs: a context, a ledger with this crate's step types declared and an
/// agent row, a registry holding `specs`, and the `run` tool over a scripted engine.
pub struct Harness {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub tools: ToolsHandle,
    pub run: Arc<bough_plugin_tools_codemode::run::Run>,
    pub conceal: Arc<Concealment>,
}

pub async fn harness(specs: Vec<ToolSpec>, cfg: CodemodeConfig) -> Harness {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
    for def in bough_plugin_tools_codemode::vocabulary::step_types() {
        ledger.0.register_step_type(def).unwrap();
    }
    for def in bough_plugin_tools::vocabulary::step_types() {
        let _ = ledger.0.register_step_type(def);
    }
    ledger
        .0
        .put_agent(AgentRow {
            name: agent(),
            traj: traj(),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .unwrap();

    let tools = ToolsHandle::with_limits(8, 5_000);
    for s in specs {
        tools.register(&ctx, s).await.unwrap();
    }

    let js = JsHandle::with_caps(Caps {
        ops: 1_000,
        memory_bytes: 1 << 20,
        stack_bytes: 1 << 16,
        wall_ms: 1_000,
        console_bytes: 4096,
    });
    js.set_engine(&ctx, Arc::new(ScriptEngine)).await.unwrap();

    let cfg = Arc::new(cfg);
    let conceal = Arc::new(Concealment::new(cfg.conceal));
    let run = Arc::new(bough_plugin_tools_codemode::run::Run {
        cfg: cfg.clone(),
        ctx: ctx.clone(),
        fiber: ctx.fiber_uid(),
        js,
        tools: tools.clone(),
        ledger: ledger.clone(),
        conceal: conceal.clone(),
    });
    // `run` is an ORDINARY registration, and the concealment goes on after it: that order is what
    // the row's `apply` does, and it is what makes `schemas()` answer `run` alone.
    tools
        .register(&ctx, bough_plugin_tools_codemode::run::spec(run.clone()))
        .await
        .unwrap();
    conceal.install(&ctx, &tools, &agent()).await.unwrap();
    Harness {
        ctx,
        ledger,
        tools,
        run,
        conceal,
    }
}

impl Harness {
    /// Call `run` with a script, exactly as the loop would.
    pub async fn program(&self, source: &str) -> Result<ToolOutcome, ToolFailure> {
        let call = Arc::new(ToolCall {
            id: ToolCallId::new("call_1"),
            name: ToolName::new("run"),
            args: serde_json::json!({ "program": source }),
            agent: agent(),
            wake: WakeId::new("w1"),
            step_index: 1,
        });
        let cx = ToolCx {
            ctx: self.ctx.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
            deadline: None,
            initiator: None,
        };
        self.run.call(call, cx).await
    }

    /// Every step of one kind, oldest first.
    pub async fn steps(&self, kind: &str) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj()],
                kinds: vec![bough_plugin_ledger::StepType::new(kind)],
                class: None,
                wake: None,
                after: None,
                before: None,
                refs: vec![],
                order: Order::SeqAsc,
                limit: None,
            })
            .await
            .unwrap()
    }
}
