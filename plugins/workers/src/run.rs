//! Invariant (§10): `ask()` is not a side channel. A worker's question surfaces as WAKE-CLASS
//! mail on the SPAWNER's lane — a durable inbox splice like any other — and then blocks the
//! worker or ends it, per the configured mode.
//!
//! The splice itself belongs to `agents`, so the seam takes it as a SINK: the default sink is the
//! spawner's own `Agent::send`, and a test can hand in a recorder and assert on the exact
//! (message, target, wake) triple that reaches the inbox.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_agents::{MailClass, Message, MessageId, Sender, Target};
use bough_plugin_ledger::AgentName;
use chrono::Utc;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::WorkerError;
use crate::ids::WorkerId;
use crate::start::AskMode;

/// What `ask()` came back with.
#[derive(Clone, Debug, PartialEq)]
pub enum AskAnswer {
    /// The spawner answered.
    Answered(String),
    /// Mode `end`, or the spawner went away: the worker stops here.
    Ended,
}

/// Where a worker's question is delivered, and where its answer comes back from.
///
/// Two operations rather than one, because the two modes of §10 differ in exactly this: `end`
/// delivers and stops, `block` delivers and waits.
#[async_trait::async_trait]
pub trait AskSink: Send + Sync + 'static {
    /// Splice `msg` onto `spawner`'s inbox. `target`/`wake` are the seam's, not the sink's.
    async fn deliver(
        &self,
        spawner: &AgentName,
        msg: Message,
        target: Target,
        wake: bool,
    ) -> Result<MessageId, WorkerError>;

    /// Wait for the spawner's answer to `msg`. `None` ⇒ the spawner will not answer.
    async fn answer(&self, msg: &MessageId) -> Option<String>;
}

/// A sink that drops questions on the floor. The default when no spawner lane is reachable: a
/// worker that asks simply ends, which is the safe half of §10's two modes.
pub struct NullAskSink;

#[async_trait::async_trait]
impl AskSink for NullAskSink {
    async fn deliver(
        &self,
        _spawner: &AgentName,
        msg: Message,
        _target: Target,
        _wake: bool,
    ) -> Result<MessageId, WorkerError> {
        Ok(msg.id)
    }
    async fn answer(&self, _msg: &MessageId) -> Option<String> {
        None
    }
}

/// The last question a run asked, and the mail it became.
#[derive(Clone, Debug, PartialEq)]
pub struct AskedQuestion {
    pub question: String,
    pub message: MessageId,
}

pub(crate) struct RunInner {
    pub(crate) id: WorkerId,
    pub(crate) spawner: AgentName,
    pub(crate) ask_mode: AskMode,
    pub(crate) cancel: CancellationToken,
    pub(crate) sink: Arc<dyn AskSink>,
    pub(crate) asked: Mutex<Option<AskedQuestion>>,
}

/// A live run, from the provider's side.
#[derive(Clone)]
pub struct WorkerRun(pub(crate) Arc<RunInner>);

impl WorkerRun {
    pub(crate) fn new(
        id: WorkerId,
        spawner: AgentName,
        ask_mode: AskMode,
        sink: Arc<dyn AskSink>,
    ) -> WorkerRun {
        WorkerRun(Arc::new(RunInner {
            id,
            spawner,
            ask_mode,
            cancel: CancellationToken::new(),
            sink,
            asked: Mutex::new(None),
        }))
    }

    /// A detached run, for tests of `ask()` that do not want a whole provider around it. The
    /// live table is the seam's; this constructor never enters it.
    pub fn for_test(
        id: WorkerId,
        spawner: AgentName,
        ask_mode: AskMode,
        sink: Arc<dyn AskSink>,
    ) -> WorkerRun {
        WorkerRun::new(id, spawner, ask_mode, sink)
    }

    pub fn id(&self) -> &WorkerId {
        &self.0.id
    }

    /// The agent this run works for.
    pub fn spawner(&self) -> &AgentName {
        &self.0.spawner
    }

    pub fn ask_mode(&self) -> AskMode {
        self.0.ask_mode
    }

    pub fn cancel(&self) -> CancellationToken {
        self.0.cancel.clone()
    }

    /// The last question this run asked, if any. The provider turns it into
    /// [`crate::WorkerOutcome::Asked`].
    pub fn asked(&self) -> Option<AskedQuestion> {
        self.0.asked.lock().clone()
    }

    /// §10: surfaces as WAKE-CLASS mail on the SPAWNER's lane, and blocks or ends per `ask_mode`.
    ///
    /// The target is `NextWake` with the wake flag set: a question the spawner cannot see until
    /// it happens to wake is not a question.
    pub async fn ask(&self, question: String) -> Result<AskAnswer, WorkerError> {
        if self.0.cancel.is_cancelled() {
            return Err(WorkerError::Cancelled(self.0.id.clone()));
        }
        let msg = Message {
            id: MessageId::new(uuid::Uuid::now_v7().to_string()),
            from: Sender::Worker(self.0.id.clone()),
            class: MailClass::Wake,
            text: question.clone(),
            subject: format!("worker {} asks", self.0.id),
            cites: Vec::new(),
            refs: BTreeSet::new(),
            mail_seq: None,
            at: Utc::now(),
        };
        let id = self
            .0
            .sink
            .deliver(&self.0.spawner, msg, Target::NextWake, true)
            .await?;
        *self.0.asked.lock() = Some(AskedQuestion {
            question,
            message: id.clone(),
        });
        match self.0.ask_mode {
            AskMode::End => Ok(AskAnswer::Ended),
            AskMode::Block => Ok(match self.0.sink.answer(&id).await {
                Some(text) => AskAnswer::Answered(text),
                None => AskAnswer::Ended,
            }),
        }
    }
}

impl std::fmt::Debug for WorkerRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerRun")
            .field("id", &self.0.id)
            .field("spawner", &self.0.spawner)
            .field("ask_mode", &self.0.ask_mode)
            .finish()
    }
}
