//! Shared test plumbing for the agents suite — the fixture half of
//! `caps.test.ts` / `subagent.test.ts` / `notes.test.ts`. Test-only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bus::Bus;
use crate::db::sqlite_db::{DbOptions, SqliteDb};
use crate::errors::BoughError;
use crate::schema::events::{BoughEvent, EventType};
use crate::schema::parts::{Message, Part, Role, Session, SessionKind};
use crate::turn::queue::TurnRegistry;
use crate::turn::runner::{RUN_STEPS, STOP};
use crate::turn::testkit::{stop, text};
use crate::types::{
    system_clock, AppCtx, HostState, LlmClient, LlmParams, LlmResult, OnText, SharedDb, TurnCtx,
};

/// A string that exists ONLY in the spawner's transcript.
pub const SPAWNER_SECRET: &str = "PINEAPPLE-QUADRANT-7";

pub fn shared_db() -> SharedDb {
    Arc::new(Mutex::new(SqliteDb::new(":memory:", DbOptions::default()).unwrap()))
}

pub fn is_cap_error(err: &BoughError) -> bool {
    matches!(err, BoughError::SpawnCap { .. })
}

// ---- session seeding --------------------------------------------------------

#[derive(Default)]
pub struct SeedOpts {
    pub kind: Option<SessionKind>,
    pub origin_id: Option<String>,
}

impl SeedOpts {
    pub fn subagent_of(origin_id: &str) -> Self {
        SeedOpts { kind: Some(SessionKind::Subagent), origin_id: Some(origin_id.to_string()) }
    }
}

/// A session of any kind, optionally hung off a lineage edge.
pub fn seed_session(db: &SharedDb, opts: SeedOpts) -> Session {
    let id = Uuid::new_v4().to_string();
    let kind = opts.kind.unwrap_or(SessionKind::Root);
    let guard = db.lock().unwrap();
    guard
        .create_session(Session {
            id: id.clone(),
            title: format!("{kind:?}"),
            kind,
            created_at: 1_000,
            parent_id: None,
            origin_message_id: opts.origin_id.as_ref().map(|o| format!("m-{o}")),
            origin_id: opts.origin_id,
            workspace: Some("/tmp/checkout".to_string()),
            origin_dir: Some("/tmp/checkout".to_string()),
            base: None,
            model: None,
            effort: None,
            draft: None,
            context_tokens: None,
            cached_tokens: None,
            last_llm_at: None,
            outcome_ok: None,
        })
        .unwrap()
}

/// A `TurnCtx` slice for the caps: db + ids + depth, nothing else live.
pub fn turn_ctx_for(db: &SharedDb, session_id: &str, turn_id: &str, depth: u8) -> TurnCtx {
    TurnCtx {
        app: AppCtx {
            db: db.clone(),
            bus: Arc::new(Bus::new(system_clock())),
            llm: None,
            model: None,
            effort: None,
            now: system_clock(),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: Arc::new(TurnRegistry::new()),
            model_defaults_path: None,
        },
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        message_id: "message-1".to_string(),
        workspace: "/tmp/checkout".to_string(),
        model: "claude-test-model".to_string(),
        cancel: CancellationToken::new(),
        exits: Arc::new(Mutex::new(vec![])),
        record: None,
        reads: Arc::new(Mutex::new(vec![])),
        touched: Arc::new(Mutex::new(vec![])),
        mcp_grant: None,
        depth,
    }
}

// ---- the agents fixture -----------------------------------------------------

/// A real in-memory db, a real bus with an event sink, and a fresh registry —
/// the harness every subagent/notes test drives.
pub struct AgentsFixture {
    pub db: SharedDb,
    pub ctx: AppCtx,
    pub events: Arc<Mutex<Vec<BoughEvent>>>,
    pub registry: Arc<TurnRegistry>,
}

impl AgentsFixture {
    pub fn new() -> Self {
        let db = shared_db();
        let bus = Arc::new(Bus::new(system_clock()));
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e: &BoughEvent| sink.lock().unwrap().push(e.clone())));
        let registry = Arc::new(TurnRegistry::new());
        let ctx = AppCtx {
            db: db.clone(),
            bus,
            llm: None,
            model: Some("claude-test-model".to_string()),
            effort: None,
            now: system_clock(),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: registry.clone(),
            model_defaults_path: None,
        };
        AgentsFixture { db, ctx, events, registry }
    }

    /// The same fixture with the client every turn will use.
    pub fn with_llm(mut self, llm: Arc<dyn LlmClient>) -> Self {
        self.ctx.llm = Some(llm);
        self
    }
}

