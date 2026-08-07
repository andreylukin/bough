//! Shared test plumbing for the thread-operation suite (the fixture half of
//! `compact.test.ts` / `extract.test.ts` / `move.test.ts` / `handoff.test.ts` /
//! `sections.test.ts`): an in-memory database, a real bus with a collector, and
//! a recording `LlmClient`. Test-only.
//!
//! Everything is offline and hermetic — no provider key, no shell, no clock
//! that has to be waited on.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bus::Bus;
use crate::db::sqlite_db::{DbOptions, SqliteDb};
use crate::errors::BoughError;
use crate::schema::events::BoughEvent;
use crate::schema::parts::{Message, Part, Role, Session, SessionKind};
use crate::schema::requests::PartPick;
use crate::turn::queue::TurnRegistry;
use crate::types::{
    system_clock, AppCtx, HostState, LlmBlock, LlmClient, LlmContentBlock, LlmParams, LlmResult,
    OnText, SharedDb,
};

use super::seed::with_db;

// ---------------------------------------------------------------------------
// The recording client
// ---------------------------------------------------------------------------

/// A one-shot completion client that records what it was asked and answers
/// with a stable, identifiable string per call (`SUMMARY-0`, `SUMMARY-1`, …).
#[derive(Default)]
pub struct RecordingLlm {
    prompts: Mutex<Vec<String>>,
    systems: Mutex<Vec<String>>,
    models: Mutex<Vec<String>>,
    /// A fixed reply for every call. Absent = `SUMMARY-<n>`.
    reply: Mutex<Option<String>>,
    /// After this many successful calls, every call fails with this message.
    fail_after: Mutex<Option<(usize, String)>>,
    calls: AtomicUsize,
}

impl RecordingLlm {
    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
    pub fn systems(&self) -> Vec<String> {
        self.systems.lock().unwrap().clone()
    }
    pub fn models(&self) -> Vec<String> {
        self.models.lock().unwrap().clone()
    }
    /// Answer this text for every call from now on.
    pub fn set_reply(&self, reply: &str) {
        *self.reply.lock().unwrap() = Some(reply.to_string());
    }
    /// Succeed `n` times, then fail every call with `message`.
    pub fn set_failure_after(&self, n: usize, message: &str) {
        *self.fail_after.lock().unwrap() = Some((n, message.to_string()));
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
        let prompt = params
            .messages
            .first()
            .and_then(|m| m.content.first())
            .map(|b| match b {
                LlmContentBlock::Text { text } => text.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(prompt);
        self.systems
            .lock()
            .unwrap()
            .push(params.system.clone().unwrap_or_default());
        self.models.lock().unwrap().push(params.model.clone());

        if let Some((after, message)) = self.fail_after.lock().unwrap().clone() {
            if n >= after {
                return Err(BoughError::bad_request(message));
            }
        }
        let text = self
            .reply
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| format!("SUMMARY-{n}"));
        Ok(LlmResult {
            content: vec![LlmBlock::Text { text }],
            stop_reason: "end_turn".to_string(),
            usage: None,
        })
    }
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

pub struct Fixture {
    pub ctx: AppCtx,
    pub llm: Arc<RecordingLlm>,
    pub events: Arc<Mutex<Vec<BoughEvent>>>,
}

impl Fixture {
    /// Testkit surface: the ops suites reach through `ctx.db` directly today.
    #[allow(dead_code)]
    pub fn db(&self) -> &SharedDb {
        &self.ctx.db
    }
    /// The types of every event published so far, in order.
    pub fn event_types(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.r#type.as_str().to_string())
            .collect()
    }
    pub fn clear_events(&self) {
        self.events.lock().unwrap().clear();
    }
}

/// A ctx whose LLM records everything and whose model default is unset (so the
/// built-in `DEFAULT_MODEL` is what an unpinned session resolves to).
pub fn scripted_ctx() -> Fixture {
    let db: SharedDb = Arc::new(Mutex::new(
        SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
    ));
    let bus = Arc::new(Bus::new(system_clock()));
    let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
    let sink = events.clone();
    bus.subscribe(Arc::new(move |e: &BoughEvent| {
        sink.lock().unwrap().push(e.clone())
    }));
    let llm = Arc::new(RecordingLlm::default());
    let ctx = AppCtx {
        db,
        bus,
        llm: Some(llm.clone()),
        model: None,
        effort: None,
        now: system_clock(),
        cheap: None,
        host: Arc::new(HostState::new()),
        starter: Arc::new(RwLock::new(None)),
        turn_registry: Arc::new(TurnRegistry::new()),
        model_defaults_path: None,
    };
    Fixture { ctx, llm, events }
}

/// The session fields the operation tests actually vary.
#[derive(Clone, Debug)]
pub struct SessionOver {
    pub title: String,
    pub kind: SessionKind,
    pub parent_id: Option<String>,
    pub workspace: Option<String>,
    pub origin_dir: Option<String>,
    pub base: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Default for SessionOver {
    fn default() -> Self {
        SessionOver {
            title: "the work".to_string(),
            kind: SessionKind::Root,
            parent_id: None,
            workspace: None,
            origin_dir: None,
            base: None,
            model: None,
            effort: None,
        }
    }
}

pub fn session_with(f: &Fixture, over: SessionOver) -> Session {
    with_db(&f.ctx.db, |d| {
        d.create_session(Session {
            id: Uuid::new_v4().to_string(),
            parent_id: over.parent_id,
            title: over.title,
            kind: over.kind,
            created_at: 1_000,
            workspace: over.workspace,
            origin_dir: over.origin_dir,
            base: over.base,
            origin_id: None,
            origin_message_id: None,
            model: over.model,
            effort: over.effort,
            draft: None,
            context_tokens: None,
            cached_tokens: None,
            last_llm_at: None,
            outcome_ok: None,
        })
    })
    .unwrap()
}

static STAMP: AtomicUsize = AtomicUsize::new(0);

pub fn message(f: &Fixture, session_id: &str, role: Role, parts: Vec<Part>) -> Message {
    let created_at = 1_700_000_000_000 + STAMP.fetch_add(1, Ordering::SeqCst) as i64;
    with_db(&f.ctx.db, |d| {
        d.create_message(Message {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            role,
            parts,
            pending: false,
            created_at,
        })
    })
    .unwrap()
}

pub fn text(f: &Fixture, session_id: &str, role: Role, t: &str) -> Message {
    message(
        f,
        session_id,
        role,
        vec![Part::Text {
            text: t.to_string(),
        }],
    )
}

/// A session with `texts.len()` messages, alternating user/supervisor.
pub fn conversation(f: &Fixture, texts: &[&str], over: SessionOver) -> (Session, Vec<Message>) {
    let source = session_with(f, over);
    let messages = texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Supervisor
            };
            text(f, &source.id, role, t)
        })
        .collect();
    (source, messages)
}

/// The text of every part of a message, joined — enough to identify a copy.
pub fn text_of(m: &Message) -> String {
    m.parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

pub fn texts_of(messages: &[Message]) -> Vec<String> {
    messages.iter().map(text_of).collect()
}

/// Whole-message picks by index into a message list.
pub fn picks(messages: &[Message], indexes: &[usize]) -> Vec<PartPick> {
    indexes
        .iter()
        .map(|&i| PartPick {
            message_id: messages[i].id.clone(),
            parts: None,
        })
        .collect()
}
