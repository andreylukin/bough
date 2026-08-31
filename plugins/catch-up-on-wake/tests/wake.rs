//! §13's wake, driven synthetically: a `WillSleep`/`DidWake` pair fired through `power-test`
//! reaches the row's own listener and produces exactly one catch-up wake per resident — none for a
//! worker, none for a disposed agent, none for a nap under `min_sleep_ms`, and none for a second
//! `DidWake` that arrives while the first catch-up is still in flight.
//!
//! The listener the test fires at is [`bough_plugin_catch_up_on_wake::listen`], the one `apply`
//! registers; the driver is a recorder, so no model is involved.

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDisposer, AgentDriver, AgentError, AgentFactory, AgentKind,
    AgentsHandle, Attach, CancelCause, CreateAgent, InboxReceipt, MailClass, Message, Sender,
    Target, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_catch_up_on_wake::{eligible, CatchUpOnWake, CatchUpOnWakeConfig};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_power::PowerEvent;
use bough_plugin_power_test::PowerTestHandle;
use parking_lot::Mutex;

#[derive(Default)]
struct Recorder {
    wakes: Mutex<Vec<(String, WakeCause)>>,
}

struct RecordingDriver {
    name: String,
    rec: Arc<Recorder>,
}

#[async_trait::async_trait]
impl AgentDriver for RecordingDriver {
    fn driver(&self) -> &'static str {
        "recording-loop"
    }
    async fn notify(&self, _receipt: &InboxReceipt, _msg: &Message) {}
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _kind: WakeKind, cause: WakeCause) -> WakeRequest {
        self.rec.wakes.lock().push((self.name.clone(), cause));
        WakeRequest::Started(bough_plugin_ledger::WakeId::new("w-1"))
    }
}

struct RecordingFactory {
    rec: Arc<Recorder>,
}

#[async_trait::async_trait]
impl AgentFactory for RecordingFactory {
    fn driver(&self) -> &'static str {
        "recording-loop"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        Ok(Arc::new(RecordingDriver {
            name: cell.agent().name().to_string(),
            rec: Arc::clone(&self.rec),
        }) as Arc<dyn AgentDriver>)
    }
}

struct Fixture {
    ctx: Context,
    fiber: bough_kernel::FiberUid,
    agents: AgentsHandle,
    rec: Arc<Recorder>,
    /// Held so the roster stays up for the length of the test.
    held: Mutex<Vec<AgentDisposer>>,
}

async fn fixture() -> Fixture {
    let core = KernelCore::new();
    let fiber = core.new_fiber_uid();
    let ctx = Context::root(core);
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
    let rec = Arc::new(Recorder::default());
    agents
        .set_factory(
            &ctx,
            Arc::new(RecordingFactory {
                rec: Arc::clone(&rec),
            }) as Arc<dyn AgentFactory>,
        )
        .await
        .expect("the slot is free");
    Fixture {
        ctx,
        fiber,
        agents,
        rec,
        held: Mutex::new(Vec::new()),
    }
}

impl Fixture {
    async fn agent(&self, name: &str, kind: AgentKind) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent {
                name: AgentName::new(name),
                traj: TrajId::new(format!("lane/{name}")),
                kind,
                scope: None,
                setup: None,
                seed: Vec::new(),
                at: chrono::Utc::now(),
            })
            .await
            .expect("the agent is created");
        self.held.lock().push(disposer);
        agent
    }

    /// Queue ordinary mail so `request_wake` has something to be over.
    async fn queue_mail(&self, agent: &Agent) {
        let mut msg = Message::new(Sender::Andrey, "hello", "hello", chrono::Utc::now());
        msg.class = MailClass::Ordinary;
        msg.from = Sender::Agent(AgentName::new("terra"));
        agent
            .send(msg, Target::NextWake, false)
            .await
            .expect("the mail lands");
    }

    fn state(&self, min_sleep_ms: u64) -> Arc<CatchUpOnWake> {
        Arc::new(CatchUpOnWake::new(
            Arc::new(CatchUpOnWakeConfig {
                min_sleep_ms,
                kinds: vec!["resident".to_string()],
            }),
            self.agents.clone(),
            self.fiber,
        ))
    }

    fn woken(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .rec
            .wakes
            .lock()
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        v.sort();
        v
    }
}

