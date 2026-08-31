//! §5/§8 and P6-D2: what the two system passes do on a fire, driven through the MANUAL schedule
//! Provider so the test owns the clock.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::WakeKind;
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentKind, AgentsHandle, Attach,
    CancelCause, CreateAgent, InboxReceipt, Message, WakeCause, WakeRequest,
};
use bough_plugin_commands::{CommandsConfig, CommandsHandle};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_schedule::{
    Cadence, FireReason, Job, JobFire, JobName, JobOutcome, JobSpec, Scheduler,
};
use bough_plugin_schedule_manual::ManualScheduler;
use bough_plugin_system_schedules::{CatchUpJob, ReconsolidateJob, RECONSOLIDATE_JOB};
use parking_lot::Mutex;

/// The crate's sweep recorder is process-wide, so the two catch-up tests take turns.
static SWEEPS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A driver that only counts the catch-up wakes it was asked for.
#[derive(Default)]
struct CountingDriver {
    wakes: Mutex<Vec<(WakeKind, WakeCause)>>,
}

#[async_trait::async_trait]
impl AgentDriver for CountingDriver {
    fn driver(&self) -> &'static str {
        "counting-loop"
    }
    async fn notify(&self, _receipt: &InboxReceipt, _msg: &Message) {}
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, kind: WakeKind, cause: WakeCause) -> WakeRequest {
        self.wakes.lock().push((kind, cause));
        WakeRequest::Nothing
    }
}

#[derive(Default)]
struct CountingFactory {
    drivers: Mutex<Vec<(String, Arc<CountingDriver>)>>,
}

impl CountingFactory {
    fn wakes(&self, name: &str) -> usize {
        self.drivers
            .lock()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, d)| d.wakes.lock().len())
            .sum()
    }
}

#[async_trait::async_trait]
impl AgentFactory for CountingFactory {
    fn driver(&self) -> &'static str {
        "counting-loop"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        let driver = Arc::new(CountingDriver::default());
        self.drivers
            .lock()
            .push((cell.agent().name().to_string(), driver.clone()));
        Ok(driver as Arc<dyn AgentDriver>)
    }
}

struct Fixture {
    ctx: Context,
    agents: AgentsHandle,
    factory: Arc<CountingFactory>,
}

async fn fixture() -> Fixture {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    let agents = AgentsHandle::new(ctx.clone(), ledger);
    let factory = Arc::new(CountingFactory::default());
    agents
        .set_factory(&ctx, factory.clone() as Arc<dyn AgentFactory>)
        .await
        .expect("the slot is free");
    Fixture {
        ctx,
        agents,
        factory,
    }
}

async fn make(
    f: &Fixture,
    name: &str,
    kind: AgentKind,
) -> (Agent, bough_plugin_agents::AgentDisposer) {
    let mut req = CreateAgent::resident(
        AgentName::new(name),
        TrajId::new(format!("t-{name}")),
        chrono::Utc::now(),
    );
    req.kind = kind;
    f.agents.create(req).await.expect("an agent")
}

fn fire(name: &str) -> JobFire {
    JobFire {
        name: JobName::new(name),
        at: chrono::Utc::now(),
        scheduled_for: chrono::Utc::now(),
        reason: FireReason::Manual,
    }
}

#[tokio::test]
async fn the_catch_up_pass_asks_every_resident_once_and_nobody_else() {
    let _turn = SWEEPS.lock().await;
    bough_plugin_system_schedules::invariant::forget();
    let f = fixture().await;
    let _sol = make(&f, "sol", AgentKind::Resident).await;
    let _terra = make(&f, "terra", AgentKind::Resident).await;
    let _worker = make(&f, "w1", AgentKind::Worker).await;

    let job = CatchUpJob {
        agents: f.agents.clone(),
        kinds: vec!["resident".to_string()],
    };
    let outcome = job.run(fire("system:catch-up")).await;
    assert!(matches!(outcome, JobOutcome::Ran { .. }), "{outcome:?}");

    assert_eq!(f.factory.wakes("sol"), 1);
    assert_eq!(f.factory.wakes("terra"), 1);
    assert_eq!(f.factory.wakes("w1"), 0, "a worker is not a resident");

    // …and one wake per agent per fire, which is the crate's own invariant.
    let sweeps = bough_plugin_system_schedules::invariant::sweeps();
    assert_eq!(sweeps.len(), 1);
    assert_eq!(sweeps[0].eligible, 2);
    assert_eq!(sweeps[0].asked, 2);
    bough_plugin_system_schedules::invariant::evaluate_sweeps(&sweeps).expect("one each");
}

#[tokio::test]
async fn a_disposed_agent_is_never_asked_for_a_catch_up_wake() {
    let _turn = SWEEPS.lock().await;
    bough_plugin_system_schedules::invariant::forget();
    let f = fixture().await;
    let (_sol, _keep) = make(&f, "sol", AgentKind::Resident).await;
    let (_gone, disposer) = make(&f, "gone", AgentKind::Resident).await;
    disposer.dispose().await;

    let job = CatchUpJob {
        agents: f.agents.clone(),
        kinds: vec!["resident".to_string()],
    };
    job.run(fire("system:catch-up")).await;

    assert_eq!(f.factory.wakes("sol"), 1);
    assert_eq!(f.factory.wakes("gone"), 0, "a disposed agent is terminal");
}

#[tokio::test]
async fn the_reconsolidation_pass_is_pending_with_no_commands_seam_and_the_job_stays_registered() {
    let f = fixture().await;
    let sched = ManualScheduler::new_in(f.ctx.clone());
    sched
        .register(
            &f.ctx,
            JobSpec {
                name: JobName::new(RECONSOLIDATE_JOB),
                cadence: Cadence::Every {
                    every_ms: 3_600_000,
                },
                catch_up: false,
                job: Arc::new(ReconsolidateJob {
                    ctx: f.ctx.clone(),
                    commands: None,
                    agents: f.agents.clone(),
                    command: "reconsolidate".into(),
                    agent: None,
                }),
            },
        )
        .await
        .expect("registered");

    // THREE fires, and the row is still there after every one of them (P6-D2).
    for _ in 0..3 {
        let run = sched
            .fire_now(&JobName::new(RECONSOLIDATE_JOB))
            .await
            .expect("fired");
        match &run.outcome {
            JobOutcome::Pending { reason } => assert!(reason.contains("commands seam"), "{reason}"),
            other => panic!("expected Pending, got {other:?}"),
        }
        bough_plugin_system_schedules::invariant::evaluate_outcome(&run.outcome)
            .expect("pending never fails the row");
        assert_eq!(sched.jobs().len(), 1, "the job stays registered");
    }
}

#[tokio::test]
async fn the_reconsolidation_pass_is_pending_while_the_command_does_not_exist() {
    let f = fixture().await;
    let commands = CommandsHandle::new(
        f.ctx.clone(),
        Arc::new(CommandsConfig {
            prefix: '/',
            suggestions: false,
        }),
    );
    let job = ReconsolidateJob {
        ctx: f.ctx.clone(),
        commands: Some(commands),
        agents: f.agents.clone(),
        command: "reconsolidate".into(),
        agent: None,
    };
    for _ in 0..3 {
        let outcome = job.run(fire(RECONSOLIDATE_JOB)).await;
        match &outcome {
            JobOutcome::Pending { reason } => {
                assert!(
                    reason.contains("no command named `reconsolidate`"),
                    "{reason}"
                )
            }
            other => panic!("expected Pending, got {other:?}"),
        }
        bough_plugin_system_schedules::invariant::evaluate_outcome(&outcome)
            .expect("pending never fails the row");
    }
}
