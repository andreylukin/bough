//! V5 — the rest of the surface works FROM A PROGRAM.
//!
//! Everything here runs real JavaScript in the embedded QuickJS engine, against the real tools
//! (`tools-operator`, `tool-actions`, `tool-workers`) over their real seams, with the
//! alias map the `bough-codemode` bundle ships. Nothing is stubbed except the two places where a
//! stub IS the seam's extension point — the action Provider (a `gh` shim: no PR is opened) and
//! the worker Provider — plus a synthetic clock, so "fires at +5m" is assertable without waiting.
//!
//! What each case proves is stated at the case.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, FiberUid, KernelCore};
use bough_plugin_actions::{
    ActionArtifact, ActionError, ActionKind, ActionProvider, ActionsHandle, ExecuteRequest,
};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, Agents, AgentsHandle, Attach,
    CancelCause, CreateAgent, InboxReceipt, Message, MessageId, Target, WakeCause, WakeKind,
    WakeRequest,
};
use bough_plugin_js::{Caps, JsHandle};
use bough_plugin_js_quickjs::{QuickJsConfig, QuickJsEngine};
use bough_plugin_ledger::vocabulary::{MailClass, MailDelivered, PinSet};
use bough_plugin_ledger::{
    ActionQuery, ActionStatus, AgentName, Append, Cite, Class, LedgerHandle, Order, Ref, Step,
    StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tools::{
    Tool, ToolCall, ToolCallId, ToolCx, ToolFailure, ToolName, ToolOutcome, Tools, ToolsHandle,
};
use bough_plugin_tools_codemode::conceal::Concealment;
use bough_plugin_tools_codemode::{CodemodeConfig, ConcealMode};
use bough_plugin_tools_operator::{Clock, OperatorConfig};
use bough_plugin_workers::{
    AskSink, Bounds, StartWorker, WorkerError, WorkerKind, WorkerOutcome, WorkerProvider,
    WorkerResult, WorkerRun, Workers, WorkersHandle,
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

// ---------------------------------------------------------------------------------------------
// the alias map the bundle ships — the fixture and `bundles/bough-codemode.yml` must agree
// ---------------------------------------------------------------------------------------------

const ALIASES: &[(&str, &str)] = &[
    ("agent", "spawn_worker"),
    ("ledger.search", "ledger_read?op=search#q"),
    ("ledger.steps", "ledger_read?op=steps#range"),
    ("ledger.tail", "ledger_read?op=tail#n"),
    ("bg", "bg?op=start#name,cmd"),
    ("bg.output", "bg?op=output#id"),
    ("bg.kill", "bg?op=kill#id"),
    ("act", "open_pr|push_to_pr|bot_thread_op|linear_write"),
];

// ---------------------------------------------------------------------------------------------
// seam stand-ins: a clock, an agent driver, a graph, a gh shim, a worker provider, an ask sink
// ---------------------------------------------------------------------------------------------

struct Synthetic(Mutex<DateTime<Utc>>);

impl Synthetic {
    fn at(s: &str) -> Arc<Synthetic> {
        Arc::new(Synthetic(Mutex::new(
            DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc),
        )))
    }
    fn advance(&self, d: chrono::Duration) {
        *self.0.lock() += d;
    }
}

impl Clock for Synthetic {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock()
    }
}

#[derive(Default)]
struct Factory {
    drivers: Mutex<Vec<Arc<Driver>>>,
}

struct Driver {
    #[allow(dead_code)]
    cell: AgentCell,
    notified: Mutex<Vec<(String, bool)>>,
}

#[async_trait::async_trait]
impl AgentFactory for Factory {
    fn driver(&self) -> &'static str {
        "v5-driver"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _m: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        let d = Arc::new(Driver {
            cell,
            notified: Mutex::new(Vec::new()),
        });
        self.drivers.lock().push(d.clone());
        Ok(d as Arc<dyn AgentDriver>)
    }
}

