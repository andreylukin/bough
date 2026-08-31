//! Invariant: this is a SECOND Provider of the same seam, and it is the phase's swap gate. It
//! honours every waterfall and appends every durable step in §5's order — and it implements
//! neither preemption nor retry nor drain debouncing, because a replacement loop is held to the
//! LEDGER PROTOCOL and not to a feature list.

pub mod invariant;
pub mod replay;
pub mod script;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{AgentCell, AgentDriver, AgentError, AgentFactory, Attach};

pub use replay::{DeliveredMail, ReplayEnv, ReplayError, ScriptedClaim, WakeInput, WakeOutcome};
pub use script::{Script, ScriptedChunk, ScriptedStep, ScriptedWake};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "agent-loop-scripted";

/// The row's config: a transcript file, or the wakes inline.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptedConfig {
    #[serde(default)]
    pub transcript: Option<PathBuf>,
    /// Raw JSON, parsed by [`Script::parse`]; the config schema stays shallow on purpose.
    #[serde(default)]
    pub wakes: Option<serde_json::Value>,
    /// `true`: running out of script is an error, not a silent idle.
    #[serde(default = "yes")]
    pub strict: bool,
}

fn yes() -> bool {
    true
}

/// Resolve the row's config into the one script it will replay (§0.2: an explicit
/// `resolve(request) -> Spec`, never a `?? default` inside `apply`).
///
/// `transcript` and `wakes` are alternatives, not a merge: naming both is a misconfiguration and
/// fails loud at boot rather than silently preferring one.
pub fn resolve_script(cfg: &ScriptedConfig) -> Result<Script, String> {
    match (&cfg.transcript, &cfg.wakes) {
        (Some(_), Some(_)) => {
            Err("`transcript` and `wakes` are alternatives: name one, not both".to_string())
        }
        (Some(path), None) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("reading transcript `{}`: {e}", path.display()))?;
            Script::parse(&text)
        }
        (None, Some(v)) => Script::from_value(v),
        (None, None) => Err(
            "a scripted loop with no transcript and no inline `wakes` would never wake: name one"
                .to_string(),
        ),
    }
}

/// The factory this row registers.
pub struct ScriptedFactory {
    cfg: Arc<ScriptedConfig>,
    script: Arc<Script>,
    env: ReplayEnv,
}

impl ScriptedFactory {
    /// The factory `apply` installs into the `agents` seam's factory slot.
    pub fn new(cfg: Arc<ScriptedConfig>, script: Arc<Script>, env: ReplayEnv) -> ScriptedFactory {
        ScriptedFactory { cfg, script, env }
    }

    /// The script this factory replays; the swap test reads it.
    pub fn script(&self) -> &Arc<Script> {
        &self.script
    }
}

#[async_trait::async_trait]
impl AgentFactory for ScriptedFactory {
    fn driver(&self) -> &'static str {
        PLUGIN_NAME
    }

    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        let cell = Arc::new(cell);
        let cfg = self.cfg.clone();
        let env = self.env.clone();
        Ok(Arc::new_cyclic(|me| ScriptedDriver {
            cell,
            cfg,
            env,
            me: me.clone(),
            next_wake: parking_lot::Mutex::new(0),
            stopping: std::sync::atomic::AtomicBool::new(false),
            wakes: Arc::new(tokio::sync::Semaphore::new(MAX_WAKES_IN_FLIGHT as usize)),
            in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }))
    }
}

/// One agent's scripted loop.
///
/// It schedules the way §5's IMMEDIATE half does and nothing more: a waking inbox mutation opens
/// one wake, which replays the next entry of the transcript. There is no debounced drain, no
/// preemption and no retry — a replacement loop is held to the LEDGER PROTOCOL, not to a feature
/// list.
///
/// The claim runs BEFORE the replay (the same order `agent-loop` uses): `AgentCell::claim` is the
/// only durable way to take an inbox item, and it appends the `inbox/spliced { op: claim }` step
/// itself — so [`replay::run_wake`] is handed an empty `claim` list rather than splicing twice.
pub struct ScriptedDriver {
    cell: Arc<AgentCell>,
    cfg: Arc<ScriptedConfig>,
    env: ReplayEnv,
    me: std::sync::Weak<ScriptedDriver>,
    /// Which transcript entry the next wake replays.
    next_wake: parking_lot::Mutex<usize>,
    stopping: std::sync::atomic::AtomicBool,
    /// Wakes in flight; `stop()` drains against it.
    wakes: Arc<tokio::sync::Semaphore>,
    /// Wakes in flight as a COUNT. §2's `status` is the driver-wide interval, not one wake, so
    /// the first wake publishes `Running` and the last one to finish publishes `Idle`. Per-wake
    /// transitions made a concurrent second wake refuse to start at all (its `Running` came back
    /// `StatusRepeat`), and made the first finisher publish `Idle` over a wake still open.
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
}