#[tokio::test]
async fn a_synthetic_pair_wakes_every_resident_exactly_once() {
    let f = fixture().await;
    let sol = f.agent("sol", AgentKind::Resident).await;
    let luna = f.agent("luna", AgentKind::Resident).await;
    f.queue_mail(&sol).await;
    f.queue_mail(&luna).await;

    let state = f.state(60_000);
    bough_plugin_catch_up_on_wake::listen(&f.ctx, Arc::clone(&state))
        .await
        .expect("the listener registers");

    let power = PowerTestHandle::new(f.ctx.clone());
    power.sleep().await;
    assert!(
        f.woken().is_empty(),
        "a `WillSleep` wakes nobody: {:?}",
        f.woken()
    );

    power.wake(Some(Duration::from_secs(8 * 3600))).await;

    assert_eq!(f.woken(), vec!["luna".to_string(), "sol".to_string()]);
    assert_eq!(
        f.rec.wakes.lock()[0].1,
        WakeCause::CatchUp,
        "the cause is attribution, and it is CatchUp"
    );
}

#[tokio::test]
async fn a_worker_and_a_disposed_agent_get_none() {
    let f = fixture().await;
    let worker = f.agent("w-1", AgentKind::Worker).await;
    f.queue_mail(&worker).await;
    let doomed = f.agent("gone", AgentKind::Resident).await;
    f.queue_mail(&doomed).await;

    let kinds = vec!["resident".to_string()];
    assert!(!eligible(&worker, &kinds), "a worker is not a resident");

    // Disposing the row's handle is what "disposed" means here. Only `gone`'s is taken; the
    // worker's stays in `held` so it is still live when the roster is walked.
    let mut kept: Vec<AgentDisposer> = Vec::new();
    let mut doomed_disposer: Option<AgentDisposer> = None;
    for d in std::mem::take(&mut *f.held.lock()) {
        if d.agent().name().to_string() == "gone" {
            doomed_disposer = Some(d);
        } else {
            kept.push(d);
        }
    }
    *f.held.lock() = kept;
    doomed_disposer.expect("`gone` was created").dispose().await;
    assert!(doomed.is_disposed(), "the handle is terminal");
    assert!(
        !eligible(&doomed, &kinds),
        "a disposed agent is not eligible"
    );

    let state = f.state(60_000);
    let woke = state
        .on_wake(&PowerEvent::DidWake {
            at: chrono::Utc::now(),
            asleep_for: Some(Duration::from_secs(8 * 3600)),
        })
        .await;
    assert!(woke.is_empty(), "nothing eligible was woken: {woke:?}");
    assert!(
        f.woken().is_empty(),
        "and no driver was asked: {:?}",
        f.woken()
    );
}

#[tokio::test]
async fn a_second_wake_during_an_in_flight_catch_up_is_dropped() {
    let f = fixture().await;
    let sol = f.agent("sol", AgentKind::Resident).await;
    f.queue_mail(&sol).await;
    let state = f.state(60_000);

    let ev = |at: chrono::DateTime<chrono::Utc>| PowerEvent::DidWake {
        at,
        asleep_for: Some(Duration::from_secs(8 * 3600)),
    };

    let first = state.on_wake(&ev(chrono::Utc::now())).await;
    assert_eq!(first.len(), 1, "the first wake lands");
    // MERGE (note 12): `on_wake` reports the WAKE it opened beside the agent, so the caller can
    // wait for exactly that wake rather than for the agent to fall idle.
    let woken_ids: Vec<_> = first.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(state.in_flight(), woken_ids, "and is held in flight");

    let second = state.on_wake(&ev(chrono::Utc::now())).await;
    assert!(second.is_empty(), "the second is dropped: {second:?}");
    assert_eq!(f.woken().len(), 1, "the driver was asked exactly once");

    // The catch-up finishing reopens the window.
    state.finish(&first[0].0);
    assert!(state.in_flight().is_empty());
    let third = state.on_wake(&ev(chrono::Utc::now())).await;
    assert_eq!(
        third.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        woken_ids,
        "a later wake asks again"
    );
    assert_eq!(f.woken().len(), 2);
}

#[tokio::test]
async fn a_nap_under_the_floor_wakes_nobody() {
    let f = fixture().await;
    let sol = f.agent("sol", AgentKind::Resident).await;
    f.queue_mail(&sol).await;
    let state = f.state(60_000);

    let woke = state
        .on_wake(&PowerEvent::DidWake {
            at: chrono::Utc::now(),
            asleep_for: Some(Duration::from_secs(10)),
        })
        .await;
    assert!(woke.is_empty(), "ten seconds is not a night away");
    assert!(f.woken().is_empty());
}

