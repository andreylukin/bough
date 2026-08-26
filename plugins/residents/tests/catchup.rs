//! §5's catch-up at launch, against a recording driver: the roster comes up, exactly one wake is
//! asked for per agent that has queued mail, none at all for an agent that does not, and
//! disabling the row tears the roster down without touching the ledger (V6, P3-D17).

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    InboxReceipt, MailClass, Message, Sender, Target, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::{AgentName, LedgerHandle, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_residents::{catch_up, hold_roster, raise_roster, ResidentsConfig, Roster};
use parking_lot::Mutex;

/// A driver that records what the seam asked of it. `wake_now` answers `Started` so the test can
/// see the request land; whether a real loop would have work is `agent-loop`'s business.
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
        WakeRequest::Started(WakeId::new("w-1"))
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
    /// This fixture's own fiber. `Context::root` is always `FiberUid(0)`, so every test in this
    /// binary would otherwise record into ONE stream and the global assertions would race.
    fiber: bough_kernel::FiberUid,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    rec: Arc<Recorder>,
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
        ledger,
        agents,
        rec,
    }
}

/// Only what THIS fixture's fiber recorded.
fn mine(f: &Fixture) -> Vec<bough_plugin_residents::invariant::Obs> {
    bough_plugin_residents::invariant::seen()
        .into_iter()
        .filter(|o| o.fiber == f.fiber)
        .collect()
}

fn cfg(bootstrap: &[&str]) -> ResidentsConfig {
    ResidentsConfig {
        bootstrap: bootstrap.iter().map(|s| s.to_string()).collect(),
        traj_prefix: "lane/".to_string(),
        resume_all: true,
        catch_up: true,
    }
}

/// Put an `agents` row in the ledger without a live handle: what a previous run left behind.
async fn seed_row(ledger: &LedgerHandle, name: &str) {
    ledger
        .0
        .put_agent(bough_plugin_ledger::AgentRow {
            name: AgentName::new(name),
            traj: TrajId::new(format!("lane/{name}")),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the row lands");
}

#[tokio::test]
async fn every_agent_row_is_resumed_at_launch() {
    let f = fixture().await;
    seed_row(&f.ledger, "sol").await;
    seed_row(&f.ledger, "terra").await;

    let roster = Arc::new(Roster::default());
    let up = raise_roster(&f.agents, &f.ledger, &cfg(&[]), &roster)
        .await
        .expect("the roster comes up");

    let mut names: Vec<String> = up.iter().map(|n| n.to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["sol".to_string(), "terra".to_string()]);
    assert_eq!(roster.len(), 2, "both disposers are held");
    assert_eq!(f.agents.list().len(), 2, "both agents are live");
}

#[tokio::test]
async fn bootstrap_creates_the_first_lane_only_when_the_ledger_has_none() {
    let f = fixture().await;
    let roster = Arc::new(Roster::default());
    raise_roster(&f.agents, &f.ledger, &cfg(&["sol"]), &roster)
        .await
        .expect("the roster comes up");

    let rows = f.ledger.0.agents().await.expect("rows");
    assert_eq!(rows.len(), 1, "one lane was bootstrapped");
    assert_eq!(rows[0].name.to_string(), "sol");
    assert_eq!(rows[0].traj.to_string(), "lane/sol");
    let first_traj = rows[0].traj.clone();

    // Second launch: the row exists, so the name is RESUMED and no new lane is minted.
    roster.dispose_all().await;
    let roster2 = Arc::new(Roster::default());
    raise_roster(&f.agents, &f.ledger, &cfg(&["sol"]), &roster2)
        .await
        .expect("the roster comes up again");
    let rows = f.ledger.0.agents().await.expect("rows");
    assert_eq!(rows.len(), 1, "no second lane");
    assert_eq!(
        rows[0].traj, first_traj,
        "the lane is the one from launch 1"
    );
}

#[tokio::test]
async fn no_wake_when_nothing_is_queued() {
    let f = fixture().await;
    seed_row(&f.ledger, "sol").await;
    let roster = Arc::new(Roster::default());
    let up = raise_roster(&f.agents, &f.ledger, &cfg(&[]), &roster)
        .await
        .expect("the roster comes up");

    catch_up(&f.agents, &up, f.fiber)
        .await
        .expect("catch-up runs");

    assert!(
        f.rec.wakes.lock().is_empty(),
        "an empty inbox produces no wake at all (V6)"
    );
    assert!(
        mine(&f).is_empty(),
        "nothing was requested, so nothing was recorded"
    );
}

#[tokio::test]
async fn one_catch_up_wake_per_agent_with_queued_mail() {
    let f = fixture().await;
    seed_row(&f.ledger, "sol").await;
    seed_row(&f.ledger, "terra").await;
    let roster = Arc::new(Roster::default());
    let up = raise_roster(&f.agents, &f.ledger, &cfg(&[]), &roster)
        .await
        .expect("the roster comes up");

    // Queue ordinary mail on `sol` only.
    let sol = f
        .agents
        .by_name(&AgentName::new("sol"))
        .expect("sol is live");
    let mut msg = Message::new(Sender::Andrey, "hello", "hello", chrono::Utc::now());
    msg.class = MailClass::Ordinary;
    msg.from = Sender::Agent(AgentName::new("terra"));
    sol.send(msg, Target::NextWake, false)
        .await
        .expect("the mail lands");

    catch_up(&f.agents, &up, f.fiber)
        .await
        .expect("catch-up runs");

    let wakes = f.rec.wakes.lock().clone();
    assert_eq!(wakes.len(), 1, "exactly one catch-up wake: {wakes:?}");
    assert_eq!(wakes[0].0, "sol");
    assert_eq!(wakes[0].1, WakeCause::CatchUp);
    assert_eq!(
        bough_plugin_residents::invariant::check_stream(&mine(&f)),
        Ok(()),
        "at most one catch-up per agent per activation"
    );
}

#[tokio::test]
async fn disabling_the_row_disposes_the_roster_and_leaves_the_ledger_untouched() {
    let f = fixture().await;
    seed_row(&f.ledger, "sol").await;
    let roster = Arc::new(Roster::default());
    let held = hold_roster(&f.ctx, Arc::clone(&roster), f.fiber)
        .await
        .expect("the effect registers");
    raise_roster(&f.agents, &f.ledger, &cfg(&[]), &roster)
        .await
        .expect("the roster comes up");
    assert_eq!(f.agents.list().len(), 1);
    let before = f.ledger.0.agents().await.expect("rows");

    // Disabling the row IS disposing its effect.
    held.dispose().await;

    assert!(
        f.agents.list().is_empty(),
        "the roster went down with the row"
    );
    assert!(roster.is_empty(), "the disposers were consumed");
    let after = f.ledger.0.agents().await.expect("rows");
    assert_eq!(
        before.len(),
        after.len(),
        "the ledger is untouched: a trajectory outlives its handle"
    );
    assert_eq!(after[0].name.to_string(), "sol");
}