impl ScriptedDriver {
    /// Mark the agent RUNNING and spawn its wake.
    ///
    /// The status transition is awaited HERE, before the caller's `notify` returns: setting it
    /// inside the spawned task left a window in which the mail was durable, a wake was armed and
    /// `when_idle()` still resolved immediately — a caller could read the chain before the wake
    /// had appended anything.
    /// Whether anything is queued for this agent: either queue of the live inbox, or delivered
    /// ordinary mail the ledger shows unconsumed (the shape a restart leaves behind).
    async fn has_queued_work(&self) -> bool {
        if !self.cell.agent().inbox().is_empty() {
            return true;
        }
        let Ok(steps) = self
            .env
            .ledger
            .0
            .steps(&bough_plugin_ledger::StepQuery {
                trajs: vec![self.cell.agent().traj().clone()],
                ..Default::default()
            })
            .await
        else {
            return false;
        };
        let consumed = bough_plugin_agent_loop::mail::consumed_union(&steps);
        bough_plugin_agent_loop::mail::unconsumed(&steps, &consumed)
            .iter()
            .any(bough_plugin_agent_loop::mail::is_ordinary)
    }

    /// Dispatch `agent/wake-request` at the agent's scope. The facts are the CALLER's: `notify`
    /// has the message in hand, and reading "the oldest pending" here instead would name the
    /// wrong trigger whenever a deferred item is still sitting in the queue.
    async fn admission(
        self: &Arc<Self>,
        kind: bough_plugin_llm::WakeKind,
        cause: bough_plugin_agents::WakeCause,
        facts: Option<bough_plugin_agents::TriggerFacts>,
    ) -> bough_plugin_agents::Admit {
        let agent = self.cell.agent();
        let out = bough_kernel::scope::scope_target(&self.env.ctx, agent.scope_key())
            .waterfall::<bough_plugin_agents::AgentWakeRequest>(
                bough_plugin_agents::WakeAdmission {
                    agent: agent.name().clone(),
                    id: agent.id().clone(),
                    kind,
                    cause,
                    trigger: facts,
                    at: chrono::Utc::now(),
                    decision: bough_plugin_agents::Admit::Open,
                },
            )
            .await;
        out.decision
    }