#[tokio::test]
async fn nothing_queued_is_no_wake_and_leaves_nothing_in_flight() {
    // A driver that answers `Nothing` is what an agent with an empty inbox looks like from here.
    struct Silent;
    #[async_trait::async_trait]
    impl AgentDriver for Silent {
        fn driver(&self) -> &'static str {
            "silent"
        }
        async fn notify(&self, _r: &InboxReceipt, _m: &Message) {}
        async fn cancel(&self, _c: CancelCause, _k: bool) {}
        async fn stop(&self) {}
        async fn wake_now(&self, _k: WakeKind, _c: WakeCause) -> WakeRequest {
            WakeRequest::Nothing
        }
    }
    struct SilentFactory;
    #[async_trait::async_trait]
    impl AgentFactory for SilentFactory {
        fn driver(&self) -> &'static str {
            "silent"
        }
        async fn attach(
            &self,
            _cell: AgentCell,
            _mode: Attach,
        ) -> Result<Arc<dyn AgentDriver>, AgentError> {
            Ok(Arc::new(Silent) as Arc<dyn AgentDriver>)
        }
    }

    let core = KernelCore::new();
    let fiber = core.new_fiber_uid();
    let ctx = Context::root(core);
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = AgentsHandle::new(ctx.clone(), ledger);
    agents
        .set_factory(&ctx, Arc::new(SilentFactory) as Arc<dyn AgentFactory>)
        .await
        .expect("the slot is free");
    let (_agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new("sol"),
            traj: TrajId::new("lane/sol"),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("created");

    let state = Arc::new(CatchUpOnWake::new(
        Arc::new(CatchUpOnWakeConfig {
            min_sleep_ms: 60_000,
            kinds: vec!["resident".to_string()],
        }),
        agents,
        fiber,
    ));
    let woke = state
        .on_wake(&PowerEvent::DidWake {
            at: chrono::Utc::now(),
            asleep_for: Some(Duration::from_secs(8 * 3600)),
        })
        .await;
    assert!(woke.is_empty(), "`Nothing` is not a wake");
    assert!(
        state.in_flight().is_empty(),
        "and it leaves nothing in flight, so the next wake may ask again"
    );
    disposer.dispose().await;
}

/// The in-flight window is closed by an effect THE ROW OWNS, not by a bare `tokio::spawn`.
///
/// The recording driver reports a wake it never finishes, so the agent this row woke is never
/// idle and the closer is still waiting when the row goes down. Disposing the listener must
/// nonetheless reach quiescence and must release the claim — before this fix the wait was owned
/// by nobody, so it survived the fiber, kept an `Arc<CatchUpOnWake>` alive and held its
/// `in_flight` entry for the life of the process.
#[tokio::test(flavor = "multi_thread")]
async fn disposing_the_row_mid_catch_up_reaches_quiescence_and_releases_the_claim() {
    let f = fixture().await;
    let sol = f.agent("sol", AgentKind::Resident).await;
    f.queue_mail(&sol).await;
    let state = f.state(60_000);
    let handle = bough_plugin_catch_up_on_wake::listen(&f.ctx, Arc::clone(&state))
        .await
        .expect("the listener registers");

    let power = PowerTestHandle::new(f.ctx.clone());
    power.wake(Some(Duration::from_secs(8 * 3600))).await;
    assert_eq!(f.woken(), vec!["sol".to_string()], "the catch-up was asked");
    assert_eq!(state.in_flight().len(), 1, "and the claim is held open");

    // The row goes down while the wake it opened is still running: the listener, and then the
    // window-closers the listener opened — which is exactly what unwinding the row's fiber does,
    // and what a bare `tokio::spawn` would have been left out of.
    tokio::time::timeout(Duration::from_secs(5), handle.dispose())
        .await
        .expect("disposing the listener reaches quiescence");
    tokio::time::timeout(Duration::from_secs(5), state.close_all())
        .await
        .expect("disposal reaches quiescence rather than hanging on a wake that never ends");

    assert!(
        state.in_flight().is_empty(),
        "the claim left with the row: {:?}",
        state.in_flight()
    );
}
