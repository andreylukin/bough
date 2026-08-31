//! §5's "own scheduled intents": an intent is a LEDGER step, the due-watcher is a fold over that
//! step plus its fire, and a synthetic clock is what makes "exactly once, even across a restart"
//! assertable without sleeping five minutes.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, FiberUid, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::{AgentName, LedgerHandle, StepQuery, StepType, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tools::{FailureClass, ToolCall, ToolCallId, ToolName, ToolResult, ToolsHandle};
use bough_plugin_tools_operator::schedule::{ScheduleId, Watcher, FIRED, INTENT};
use bough_plugin_tools_operator::{Clock, OperatorConfig};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

// ---------------------------------------------------------------------------
// the synthetic clock
// ---------------------------------------------------------------------------

struct Synthetic(Mutex<DateTime<Utc>>);

impl Synthetic {
    fn at(s: &str) -> Arc<Synthetic> {
        Arc::new(Synthetic(Mutex::new(
            DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc),
        )))
    }
    fn advance(&self, d: chrono::Duration) {
        let mut t = self.0.lock();
        *t += d;
    }
}

impl Clock for Synthetic {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock()
    }
}

// ---------------------------------------------------------------------------
// the smallest driver that can receive a wake
// ---------------------------------------------------------------------------

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
        "schedule-test-driver"
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
        "schedule-test-driver"
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

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

fn cfg(horizon_days: u32) -> Arc<OperatorConfig> {
    Arc::new(OperatorConfig {
        max_view_bytes: 1_000_000,
        max_files_per_patch: 8,
        bg_log_dir: PathBuf::from("/tmp"),
        bg_max: 4,
        bg_poll_ms: 20,
        ledger_page: 50,
        schedule_max_horizon_days: horizon_days,
        schedule_tick_ms: 10,
        sh_max_legs: 8,
        sh_timeout_ms: 120_000,
        sh_tags_min: 3,
        sh_tags_max: 5,
    })
}

const T0: &str = "2026-08-27T12:00:00Z";

/// Fiber uids must be unique across the whole BINARY: the invariant recorder is a process-global
/// keyed by fiber, and two tests reusing uid 1 would read as one window that fired twice.
static NEXT_FIBER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn fiber_base() -> u64 {
    NEXT_FIBER.fetch_add(100, std::sync::atomic::Ordering::SeqCst)
}

struct Fx {
    fiber_base: u64,
    ctx: Context,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    tools: ToolsHandle,
    clock: Arc<Synthetic>,
    factory: Arc<Factory>,
    _disposer: bough_plugin_agents::AgentDisposer,
    agent: Agent,
}

async fn fixture(horizon_days: u32) -> Fx {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    ledger
        .declare_step_types(&ctx, bough_plugin_tools_operator::schedule::step_types())
        .await
        .expect("the two schedule types are ours to declare");
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
    let factory = Arc::new(Factory::default());
    agents
        .set_factory(&ctx, factory.clone() as Arc<dyn AgentFactory>)
        .await
        .unwrap();
    let clock = Synthetic::at(T0);
    let (agent, _disposer) = agents
        .create(CreateAgent::resident(
            AgentName::new("lane"),
            TrajId::new("t-lane"),
            clock.now(),
        ))
        .await
        .unwrap();
    let tools = ToolsHandle::with_limits(4, 10_000);
    for spec in bough_plugin_tools_operator::specs(
        cfg(horizon_days),
        clock.clone() as Arc<dyn Clock>,
        ledger.clone(),
        Some(agents.clone()),
        bough_plugin_tools_operator::bg::BgJobs::new(cfg(horizon_days), PathBuf::from("/tmp")),
        PathBuf::from("/tmp"),
    ) {
        tools.register(&ctx, spec).await.unwrap();
    }
    Fx {
        fiber_base: fiber_base(),
        ctx,
        ledger,
        agents,
        tools,
        clock,
        factory,
        _disposer,
        agent,
    }
}

impl Fx {
    async fn schedule(&self, at: &str, intent: &str) -> ToolResult {
        self.tools
            .execute(
                &self.ctx,
                vec![ToolCall {
                    id: ToolCallId::new(format!("c-{at}")),
                    name: ToolName::new("schedule"),
                    args: serde_json::json!({ "at": at, "intent": intent }),
                    agent: AgentName::new("lane"),
                    wake: WakeId::new("w1"),
                    step_index: 1,
                }],
            )
            .await
            .pop()
            .expect("one call, one result")
    }

    /// A watcher is a plain object over the ledger: building a SECOND one is exactly what a
    /// restart does, which is how the replay case is tested.
    fn watcher(&self, fiber: u64) -> Watcher {
        Watcher {
            cfg: cfg(30),
            clock: self.clock.clone() as Arc<dyn Clock>,
            ledger: self.ledger.clone(),
            agents: self.agents.clone(),
            fiber: FiberUid(self.fiber_base + fiber),
        }
    }

    async fn count(&self, kind: &str) -> usize {
        self.ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new(kind)],
                ..Default::default()
            })
            .await
            .unwrap()
            .len()
    }
}

