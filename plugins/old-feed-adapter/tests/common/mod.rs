//! The shared fixture: a kernel context, a ledger (either provider), a live agent behind a
//! recording driver, and FIXTURE DATABASES built with `rusqlite` — the jungler db does not exist
//! on this machine, so the fixture is authoritative for the shape the adapter reads (§2.6).

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::{AgentName, LedgerHandle, Order, Step, StepQuery, StepType, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};
use bough_plugin_old_feed_adapter::{OldFeedConfig, OldFeedHandle};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::Connection;

/// Which ledger provider a case runs against. The tier-1 projection case runs both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Memory,
    Sqlite,
}

pub struct Fx {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    pub dir: tempfile::TempDir,
    pub jungler_db: std::path::PathBuf,
    pub bough_db: std::path::PathBuf,
    pub state_db: std::path::PathBuf,
    _factory: Arc<StubFactory>,
}

pub fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
}

pub fn traj() -> TrajId {
    TrajId::new("lane/sol")
}

pub fn sol() -> AgentName {
    AgentName::new("sol")
}

impl Fx {
    pub async fn new(which: Which) -> Fx {
        let ctx = Context::root(KernelCore::new());
        let dir = tempfile::tempdir().expect("a temp dir");
        let ledger = match which {
            Which::Memory => LedgerHandle(MemoryStore::new(ctx.clone())),
            Which::Sqlite => LedgerHandle(
                SqliteStore::open(
                    &SqliteConfig {
                        path: dir.path().join("ledger.db"),
                        busy_timeout_ms: 5_000,
                    },
                    ctx.clone(),
                )
                .expect("a fresh ledger"),
            ),
        };
        let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
        let factory = Arc::new(StubFactory::default());
        std::mem::forget(
            agents
                .set_factory(&ctx, factory.clone() as Arc<dyn AgentFactory>)
                .await
                .expect("the slot is free"),
        );
        Fx {
            jungler_db: dir.path().join("jungler.db"),
            bough_db: dir.path().join("bough.db"),
            state_db: dir.path().join("old-feed.db"),
            ctx,
            ledger,
            agents,
            dir,
            _factory: factory,
        }
    }

    /// The receiving agent. Its creation writes the `agents` row the sweep reads the traj from.
    pub async fn sol_agent(&self) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(sol(), traj(), at()))
            .await
            .expect("a fresh agent");
        std::mem::forget(disposer);
        agent
    }

    pub fn cfg(&self) -> OldFeedConfig {
        OldFeedConfig {
            jungler_db: self.jungler_db.clone(),
            bough_db: self.bough_db.clone(),
            state_db: self.state_db.clone(),
            poll_ms: 30_000,
            batch: 200,
            deliver_to: "sol".to_string(),
            priming_limit: 40,
            tier1: true,
        }
    }

    /// A handle over the CURRENT config. Building a second one over the same `state_db` is what a
    /// restart looks like from the adapter's side.
    pub fn feed(&self, cfg: OldFeedConfig) -> OldFeedHandle {
        OldFeedHandle::open(Arc::new(cfg), self.ledger.clone(), self.agents.clone())
            .expect("the row activates")
    }

    pub async fn steps_of_kind(&self, kind: &str) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj()],
                kinds: vec![StepType::new(kind)],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("a read")
    }

    pub async fn all_steps(&self) -> Vec<Step> {
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
}

// ---- the fixture databases --------------------------------------------------------------------

/// The jungler shape §2.6 documents, with three events, three nodes and three story sections.
pub fn standard_jungler(path: &std::path::Path) {
    let conn = Connection::open(path).expect("a fresh jungler db");
    conn.execute_batch(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, at INTEGER, kind TEXT, subject TEXT,
                              body TEXT, ref TEXT, url TEXT, lane TEXT);
         CREATE TABLE nodes (id INTEGER PRIMARY KEY, kind TEXT, title TEXT, summary TEXT,
                             updated_at INTEGER, lane TEXT);
         CREATE TABLE lane_story (id INTEGER PRIMARY KEY, lane TEXT, ord INTEGER, heading TEXT,
                                  body TEXT, updated_at INTEGER);
         INSERT INTO events VALUES
           (1, 1700000000000, 'pr',    'PR #4 opened', 'a body',   'gh:b/r#4', 'https://x/4', 'rebuild'),
           (2, 1700000001000, 'ci',    'CI green',     'all pass', NULL,       NULL,          'rebuild'),
           (3, 1700000002000, 'issue', 'issue #9',     'a bug',    'gh:b/r#9', NULL,          'rebuild');
         INSERT INTO nodes VALUES
           (1, 'lane', 'the rebuild', 'the rebuild is under way', 1700000000000, 'rebuild'),
           (2, 'lane', 'empty',       '',                         1700000001000, 'rebuild');
         INSERT INTO lane_story VALUES
           (1, 'rebuild', 2, 'second', 'the second chapter', 1700000000000),
           (2, 'rebuild', 1, 'first',  'the first chapter',  1700000001000);",
    )
    .expect("the fixture schema");
}

/// The old bough db: command memory (which is NEVER mail) and note sections.
pub fn standard_bough(path: &std::path::Path) {
    let conn = Connection::open(path).expect("a fresh bough db");
    conn.execute_batch(
        "CREATE TABLE command_history (id INTEGER PRIMARY KEY, session_id TEXT, ts INTEGER,
             repo TEXT, cmd TEXT, tags TEXT, exit_code INTEGER, duration_ms INTEGER,
             output_head TEXT);
         CREATE TABLE command_tags (command_id INTEGER, tag TEXT);
         CREATE TABLE note_sections (id INTEGER PRIMARY KEY, note_id INTEGER, ord INTEGER,
             heading TEXT, body TEXT, author TEXT, created_at INTEGER, updated_at INTEGER);
         INSERT INTO command_history VALUES
           (1, 's', 1700000000000, 'bough',  'cargo test -p bough-plugin-ledger', 'cargo:test', 0, 10, 'ok'),
           (2, 's', 1700000001000, 'bough',  'rg todo',                           'search',     0, 10, ''),
           (3, 's', 1700000002000, 'jungler','cargo build',                       'cargo:build',0, 10, '');
         INSERT INTO command_tags VALUES (1,'cargo'),(1,'test'),(2,'search'),(3,'cargo'),(3,'build');
         INSERT INTO note_sections VALUES
           (1, 7, 0, 'the seam', 'a seam has three roles', 'human', 1700000000000, 1700000000000),
           (2, 7, 1, 'the rule', 'plugins, not loop changes', 'human', 1700000000000, 1700000000000);",
    )
    .expect("the fixture schema");
}

// ---- a driver that records, and never runs a wake ---------------------------------------------

#[derive(Default)]
pub struct StubFactory {
    pub drivers: Mutex<Vec<Arc<StubDriver>>>,
}

#[async_trait::async_trait]
impl AgentFactory for StubFactory {
    fn driver(&self) -> &'static str {
        "old-feed-test-driver"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        let d = Arc::new(StubDriver {
            _cell: cell,
            notified: Mutex::new(Vec::new()),
        });
        self.drivers.lock().push(d.clone());
        Ok(d as Arc<dyn AgentDriver>)
    }
}

pub struct StubDriver {
    _cell: AgentCell,
    pub notified: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AgentDriver for StubDriver {
    fn driver(&self) -> &'static str {
        "old-feed-test-driver"
    }
    async fn notify(&self, _receipt: &InboxReceipt, msg: &Message) {
        self.notified.lock().push(msg.subject.clone());
    }
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        WakeRequest::Nothing
    }
}