    /// Returns the id of the wake it opened, or `None` when none was.
    async fn spawn_wake(
        self: &Arc<Self>,
        kind: bough_plugin_llm::WakeKind,
        cause: bough_plugin_agents::WakeCause,
        trigger: Option<bough_plugin_ledger::StepId>,
        facts: Option<bough_plugin_agents::TriggerFacts>,
    ) -> Option<bough_plugin_ledger::WakeId> {
        use std::sync::atomic::Ordering;
        if self.stopping.load(Ordering::SeqCst) {
            return None;
        }
        // P5-D1: the SAME admission point the live loop has, so every test that proves dormancy
        // proves it for both Providers. A `Defer` opens no wake and touches no status.
        if let bough_plugin_agents::Admit::Defer { by, reason } =
            self.admission(kind, cause, facts).await
        {
            tracing::debug!(agent = %self.cell.agent().name(), by, reason, "scripted wake deferred");
            self.cell.wake_refused();
            return None;
        }
        if self.in_flight.fetch_add(1, Ordering::SeqCst) == 0
            && self
                .cell
                .set_status(bough_plugin_agents::Status::Running)
                .await
                .is_err()
        {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let index = {
            let mut n = self.next_wake.lock();
            let i = *n;
            *n += 1;
            i
        };
        let me = self.clone();
        let wake = bough_plugin_ledger::WakeId::new(uuid::Uuid::now_v7().to_string());
        let opened = wake.clone();
        tokio::spawn(async move {
            // See `agent-loop`'s driver: the pending-wake flag goes down when the wake starts, and
            // for a concurrent second wake there is no status edge to do it.
            me.cell.wake_started();
            let _permit = me.wakes.clone().acquire_owned().await.ok();
            let agent = me.cell.agent().clone();
            let at = chrono::Utc::now();
            // Claim both queues: a `steer` to an idle agent must not open a wake that claims
            // nothing (the same rule `agent-loop` follows).
            let mut claimed = Vec::new();
            for target in [
                bough_plugin_agents::Target::NextWake,
                bough_plugin_agents::Target::NextStep,
            ] {
                if let Ok(mut c) = me
                    .cell
                    .claim(
                        bough_plugin_agents::ClaimSelector::all(target),
                        wake.clone(),
                        at,
                    )
                    .await
                {
                    claimed.append(&mut c);
                }
            }
            let answers_andrey = claimed.iter().any(|c| c.message.is_andrey());
            let input = WakeInput {
                traj: agent.traj().clone(),
                agent: agent.name().clone(),
                agent_id: agent.id().clone(),
                wake,
                index,
                kind: if answers_andrey {
                    bough_plugin_llm::WakeKind::Answer
                } else {
                    bough_plugin_llm::WakeKind::Catchup
                },
                urgency: bough_plugin_ledger::vocabulary::Urgency::Immediate,
                trigger,
                answers_andrey,
                model_override: None,
                // Already spliced by `claim` above; handing them over again would double-append.
                claim: Vec::new(),
                deliver: claimed
                    .iter()
                    .map(|c| replay::DeliveredMail {
                        summary: c.message.text.clone(),
                        from: bough_plugin_agent_loop::wake::sender_ref(&c.message.from),
                        class: format!("{:?}", c.message.class).to_lowercase(),
                        subject: c.message.subject.clone(),
                    })
                    .collect(),
                handle: Some(agent.clone()),
                at,
            };
            let out = replay::run_wake(&me.env, &input).await;
            if let Err(e) = &out {
                if me.cfg.strict {
                    tracing::error!(error = %e, "scripted wake ran out of script");
                }
            }
            if me.in_flight.fetch_sub(1, Ordering::SeqCst) == 1 {
                let _ = me.cell.set_status(bough_plugin_agents::Status::Idle).await;
            }
        });
        Some(opened)
    }
}

#[async_trait::async_trait]
impl AgentDriver for ScriptedDriver {
    fn driver(&self) -> &'static str {
        PLUGIN_NAME
    }

    /// §5's catch-up (P3-D16): one scripted wake if the transcript has one left.
    async fn wake_now(
        &self,
        kind: bough_plugin_agents::WakeKind,
        cause: bough_plugin_agents::WakeCause,
    ) -> bough_plugin_agents::WakeRequest {
        use bough_plugin_agents::WakeRequest;
        let Some(me) = self.me.upgrade() else {
            return WakeRequest::Nothing;
        };
        // TWO conditions, both necessary. §5's catch-up is over QUEUED MAIL, so an agent with
        // nothing queued gets no wake from either Provider — the seam's contract cannot differ by
        // driver or `residents` would behave differently under the test profile. And a wake with
        // no transcript entry left would run off the end of the script, which is what strict mode
        // exists to refuse.
        if *me.next_wake.lock() >= me.env.script.wakes.len() {
            return WakeRequest::Nothing;
        }
        if !me.has_queued_work().await {
            return WakeRequest::Nothing;
        }
        let facts = [
            bough_plugin_agents::Target::NextWake,
            bough_plugin_agents::Target::NextStep,
        ]
        .into_iter()
        .flat_map(|t| me.cell.agent().inbox().pending(t))
        .next()
        .map(|m| facts_of(&m));
        match me.spawn_wake(kind, cause, None, facts).await {
            Some(wake) => WakeRequest::Started(wake),
            None => WakeRequest::Nothing,
        }
    }