#[async_trait::async_trait]
impl AgentDriver for Driver {
    fn driver(&self) -> &'static str {
        "v5-driver"
    }
    async fn notify(&self, receipt: &InboxReceipt, msg: &Message) {
        self.notified
            .lock()
            .push((msg.subject.clone(), receipt.wake));
    }
    async fn cancel(&self, _c: CancelCause, _keep: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _k: WakeKind, _c: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
}

/// The `gh` shim: it performs no outward act, and it embeds the seam's marker the way a real
/// Provider must, so the journal's "done" row is the real shape.
#[derive(Default)]
struct GhShim {
    seen: Mutex<Vec<(ActionKind, String, String)>>,
}

#[async_trait::async_trait]
impl ActionProvider for GhShim {
    fn kinds(&self) -> Vec<ActionKind> {
        vec![ActionKind::OpenPr, ActionKind::PushToPr]
    }
    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        self.seen.lock().push((
            req.request.kind,
            req.canonical_target.clone(),
            req.marker.clone(),
        ));
        Ok(ActionArtifact {
            locator: format!("https://github.test/{}/pull/1", req.canonical_target),
            marker: req.marker.clone(),
            detail: serde_json::json!({ "shim": "gh" }),
        })
    }
}

/// The worker Provider. It records what the seam handed it, and — when `ask_program` is set — the
/// worker RUNS THAT PROGRAM as itself, which is how `ask()` is exercised from inside a sandbox by
/// an agent that actually has a spawner.
struct Recorder {
    seen: Mutex<Vec<StartWorker>>,
    ask_program: Mutex<Option<String>>,
    ask_console: Mutex<Vec<String>>,
    fixture: Mutex<Option<Arc<Inner>>>,
}

impl Recorder {
    fn new() -> Arc<Recorder> {
        Arc::new(Recorder {
            seen: Mutex::new(Vec::new()),
            ask_program: Mutex::new(None),
            ask_console: Mutex::new(Vec::new()),
            fixture: Mutex::new(None),
        })
    }
}