/// A root session with one user message — idle, with no turn ever run (the
/// `notes.test.ts` fixture shape).
pub fn seed_idle_session(f: &AgentsFixture) -> Session {
    let guard = f.db.lock().unwrap();
    let session = guard
        .create_session(Session {
            id: Uuid::new_v4().to_string(),
            title: "the spawner".to_string(),
            kind: SessionKind::Root,
            created_at: 1_000,
            parent_id: None,
            origin_id: None,
            origin_message_id: None,
            workspace: Some("/tmp/checkout".to_string()),
            origin_dir: Some("/tmp/checkout".to_string()),
            base: None,
            model: None,
            effort: None,
            draft: None,
            context_tokens: None,
            cached_tokens: None,
            last_llm_at: None,
            outcome_ok: None,
        })
        .unwrap();
    guard
        .create_message(Message {
            id: Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            role: Role::User,
            parts: vec![Part::Text { text: "delegate the audit".to_string() }],
            pending: false,
            created_at: 1_001,
        })
        .unwrap();
    session
}

/// A spawner session with a transcript the child must not inherit.
pub struct SeededSpawner {
    pub session: Session,
    pub supervisor: Message,
}

pub fn seed_spawner(f: &AgentsFixture) -> SeededSpawner {
    let session = {
        let guard = f.db.lock().unwrap();
        guard
            .create_session(Session {
                id: Uuid::new_v4().to_string(),
                title: "the spawner".to_string(),
                kind: SessionKind::Root,
                created_at: 1_000,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some("/tmp/checkout".to_string()),
                origin_dir: Some("/tmp/checkout".to_string()),
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
            .unwrap()
    };
    let guard = f.db.lock().unwrap();
    guard
        .create_message(Message {
            id: Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            role: Role::User,
            parts: vec![Part::Text {
                text: format!("the plan is {SPAWNER_SECRET}, do not tell anyone"),
            }],
            pending: false,
            created_at: 1_001,
        })
        .unwrap();
    let supervisor = guard
        .create_message(Message {
            id: Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            role: Role::Supervisor,
            parts: vec![Part::Text { text: format!("acknowledged, {SPAWNER_SECRET} it is") }],
            pending: true,
            created_at: 1_002,
        })
        .unwrap();
    SeededSpawner { session, supervisor }
}

/// The spawning turn's ctx, as the runner would have built it.
pub fn spawner_turn_ctx(
    f: &AgentsFixture,
    seeded: &SeededSpawner,
    llm: Arc<dyn LlmClient>,
) -> TurnCtx {
    let mut app = f.ctx.clone();
    app.llm = Some(llm);
    TurnCtx {
        app,
        session_id: seeded.session.id.clone(),
        turn_id: "turn-spawner".to_string(),
        message_id: seeded.supervisor.id.clone(),
        workspace: seeded.session.workspace.clone().unwrap_or_else(|| "/tmp/checkout".into()),
        model: "claude-test-model".to_string(),
        cancel: CancellationToken::new(),
        exits: Arc::new(Mutex::new(vec![])),
        record: None,
        reads: Arc::new(Mutex::new(vec![])),
        touched: Arc::new(Mutex::new(vec![])),
        mcp_grant: None,
        depth: 0,
    }
}

// ---- scripted clients -------------------------------------------------------

/// A model that answers every call with the same text plus `stop`, snapshotting
/// each call — the shortest complete turn there is, repeatable.
pub struct RecordingLlm {
    reply: String,
    calls: Mutex<Vec<LlmParams>>,
}

impl RecordingLlm {
    pub fn calls(&self) -> Vec<LlmParams> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmClient for RecordingLlm {
    async fn run(
        &self,
        params: LlmParams,
        _on_text: OnText,
        _cancel: CancellationToken,
    ) -> Result<LlmResult, BoughError> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(params);
            calls.len()
        };
        Ok(LlmResult {
            content: vec![text(&self.reply), stop(&format!("stop-{n}"))],
            stop_reason: "tool_use".to_string(),
            usage: None,
        })
    }
}

pub fn recording_llm(reply: &str) -> Arc<RecordingLlm> {
    Arc::new(RecordingLlm { reply: reply.to_string(), calls: Mutex::new(vec![]) })
}

/// A model whose round parks until released, and that rejects like a real
/// abort when the turn is interrupted. Returns (client, release, started):
/// `started` flips true once the round is actually in flight.
pub fn gated_llm(
    report: &str,
) -> (Arc<dyn LlmClient>, Arc<dyn Fn() + Send + Sync>, tokio::sync::watch::Receiver<bool>) {
    let (started_tx, started_rx) = tokio::sync::watch::channel(false);
    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);

    struct Gated {
        report: String,
        started: tokio::sync::watch::Sender<bool>,
        gate: tokio::sync::watch::Receiver<bool>,
    }

    #[async_trait]
    impl LlmClient for Gated {
        async fn run(
            &self,
            _params: LlmParams,
            _on_text: OnText,
            cancel: CancellationToken,
        ) -> Result<LlmResult, BoughError> {
            let _ = self.started.send(true);
            let mut gate = self.gate.clone();
            loop {
                if *gate.borrow() {
                    break;
                }
                tokio::select! {
                    _ = cancel.cancelled() => {
                        // The llm layer's abort: status 499, never retried.
                        return Err(BoughError::llm_with("interrupted", 499, None));
                    }
                    changed = gate.changed() => {
                        if changed.is_err() {
                            break;
                        }
                    }
                }
            }
            Ok(LlmResult {
                content: vec![text(&self.report), stop("stop-gated")],
                stop_reason: "tool_use".to_string(),
                usage: None,
            })
        }
    }

    let client: Arc<dyn LlmClient> =
        Arc::new(Gated { report: report.to_string(), started: started_tx, gate: gate_rx });
    let release: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = gate_tx.send(true);
    });
    (client, release, started_rx)
}

