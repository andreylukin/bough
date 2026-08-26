//! The shared fixture: a kernel context, a memory ledger, live agents behind a recording driver,
//! and the recording `gh` shim first in the collector's own environment — so an unplanned `gh`
//! call is a red test rather than a network request.

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_collector_github::{GithubCollector, GithubCollectorConfig};
use bough_plugin_gh_cli::shim::fixture_name;
use bough_plugin_ledger::{AgentName, LedgerHandle, Order, Step, StepQuery, StepType, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_schedule::Cadence;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

pub fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
}

pub fn traj_of(name: &str) -> TrajId {
    TrajId::new(format!("lane/{name}"))
}

pub struct Fx {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub dir: tempfile::TempDir,
    pub shim_dir: std::path::PathBuf,
    pub state_db: std::path::PathBuf,
    _factory: Option<Arc<StubFactory>>,
}

impl Fx {
    pub async fn new() -> Fx {
        let mut fx = Fx::new_without_factory().await;
        let factory = Arc::new(StubFactory::default());
        std::mem::forget(
            fx.agents
                .set_factory(&fx.ctx, factory.clone() as Arc<dyn AgentFactory>)
                .await
                .expect("the slot is free"),
        );
        fx._factory = Some(factory);
        fx
    }

    /// The fixture WITHOUT a factory in the slot, for a test that installs its own driver.
    pub async fn new_without_factory() -> Fx {
        let ctx = Context::root(KernelCore::new());
        let dir = tempfile::tempdir().expect("a temp dir");
        let shim_dir = dir.path().join("gh");
        std::fs::create_dir_all(&shim_dir).expect("the shim dir");
        let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
        let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
        Fx {
            state_db: dir.path().join("collect-github.db"),
            ctx,
            ledger,
            agents,
            dir,
            shim_dir,
            _factory: None,
        }
    }

    pub async fn agent(&self, name: &str) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                traj_of(name),
                at(),
            ))
            .await
            .expect("a fresh agent");
        std::mem::forget(disposer);
        agent
    }

    /// A fixture answer for exactly one `gh` argv. `ext` is `json` (stdout, exit 0) or `err`.
    pub fn gh_fixture(&self, args: &[&str], ext: &str, body: &str) {
        std::fs::write(
            self.shim_dir.join(format!("{}.{ext}", fixture_name(args))),
            body,
        )
        .expect("a fixture");
    }

    pub fn gh_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.to_string())
            .collect()
    }

    pub fn cfg(&self) -> GithubCollectorConfig {
        GithubCollectorConfig {
            cadence: Cadence::Every { every_ms: 300_000 },
            gh_bin: concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fixtures/gh/gh")
                .to_string(),
            repos: vec!["o/r".to_string()],
            prs: true,
            review_requests: true,
            mentions: false,
            checks: false,
            deliver_to: vec!["sol".to_string()],
            wake_classes: vec![bough_plugin_collect_core::WakeClass::ReviewRequest],
            known_bots: vec!["dependabot[bot]".to_string()],
            state_db: self.state_db.clone(),
            batch: 50,
            timeout_ms: 10_000,
        }
    }

    /// A collector over the CURRENT config. Building a second one over the same `state_db` is
    /// what a restart looks like from the row's side.
    pub fn collector(&self, cfg: GithubCollectorConfig) -> GithubCollector {
        GithubCollector::open(Arc::new(cfg), self.ledger.clone(), self.agents.clone())
            .expect("the row activates")
            .with_gh_env(vec![
                (
                    "GH_SHIM_DIR".to_string(),
                    self.shim_dir.display().to_string(),
                ),
                (
                    "GH_SHIM_LOG".to_string(),
                    self.dir.path().join("argv.log").display().to_string(),
                ),
            ])
    }

    /// Every durable `inbox/spliced` claim on that agent's trajectory.
    pub async fn claims(&self, agent: &str) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj_of(agent)],
                kinds: vec![StepType::new("inbox/spliced")],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("a read")
            .into_iter()
            .filter(|s| s.body["op"].as_str() == Some("claim"))
            .collect()
    }

    pub async fn delivered(&self, agent: &str) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj_of(agent)],
                kinds: vec![StepType::new("mail/delivered")],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("a read")
    }
}

// ---- a driver that records, and never runs a wake ---------------------------------------------

#[derive(Default)]
pub struct StubFactory {
    pub drivers: Mutex<Vec<Arc<StubDriver>>>,
}

#[async_trait::async_trait]
impl AgentFactory for StubFactory {
    fn driver(&self) -> &'static str {
        "collector-github-test-driver"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        let d = Arc::new(StubDriver {
            _cell: cell,
            notified: Mutex::new(Vec::new()),
            woken: Mutex::new(Vec::new()),
        });
        self.drivers.lock().push(d.clone());
        Ok(d as Arc<dyn AgentDriver>)
    }
}

pub struct StubDriver {
    _cell: AgentCell,
    pub notified: Mutex<Vec<String>>,
    pub woken: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AgentDriver for StubDriver {
    fn driver(&self) -> &'static str {
        "collector-github-test-driver"
    }
    async fn notify(&self, _receipt: &InboxReceipt, msg: &Message) {
        self.notified.lock().push(msg.subject.clone());
    }
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        self.woken.lock().push("wake".to_string());
        WakeRequest::Nothing
    }
}