#[async_trait::async_trait]
impl WorkerProvider for Recorder {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Spawn, WorkerKind::Fork]
    }
    async fn start(
        &self,
        req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError> {
        self.seen.lock().push((*req).clone());
        let program = if req.kind == WorkerKind::Spawn {
            self.ask_program.lock().clone()
        } else {
            None
        };
        let fixture = self.fixture.lock().clone();
        if let (Some(src), Some(inner)) = (program, fixture) {
            let worker = WorkersHandle::worker_agent_name(&req.spawner, run.id());
            inner.put_agent(&worker).await;
            let out = inner.program_as(&worker, "worker-call", &src).await;
            self.ask_console.lock().push(match out {
                Ok(o) => o.content,
                Err(e) => format!("REFUSED {}", e.message),
            });
        }
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

/// The lane a worker's question is delivered on. It answers, so `AskMode::Block` returns text.
#[derive(Default)]
struct Sink {
    asked: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AskSink for Sink {
    async fn deliver(
        &self,
        _spawner: &AgentName,
        msg: Message,
        _target: Target,
        _wake: bool,
    ) -> Result<MessageId, WorkerError> {
        self.asked.lock().push(msg.text.clone());
        Ok(msg.id)
    }
    async fn answer(&self, _msg: &MessageId) -> Option<String> {
        Some("staging".to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------------------------

fn agent() -> AgentName {
    AgentName::new("lane")
}

fn traj() -> TrajId {
    TrajId::new("t-lane")
}

fn opcfg() -> Arc<OperatorConfig> {
    Arc::new(OperatorConfig {
        max_view_bytes: 1_000_000,
        max_files_per_patch: 8,
        bg_log_dir: PathBuf::from("/tmp"),
        bg_max: 2,
        bg_poll_ms: 20,
        ledger_page: 50,
        schedule_max_horizon_days: 30,
        schedule_tick_ms: 10,
        sh_max_legs: 8,
        sh_timeout_ms: 120_000,
        sh_tags_min: 3,
        sh_tags_max: 5,
    })
}

fn config() -> CodemodeConfig {
    CodemodeConfig {
        caps: Some(Caps {
            ops: 20_000_000,
            memory_bytes: 64 << 20,
            stack_bytes: 1 << 20,
            wall_ms: 20_000,
            console_bytes: 65_536,
        }),
        conceal: ConcealMode::Mirror,
        aliases: ALIASES
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect::<BTreeMap<String, String>>(),
        namespaces: BTreeMap::new(),
        hide: Default::default(),
        shell_tools: ["bash".to_string()].into_iter().collect(),
        shell_content_result: ["bash".to_string()].into_iter().collect(),
        tags_min: 3,
        tags_max: 5,
        inner_deadline_ms: None,
        max_parallel_calls: 8,
        max_console_bytes: 65_536,
        max_calls_per_program: 32,
        tags_required: false,
        surface_section: false,
    }
}

static NEXT_FIBER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct Inner {
    ctx: Context,
    ledger: LedgerHandle,
    run: Arc<bough_plugin_tools_codemode::run::Run>,
}

impl Inner {
    async fn put_agent(&self, name: &AgentName) {
        self.ledger
            .0
            .put_agent(bough_plugin_ledger::AgentRow {
                name: name.clone(),
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

    /// Call `run` exactly as the loop would, as `who`.
    async fn program_as(
        &self,
        who: &AgentName,
        id: &str,
        source: &str,
    ) -> Result<ToolOutcome, ToolFailure> {
        let call = Arc::new(ToolCall {
            id: ToolCallId::new(id),
            name: ToolName::new("run"),
            args: serde_json::json!({ "program": source }),
            agent: who.clone(),
            wake: WakeId::new("w1"),
            step_index: 1,
        });
        self.run
            .call(
                call,
                ToolCx {
                    ctx: self.ctx.clone(),
                    cancel: Default::default(),
                    deadline: None,
                    initiator: None,
                },
            )
            .await
    }
}

struct Fx {
    inner: Arc<Inner>,
    clock: Arc<Synthetic>,
    agents: AgentsHandle,
    factory: Arc<Factory>,
    actions: Arc<ActionsHandle>,
    gh: Arc<GhShim>,
    workers: WorkersHandle,
    recorder: Arc<Recorder>,
    sink: Arc<Sink>,
    fiber_base: u64,
    agent: Agent,
    _slots: Vec<Box<dyn std::any::Any>>,
}

async fn fixture(bounds: Bounds) -> Fx {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    for def in bough_plugin_tools_codemode::vocabulary::step_types() {
        let _ = ledger.0.register_step_type(def);
    }
    for def in bough_plugin_tools::vocabulary::step_types() {
        let _ = ledger.0.register_step_type(def);
    }
    ledger
        .declare_step_types(&ctx, bough_plugin_tools_operator::schedule::step_types())
        .await
        .expect("the schedule types declare");

    let clock = Synthetic::at("2026-08-27T12:00:00Z");
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
    let factory = Arc::new(Factory::default());
    agents
        .set_factory(&ctx, factory.clone() as Arc<dyn AgentFactory>)
        .await
        .expect("the slot is free");
    let (live, disposer) = agents
        .create(CreateAgent::resident(agent(), traj(), clock.now()))
        .await
        .expect("the agent is created");

    let actions = Arc::new(ActionsHandle::new(ledger.clone()));
    let gh = Arc::new(GhShim::default());
    let gh_eff = actions
        .provider(&ctx, gh.clone() as Arc<dyn ActionProvider>)
        .await
        .expect("the shim mounts");

    let workers = WorkersHandle::new(bounds);
    let recorder = Recorder::new();
    let w_eff = workers
        .provider(&ctx, recorder.clone() as Arc<dyn WorkerProvider>)
        .await
        .expect("the worker provider mounts");
    let sink = Arc::new(Sink::default());
    let s_eff = workers
        .ask_sink(&ctx, sink.clone() as Arc<dyn AskSink>)
        .await
        .expect("the sink mounts");

    let tools = ToolsHandle::with_limits(8, 20_000);
    for spec in bough_plugin_tools_operator::specs(
        opcfg(),
        clock.clone() as Arc<dyn Clock>,
        ledger.clone(),
        Some(agents.clone()),
        bough_plugin_tools_operator::bg::BgJobs::new(opcfg(), PathBuf::from("/tmp")),
        PathBuf::from("/tmp"),
    ) {
        tools
            .register(&ctx, spec)
            .await
            .expect("an operator tool registers");
    }
    for kind in ActionKind::all() {
        tools
            .register(
                &ctx,
                bough_plugin_tool_actions::spec(*kind, actions.clone()),
            )
            .await
            .expect("an action tool registers");
    }
    for spec in [
        bough_plugin_tool_workers::SpawnWorkerTool::spec(),
        bough_plugin_tool_workers::ForkTool::spec(),
        bough_plugin_tool_workers::AskTool::spec(),
    ] {
        tools
            .register(&ctx, spec)
            .await
            .expect("a worker tool registers");
    }

    // The services the tools read out of the context.
    let mut slots: Vec<Box<dyn std::any::Any>> = vec![
        Box::new(disposer),
        Box::new(gh_eff),
        Box::new(w_eff),
        Box::new(s_eff),
        Box::new(ctx.provide::<Agents>(agents.clone()).await.expect("agents")),
        Box::new(
            ctx.provide::<Workers>(workers.clone())
                .await
                .expect("workers"),
        ),
        Box::new(ctx.provide::<Tools>(tools.clone()).await.expect("tools")),
    ];
    let js = JsHandle::with_caps(Caps {
        ops: 20_000_000,
        memory_bytes: 64 << 20,
        stack_bytes: 1 << 20,
        wall_ms: 20_000,
        console_bytes: 65_536,
    });
    let js_eff = js
        .set_engine(
            &ctx,
            Arc::new(QuickJsEngine::new(Arc::new(QuickJsConfig {
                interrupt_check_ops: 10_000,
                max_concurrent_programs: 4,
            }))),
        )
        .await
        .expect("the engine mounts");
    slots.push(Box::new(js_eff));

    let cfg = Arc::new(config());
    let conceal = Arc::new(Concealment::new(cfg.conceal));
    let run = Arc::new(bough_plugin_tools_codemode::run::Run {
        cfg,
        ctx: ctx.clone(),
        fiber: ctx.fiber_uid(),
        js,
        tools: tools.clone(),
        ledger: ledger.clone(),
        conceal: conceal.clone(),
    });
    tools
        .register(&ctx, bough_plugin_tools_codemode::run::spec(run.clone()))
        .await
        .expect("`run` registers");
    conceal
        .install(&ctx, &tools, &agent())
        .await
        .expect("the mirror installs");

    let inner = Arc::new(Inner {
        ctx: ctx.clone(),
        ledger: ledger.clone(),
        run,
    });
    *recorder.fixture.lock() = Some(inner.clone());

    Fx {
        inner,
        clock,
        agents,
        factory,
        actions,
        gh,
        workers,
        recorder,
        sink,
        fiber_base: NEXT_FIBER.fetch_add(100, std::sync::atomic::Ordering::SeqCst),
        agent: live,
        _slots: slots,
    }
}

impl Fx {
    async fn program(&self, src: &str) -> ToolOutcome {
        match self.inner.program_as(&agent(), "call_1", src).await {
            Ok(o) => o,
            Err(e) => panic!("the program failed outright: {}", e.message),
        }
    }

    async fn steps(&self, kind: &str) -> Vec<Step> {
        self.inner
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj()],
                kinds: vec![StepType::new(kind)],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("the ledger reads")
    }

    /// The names of the tools the program's sub-steps say it called, in order.
    async fn called(&self) -> Vec<String> {
        self.steps("program/call")
            .await
            .iter()
            .filter_map(|s| {
                s.body
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect()
    }

    async fn pin(&self, title: &str, text: &str) {
        self.inner
            .ledger
            .0
            .append(Append {
                traj: traj(),
                wake: WakeId::new("w0"),
                kind: StepType::new("pin/set"),
                class: Class::Thought,
                body: serde_json::to_value(PinSet {
                    title: title.to_string(),
                    text: text.to_string(),
                    supersedes: vec![],
                })
                .unwrap(),
                cites: vec![],
                at: self.clock.now(),
                id: None,
            })
            .await
            .expect("a pin appends");
    }

    async fn deliver(&self, subject: &str) {
        self.inner
            .ledger
            .0
            .append(Append {
                traj: traj(),
                wake: WakeId::new("wake:outside"),
                kind: StepType::new("mail/delivered"),
                class: Class::Evidence,
                body: serde_json::to_value(MailDelivered {
                    class: MailClass::Ordinary,
                    from: Ref::new("andrey"),
                    subject: subject.to_string(),
                    summary: "the deploy is red".to_string(),
                    refs: vec![],
                })
                .unwrap(),
                cites: vec![Cite {
                    r#ref: Ref::new("andrey"),
                    url: None,
                }],
                at: self.clock.now(),
                id: None,
            })
            .await
            .expect("mail appends");
    }
}

// ---------------------------------------------------------------------------------------------
// the cases
// ---------------------------------------------------------------------------------------------

/// The three ledger reads are three FUNCTIONS over one op-discriminated tool, and each one
/// reaches the real store: a search finds the row a pin wrote, a tail counts, and a range reads
/// between two seqs. The `op` is the alias's, not the program's.
#[tokio::test]
async fn ledger_search_steps_and_tail_drill_from_a_tier_to_raw_steps() {
    let fx = fixture(bounds()).await;
    for n in 0..6 {
        fx.pin(&format!("pin {n}"), "ordinary body").await;
    }
    fx.pin("the needle", "quokkaphone regression").await;

    let out = fx
        .program(
            r#"
            const s = await ledger.search("quokkaphone");
            const kinds = s.steps.map(x => x.kind).join(",");
            console.log("search=" + s.count + " kinds=" + kinds);
            const t = await ledger.tail(4);
            console.log("tail=" + t.count);
            const r = await ledger.steps("0..3");
            console.log("steps=" + r.steps.map(x => x.seq).join(","));
            "#,
        )
        .await;

    assert!(out.content.contains("kinds="), "{}", out.content);
    assert!(
        out.content.contains("pin/set"),
        "the search found the pin it was looking for: {}",
        out.content
    );
    assert!(out.content.contains("tail=4"), "{}", out.content);
    assert!(
        out.content.contains("steps=1,2"),
        "a range reads exactly the seqs it names (`after` is exclusive): {}",
        out.content
    );

    // Every one went through the pipeline as the SAME tool, with the alias's `op` filled in.
    let calls = fx.steps("program/call").await;
    let ops: Vec<String> = calls
        .iter()
        .map(|s| s.body["args"]["op"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        ops,
        vec!["search", "tail", "steps"],
        "the op is the ALIAS's: {ops:?}"
    );
    assert!(calls.iter().all(|s| s.body["name"] == "ledger_read"));
    // A drill is EVIDENCE: its result step cites the rows it read.
    let results = fx.steps("program/result").await;
    assert!(
        results.iter().any(|s| !s.cites.is_empty()),
        "a drill's result cites its steps"
    );
}

/// `inbox()` returns the mail no wake has consumed, through the same pipeline.
#[tokio::test]
async fn inbox_returns_the_unconsumed_mail() {
    let fx = fixture(bounds()).await;
    fx.deliver("ci is red").await;
    fx.deliver("review please").await;

    let out = fx
        .program(
            r#"
            const m = await inbox();
            console.log("count=" + m.count);
            console.log("first=" + m.mail[0].subject);
            "#,
        )
        .await;
    assert!(out.content.contains("count=2"), "{}", out.content);
    assert!(out.content.contains("first=ci is red"), "{}", out.content);
    assert_eq!(fx.called().await, vec!["inbox"]);
}

/// `act(kind, target, payload)` is ONE function over four kinds: the first argument picks the
/// tool, the act goes through the actions journal (intent then done, with the seam's idempotency
/// marker), and the `gh` shim is what stands where the outward act would be.
#[tokio::test]
async fn act_open_pr_goes_through_the_actions_journal() {
    let fx = fixture(bounds()).await;
    let out = fx
        .program(
            r#"
            const a = await act("open_pr", "bough/rebuild", { title: "codemode" });
            console.log(JSON.stringify(a).slice(0, 300));
            try {
                await act("linear_write", "TEAM-1", {});
                console.log("UNREACHED");
            } catch (e) {
                console.log("refused=" + String(e.message || e).slice(0, 60));
            }
            "#,
        )
        .await;

    let seen = fx.gh.seen.lock().clone();
    assert_eq!(seen.len(), 1, "one act reached the Provider");
    assert_eq!(seen[0].0, ActionKind::OpenPr);
    assert_eq!(seen[0].1, "bough/rebuild", "the target is canonicalised");
    assert!(
        seen[0]
            .2
            .starts_with(bough_plugin_actions::journal::MARKER_PREFIX),
        "the Provider is handed the seam's marker: {}",
        seen[0].2
    );

    let rows = fx.actions.0.clone();
    let journal = fx
        .inner
        .ledger
        .0
        .actions(&ActionQuery::default())
        .await
        .expect("the journal reads");
    let _ = rows;
    assert_eq!(journal.len(), 1, "one journal row");
    assert_eq!(journal[0].status, ActionStatus::Done);
    assert_eq!(journal[0].kind, "open_pr");
    assert!(!journal[0].idem_key.as_str().is_empty(), "the row is keyed");
    assert!(
        out.content
            .contains("https://github.test/bough/rebuild/pull/1"),
        "the program got the locator back: {}",
        out.content
    );
    // A kind with no Provider is refused IN the sandbox, as a catchable error.
    assert!(out.content.contains("refused="), "{}", out.content);
    assert!(!out.content.contains("UNREACHED"), "{}", out.content);
}

/// `agent()` and `fork()` reach the WORKERS seam — not a shortcut around it — with the kind, the
/// task, the depth and the tool restriction the program asked for; and `ask()` from inside a
/// worker's own program is delivered on the spawner's lane and comes back as the answer.
#[tokio::test]
async fn agent_ask_and_fork_go_through_the_workers_seam() {
    let fx = fixture(bounds()).await;
    *fx.recorder.ask_program.lock() = Some(
        r#"const a = await ask("which environment?"); console.log("answer=" + a);"#.to_string(),
    );

    let out = fx
        .program(
            r#"
            await agent("port the retry test", { name: "retry-port", tools: ["bash", "view"] });
            await fork("try the other approach");
            console.log("done");
            "#,
        )
        .await;
    assert!(out.content.contains("done"), "{}", out.content);

    let seen = fx.recorder.seen.lock().clone();
    assert_eq!(seen.len(), 2, "two starts");
    assert_eq!(seen[0].kind, WorkerKind::Spawn);
    assert_eq!(
        seen[0].task, "[worker: retry-port]\nport the retry test",
        "the opts name is woven into the task header"
    );
    assert_eq!(seen[0].spawner, agent());
    assert_eq!(seen[0].depth, 1);
    let restrict = seen[0].tools.as_ref().expect("agent() narrowed the tools");
    assert_eq!(
        restrict.allow.as_ref().map(|a| a.len()),
        Some(2),
        "the allow-list is the intersection the seam composes"
    );
    assert_eq!(seen[1].kind, WorkerKind::Fork);
    assert_eq!(seen[1].task, "try the other approach");
    assert!(seen[1].tools.is_none(), "a fork keeps the parent's tools");

    // The ask: delivered on the spawner's lane, answered, and the answer reached the worker's
    // program as the value of `await ask(...)`.
    assert_eq!(
        fx.sink.asked.lock().clone(),
        vec!["which environment?".to_string()],
        "the question went through the seam's sink"
    );
    let console = fx.recorder.ask_console.lock().clone();
    assert!(
        console.iter().any(|c| c.contains("answer=staging")),
        "the worker's program received the spawner's answer: {console:?}"
    );
    let _ = &fx.workers;
}

/// The seam's bounds hold from a program: the cap-plus-one spawn is refused, the refusal names
/// the bound, and no extra worker started.
#[tokio::test]
async fn the_worker_bounds_refuse_the_cap_plus_one_spawn() {
    let fx = fixture(Bounds {
        max_in_flight: 8,
        max_depth: 3,
        per_wake_spawn_cap: 2,
    })
    .await;

    let out = fx
        .program(
            r#"
            for (const t of ["one", "two", "three"]) {
                try {
                    await agent(t, { tools: [] });
                    console.log("started=" + t);
                } catch (e) {
                    console.log("refused=" + String(e.message || e));
                }
            }
            "#,
        )
        .await;

    assert!(out.content.contains("started=one"), "{}", out.content);
    assert!(out.content.contains("started=two"), "{}", out.content);
    assert!(
        out.content.contains("refused=") && out.content.contains("per_wake_spawn_cap"),
        "the third is refused and the refusal names the bound: {}",
        out.content
    );
    assert_eq!(
        fx.recorder.seen.lock().len(),
        2,
        "the cap+1 never reached a Provider"
    );
}

/// `schedule(at, intent)` from a program writes the intent, and the due-watcher fires it exactly
/// once on the synthetic clock and wakes its creator.
#[tokio::test]
async fn a_scheduled_intent_from_a_program_fires_on_the_synthetic_clock() {
    use bough_plugin_tools_operator::schedule::{Watcher, FIRED, INTENT};

    let fx = fixture(bounds()).await;
    let out = fx
        .program(r#"console.log(String(await schedule("+5m", "check the deploy")).slice(0, 80));"#)
        .await;
    assert!(!out.content.is_empty());
    assert_eq!(
        fx.steps(INTENT).await.len(),
        1,
        "the intent is a ledger step"
    );
    assert_eq!(fx.called().await, vec!["schedule"]);

    let watcher = Watcher {
        cfg: opcfg(),
        clock: fx.clock.clone() as Arc<dyn Clock>,
        ledger: fx.inner.ledger.clone(),
        agents: fx.agents.clone(),
        fiber: FiberUid(fx.fiber_base + 1),
    };
    assert!(
        watcher.tick().await.expect("a tick reads").is_empty(),
        "nothing is due yet"
    );
    fx.clock.advance(chrono::Duration::minutes(6));
    assert_eq!(
        watcher.tick().await.expect("a tick reads").len(),
        1,
        "the due intent fires"
    );
    assert_eq!(fx.steps(FIRED).await.len(), 1);
    let driver = fx.factory.drivers.lock()[0].clone();
    let notified = driver.notified.lock().clone();
    assert_eq!(
        notified.len(),
        1,
        "the creator was woken once: {notified:?}"
    );
    assert!(notified[0].1, "the message asked for a wake");
    assert!(fx.agent.has_pending_wake());

    // Exactly once, however often the clock moves.
    fx.clock.advance(chrono::Duration::hours(3));
    assert!(watcher.tick().await.expect("a tick reads").is_empty());
    assert_eq!(fx.steps(FIRED).await.len(), 1);
}

/// The alias map this file drives the sandbox with is the one the bundle ships. Without this the
/// cases above would prove a surface no composition mounts.
#[test]
fn the_bundle_binds_the_documented_names() {
    let yml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../bundles/bough-base.yml"
    ))
    .expect("the base bundle is in the tree");
    for (js, value) in ALIASES {
        assert!(
            yml.contains(&format!("{js}: {value}")),
            "`{js}: {value}` is missing from bundles/bough-base.yml"
        );
    }
}

fn bounds() -> Bounds {
    Bounds {
        max_in_flight: 8,
        max_depth: 3,
        per_wake_spawn_cap: 8,
    }
}