// ---------------------------------------------------------------------------
// the cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_intent_at_t_plus_5m_fires_exactly_once_and_wakes_its_creator() {
    let fx = fixture(30).await;
    let out = fx.schedule("+5m", "check the deploy").await;
    assert!(out.ok, "{:?}", out.failure);
    assert_eq!(fx.count(INTENT).await, 1, "the intent is a ledger step");
    assert!(!out.cites.is_empty(), "the tool cites the step it wrote");

    // Not yet due: a tick before the time does nothing at all.
    let watcher = fx.watcher(1);
    assert!(
        watcher.tick().await.unwrap().is_empty(),
        "nothing is due yet"
    );
    assert_eq!(fx.count(FIRED).await, 0);

    fx.clock.advance(chrono::Duration::minutes(6));
    let fired = watcher.tick().await.unwrap();
    assert_eq!(fired.len(), 1, "the due intent fires");
    assert_eq!(fx.count(FIRED).await, 1);

    // It woke the CREATOR, as wake-class mail on the next wake.
    let driver = fx.factory.drivers.lock()[0].clone();
    let notified = driver.notified.lock().clone();
    assert_eq!(notified.len(), 1, "one message reached the agent");
    assert!(
        notified[0].0.contains("scheduled intent"),
        "{:?}",
        notified[0]
    );
    assert!(notified[0].1, "the message asked for a wake");
    assert!(
        fx.agent.has_pending_wake(),
        "the creator has a wake pending"
    );

    // Ticking again at the same instant, and later, changes nothing: fired is a set.
    assert!(watcher.tick().await.unwrap().is_empty());
    fx.clock.advance(chrono::Duration::hours(3));
    assert!(watcher.tick().await.unwrap().is_empty());
    assert_eq!(fx.count(FIRED).await, 1, "an intent fires EXACTLY once");
    assert_eq!(
        driver.notified.lock().len(),
        1,
        "and the creator is woken exactly once"
    );
}

#[tokio::test]
async fn a_restart_replays_the_fold_and_does_not_fire_again() {
    let fx = fixture(30).await;
    assert!(fx.schedule("+5m", "check the deploy").await.ok);
    fx.clock.advance(chrono::Duration::minutes(6));
    assert_eq!(fx.watcher(1).tick().await.unwrap().len(), 1);

    // A fresh watcher with no memory of the first: this is the restart.
    let restarted = fx.watcher(2);
    assert!(
        restarted.tick().await.unwrap().is_empty(),
        "the fired set comes off the LEDGER, so a restart cannot double-fire"
    );
    assert_eq!(fx.count(FIRED).await, 1);
    assert_eq!(fx.factory.drivers.lock()[0].notified.lock().len(), 1);

    // And the row's own invariant agrees.
    let mine: Vec<_> = bough_plugin_tools_operator::invariant::seen()
        .into_iter()
        .filter(|o| o.fiber.0 >= fx.fiber_base && o.fiber.0 < fx.fiber_base + 100)
        .collect();
    assert_eq!(mine.len(), 2, "both watchers recorded a window");
    assert_eq!(
        bough_plugin_tools_operator::invariant::evaluate(&mine),
        Ok(())
    );
}

#[tokio::test]
async fn a_horizon_beyond_the_bound_is_refused() {
    let fx = fixture(7).await;
    let out = fx.schedule("+30d", "much too far away").await;
    assert!(!out.ok, "beyond the horizon is refused");
    let f = out.failure.expect("a refusal carries a failure");
    assert_eq!(f.kind, FailureClass::Denied);
    assert!(f.message.contains("7-day"), "{}", f.message);
    assert_eq!(fx.count(INTENT).await, 0, "a refusal writes nothing");

    // Inside the bound it is accepted.
    assert!(fx.schedule("+2d", "soon enough").await.ok);
    assert_eq!(fx.count(INTENT).await, 1);
}

#[tokio::test]
async fn an_unparseable_instant_is_refused_before_any_append() {
    let fx = fixture(30).await;
    let out = fx.schedule("next tuesday", "vague").await;
    assert!(!out.ok);
    assert_eq!(fx.count(INTENT).await, 0);
}

/// The fold, in the small: two intents, one due, one fired already.
#[tokio::test]
async fn only_due_and_unfired_intents_are_selected() {
    use bough_plugin_tools_operator::schedule::{due, Pending, ScheduleIntentBody};
    let t0 = DateTime::parse_from_rfc3339(T0)
        .unwrap()
        .with_timezone(&Utc);
    let mk = |id: &str, mins: i64| Pending {
        body: ScheduleIntentBody {
            id: ScheduleId::new(id),
            agent: AgentName::new("lane"),
            at: t0 + chrono::Duration::minutes(mins),
            intent: "x".into(),
        },
        traj: TrajId::new("t-lane"),
        step: bough_plugin_ledger::StepId::new(format!("s-{id}")),
    };
    let all = vec![mk("a", -1), mk("b", 5), mk("c", -2)];
    let fired: BTreeSet<ScheduleId> = [ScheduleId::new("c")].into_iter().collect();
    let picked = due(&all, &fired, t0);
    assert_eq!(picked.len(), 1);
    assert_eq!(picked[0].body.id, ScheduleId::new("a"));
}

/// A declared inject key is a reload trigger, so declaring one `apply` never reads remounts the
/// row (killing every live `bg` job with it) the first time some other bundle supplies a provider
/// for it. `apply` reads `tools`, `ledger`, `workspace` and optionally `agents` — and nothing else.
#[test]
fn the_row_declares_only_the_inject_keys_apply_reads() {
    use bough_kernel::Plugin;
    use bough_plugin_tools_operator::OperatorPlugin;
    let inject = OperatorPlugin::inject();
    assert_eq!(
        inject.required.iter().cloned().collect::<Vec<_>>(),
        vec!["ledger".to_string(), "tools".into(), "workspace".into()],
    );
    assert_eq!(
        inject.optional.iter().cloned().collect::<Vec<_>>(),
        vec!["agents".to_string()],
        "`mail` and `schedule` are read nowhere in `apply`",
    );
}