    async fn notify(
        &self,
        receipt: &bough_plugin_agents::InboxReceipt,
        msg: &bough_plugin_agents::Message,
    ) {
        if !receipt.wake {
            return;
        }
        // The cause is what an admission listener reads (P5-D1): Andrey always reactivates, and
        // wake-class mail only reactivates a lane that asked for its class.
        let (kind, cause) = if msg.is_andrey() {
            (
                bough_plugin_llm::WakeKind::Answer,
                bough_plugin_agents::WakeCause::Andrey,
            )
        } else {
            (
                bough_plugin_llm::WakeKind::Catchup,
                bough_plugin_agents::WakeCause::Mail { class: msg.class },
            )
        };
        if let Some(me) = self.me.upgrade() {
            me.spawn_wake(kind, cause, Some(receipt.step.clone()), Some(facts_of(msg)))
                .await;
        }
    }

    async fn cancel(&self, _cause: bough_plugin_agents::CancelCause, _keep_inbox: bool) {
        self.cell.cancel_token().cancel();
    }

    async fn stop(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Drain: every permit back means every wake in flight has ended.
        let _ = self.wakes.acquire_many(MAX_WAKES_IN_FLIGHT).await;
    }
}

/// The trigger facts of one message, so an admission listener never re-reads the inbox.
fn facts_of(m: &bough_plugin_agents::Message) -> bough_plugin_agents::TriggerFacts {
    bough_plugin_agents::TriggerFacts {
        message: m.id.clone(),
        from_andrey: m.is_andrey(),
        class: m.class,
        refs: m.refs.clone(),
        mail_seq: m.mail_seq,
    }
}

/// The scripted loop replays one wake at a time; the semaphore is the drain latch `stop` waits on.
const MAX_WAKES_IN_FLIGHT: u32 = 1;

/// The Provider row. In the catalog, in NO bundle: the swap patch names it.
pub struct ScriptedLoopPlugin;

#[async_trait::async_trait]
impl Plugin for ScriptedLoopPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ScriptedConfig;

    /// `workers` for the same reason `agent-loop` declares it: this Provider executes tools too,
    /// so a `spawn_worker` in a scripted transcript resolves the seam through THIS row's context.
    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["agents", "ledger", "projection", "tools"])
            .union(&bough_kernel::Inject::optional(["workers"]))
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |detail: String| PluginError::new(entry.clone(), anyhow::anyhow!(detail));

        // Misconfiguration fails LOUD (§0.2): an unreadable or unreplayable transcript is a boot
        // failure, not a row that mounts and never wakes.
        let script = Arc::new(resolve_script(&cfg).map_err(fail)?);

        let ledger = ctx
            .get::<bough_plugin_ledger::Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let projection = ctx
            .get::<bough_plugin_projection::Projection>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // The row injects `tools`; taking the handle here is what makes a scripted `tool/call`
        // run the real guarded pipeline rather than dangling in the ledger.
        let tools = ctx
            .get::<bough_plugin_tools::Tools>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // P2-D18: the shared EVALUATOR lives in `agent-loop`; this row is its second RECORDER.
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || bough_plugin_agent_loop::invariant::forget(mine));
            Ok(())
        })
        .await?;
        let recorder: replay::Recorder = Arc::new(move |r: replay::Recorded| {
            bough_plugin_agent_loop::invariant::record(
                bough_plugin_agent_loop::invariant::SentRequest {
                    fiber: mine,
                    wake: r.wake,
                    step_index: r.step_index,
                    request: r.request,
                },
            );
        });

        let env = ReplayEnv {
            ctx: ctx.clone(),
            ledger: bough_plugin_ledger::LedgerHandle(ledger.0.clone()),
            projection: Some(bough_plugin_projection::ProjectionHandle(
                projection.0.clone(),
            )),
            script: script.clone(),
            strict: cfg.strict,
            prompt_ver: "scripted".to_string(),
            composition: ctx.entry_id().to_string(),
            default_max_tokens: 8192,
            recorder: Some(recorder),
            tools: Some(bough_plugin_tools::ToolsHandle(tools.0.clone())),
        };

        let agents = ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // The slot the swap test frees: taking it is an EFFECT, so unloading this row hands it
        // back to whichever loop Provider the patch mounts next.
        bough_plugin_agents::AgentsHandle(agents.0.clone())
            .set_factory(&ctx, Arc::new(ScriptedFactory::new(cfg, script, env)))
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::requests_reconstruct_from_the_ledger()]
    }
}

bough_kernel::register_plugin!(ScriptedLoopPlugin);