// ---- the one-turn-per-session watcher ---------------------------------------

/// One turn per session, watched from the bus — the load-bearing invariant of
/// the whole notes suite. A turn announces itself with a `message.started`
/// carrying its pending supervisor message and closes with `turn.finished`;
/// the depth between the two is the number of turns a session has open, and
/// anything above one is the invariant broken.
pub struct TurnWatch {
    starts: Arc<Mutex<Vec<String>>>,
    violations: Arc<Mutex<Vec<String>>>,
}

impl TurnWatch {
    pub fn turns_for(&self, session_id: &str) -> usize {
        self.starts.lock().unwrap().iter().filter(|s| s.as_str() == session_id).count()
    }
    pub fn violations(&self) -> Vec<String> {
        self.violations.lock().unwrap().clone()
    }
}

pub fn watch_turns(bus: &Bus) -> TurnWatch {
    let live: Arc<Mutex<HashMap<String, i64>>> = Arc::new(Mutex::new(HashMap::new()));
    let starts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let violations: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let (l, s, v) = (live.clone(), starts.clone(), violations.clone());
    bus.subscribe(Arc::new(move |e: &BoughEvent| {
        let Some(id) = e.session_id.clone() else { return };
        match e.r#type {
            EventType::MessageStarted
                if e.data.get("role").and_then(|r| r.as_str()) == Some("supervisor") =>
            {
                let mut live = l.lock().unwrap();
                let depth = live.get(&id).copied().unwrap_or(0) + 1;
                live.insert(id.clone(), depth);
                s.lock().unwrap().push(id.clone());
                if depth > 1 {
                    v.lock().unwrap().push(id);
                }
            }
            EventType::TurnFinished => {
                let mut live = l.lock().unwrap();
                let depth = (live.get(&id).copied().unwrap_or(0) - 1).max(0);
                live.insert(id, depth);
            }
            _ => {}
        }
    }));
    TurnWatch { starts, violations }
}

/// Poll until `pred` holds. Bounded, so a broken wake fails as a timeout, not
/// a hang.
pub async fn until(pred: impl Fn() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pred() {
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for: {what}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

// Keep the protocol names referenced so a rename over there breaks here too.
#[allow(dead_code)]
const _PINNED_TOOL_NAMES: (&str, &str) = (RUN_STEPS, STOP);
