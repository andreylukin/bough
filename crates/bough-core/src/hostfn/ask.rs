//! `ask()` (port of `src/hostfn/ask.ts`) — the mid-task question, and the hold
//! that parks a program on a human.
//!
//! THE INVARIANT THIS HOLDS: **a hold is memory-only, and it always settles.**
//!
//! *Memory-only*, deliberately ("the hold dies with the turn"). A pending
//! question means nothing once the turn that raised it is gone — there is no
//! program left to hand the answer to. Nothing here touches the database
//! except to append the SETTLED question to the transcript, which is the
//! durable record: a `Part::Ask` that replays as plain text and can never
//! re-block. A restart therefore leaves nothing pending, with no recovery
//! pass, because there was never anything to recover.
//!
//! *Always settles* is the other half. Four things can end a hold and every
//! one re-emits the same id with a final status: (1) the user answers; (2) the
//! user dismisses — `ask()` rejects with a catchable `user declined`; (3) the
//! turn's interrupt reaches the parked program; (4) the turn ends while a hold
//! is still parked — the sweep rides `turn.finished` off the bus.
//!
//! WHY THE SETTLED PART IS BUFFERED UNTIL `message.finished`: the turn runner
//! owns the supervisor message's `parts` array in memory and writes it
//! WHOLESALE on every append. A part written to the row from out here is
//! therefore erased by the runner's very next append. So settled parts are
//! held and flushed once, after the runner's last write, on
//! `message.finished`; a hold that settles after that (the sweep) is applied
//! straight through. [`append_ask_part`] preserves the message's `pending`
//! flag so a late append can never flip a finished message back to busy.
//!
//! `hostfn/` imports nothing from the server crate: the registry takes a
//! `Bus`, the host function takes a `TurnCtx`, and the HTTP handlers that
//! drive them live in `bough-server::questions`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bus::Bus;
use crate::errors::BoughError;
use crate::schema::events::{EventInput, EventType, MessageFinishedData, MessagePartData};
use crate::schema::parts::{AskQuestion, AskQuestionStatus, AskStatus, Part};
use crate::types::{system_clock, Clock, Db, TurnCtx};

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// How a hold ended. `pending` is the only non-terminal status, and it never
/// appears here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskSettlement {
    Answered,
    Declined,
    Interrupted,
}

impl From<AskSettlement> for AskQuestionStatus {
    fn from(s: AskSettlement) -> AskQuestionStatus {
        match s {
            AskSettlement::Answered => AskQuestionStatus::Answered,
            AskSettlement::Declined => AskQuestionStatus::Declined,
            AskSettlement::Interrupted => AskQuestionStatus::Interrupted,
        }
    }
}

impl From<AskSettlement> for AskStatus {
    fn from(s: AskSettlement) -> AskStatus {
        match s {
            AskSettlement::Answered => AskStatus::Answered,
            AskSettlement::Declined => AskStatus::Declined,
            AskSettlement::Interrupted => AskStatus::Interrupted,
        }
    }
}

/// One question a program is parked on.
#[derive(Clone, Debug)]
pub struct AskInput {
    pub session_id: String,
    /// The supervisor message whose turn raised it — the transcript anchor.
    pub message_id: String,
    pub question: String,
    /// Pick-one choices. Free text stays possible either way.
    pub options: Option<Vec<String>>,
}

/// What `raise` hands back: the record as announced, and the settlement the
/// program awaits.
pub struct RaisedAsk {
    /// The record as raised (`status: pending`). The final status arrives
    /// through [`RaisedAsk::settled`].
    pub record: AskQuestion,
    rx: oneshot::Receiver<(AskSettlement, Option<String>)>,
}

impl RaisedAsk {
    /// How the hold ended, and the answer when there was one.
    pub async fn settled(self) -> (AskSettlement, Option<String>) {
        // A dropped sender can only mean the registry was torn down mid-hold —
        // indistinguishable from an interrupt, and answered the same way.
        self.rx.await.unwrap_or((AskSettlement::Interrupted, None))
    }

    /// Resolves with the user's answer; rejects catchably with `user declined`
    /// on a dismissal and with an interrupt notice when the turn is stopped.
    pub async fn answer(self) -> Result<String, BoughError> {
        let question = self.record.question.clone();
        match self.settled().await {
            (AskSettlement::Answered, ans) => Ok(ans.unwrap_or_default()),
            (AskSettlement::Declined, _) => Err(declined(&question)),
            (AskSettlement::Interrupted, _) => Err(interrupted(&question)),
        }
    }
}

struct PendingAsk {
    record: AskQuestion,
    /// Raise order, for a stable oldest-first `list` under one clock tick.
    seq: u64,
    bus: Arc<Bus>,
    tx: oneshot::Sender<(AskSettlement, Option<String>)>,
    /// The interrupt watcher, aborted on settle so no task outlives its hold.
    watcher: Option<tokio::task::JoinHandle<()>>,
}

/// The live holds.
///
/// A struct rather than a global for the same reason `DetachedSubagents` is
/// one: two tests in one file must not be able to settle each other's
/// questions. Production uses the instance on `HostState`, which is what the
/// HTTP routes and the turn's host function share — the two must see the same
/// map or an answer arrives at nobody.
pub struct AskHolds {
    pending: Mutex<HashMap<String, PendingAsk>>,
    /// Injected clock, so `ts` (which orders the list) is assertable.
    now: Clock,
    seq: AtomicU64,
}

/// The name `HostState` knows the registry by (the wave-1 stub's name).
pub type AskRegistry = AskHolds;

impl AskHolds {
    pub fn new() -> Self {
        Self::with_clock(system_clock())
    }

    pub fn with_clock(now: Clock) -> Self {
        AskHolds {
            pending: Mutex::new(HashMap::new()),
            now,
            seq: AtomicU64::new(0),
        }
    }

    /// Raise one question; the returned [`RaisedAsk`] parks until it settles.
    ///
    /// No timeout: a question is user-paced by design, and a deadline would
    /// turn "the user stepped away" into a spurious failure of work that was
    /// going fine. The turn is what bounds it — its interrupt, and the sweep
    /// when it ends.
    ///
    /// The hold is registered BEFORE the announcement, so a listener that
    /// answers synchronously — a test, or a same-process client — finds it
    /// rather than racing it. An already-cancelled token settles immediately
    /// rather than watching a token that has already fired.
    pub fn raise(
        self: &Arc<Self>,
        bus: &Arc<Bus>,
        q: AskInput,
        cancel: Option<&CancellationToken>,
    ) -> RaisedAsk {
        let options: Option<Vec<String>> = q
            .options
            .map(|o| o.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>())
            .filter(|o: &Vec<String>| !o.is_empty());
        let record = AskQuestion {
            id: Uuid::new_v4().to_string(),
            session_id: q.session_id,
            message_id: q.message_id,
            question: q.question,
            options,
            status: AskQuestionStatus::Pending,
            answer: None,
            ts: (self.now)(),
        };
        let (tx, rx) = oneshot::channel();
        let entry = PendingAsk {
            record: record.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            bus: bus.clone(),
            tx,
            watcher: None,
        };
        self.pending
            .lock()
            .unwrap()
            .insert(record.id.clone(), entry);
        bus.publish(EventInput {
            r#type: EventType::AskQuestion,
            session_id: Some(record.session_id.clone()),
            data: serde_json::to_value(&record).unwrap_or_default(),
        });

        if let Some(cancel) = cancel {
            if cancel.is_cancelled() {
                // Already stopped: settle now rather than watching a token
                // that will never fire again.
                self.settle(&record.id, AskSettlement::Interrupted, None);
            } else {
                let holds = Arc::clone(self);
                let token = cancel.clone();
                let id = record.id.clone();
                let handle = tokio::spawn(async move {
                    token.cancelled().await;
                    holds.settle(&id, AskSettlement::Interrupted, None);
                });
                // Attach the watcher — unless the hold settled in the gap, in
                // which case the task is already pointless.
                let mut map = self.pending.lock().unwrap();
                match map.get_mut(&record.id) {
                    Some(e) => e.watcher = Some(handle),
                    None => handle.abort(),
                }
            }
        }

        RaisedAsk { record, rx }
    }

    /// Settle. First-wins: `false` means someone already settled this one —
    /// two clients answering at once is an ordinary race, not an error.
    /// Re-emits the SAME id with its final status, so the hold card updates in
    /// place instead of a second card appearing next to a stale one.
    fn settle(&self, id: &str, status: AskSettlement, ans: Option<String>) -> bool {
        let entry = self.pending.lock().unwrap().remove(id);
        let Some(mut entry) = entry else { return false };
        if let Some(watcher) = entry.watcher.take() {
            watcher.abort();
        }
        entry.record.status = status.into();
        if let Some(a) = &ans {
            entry.record.answer = Some(a.clone());
        }
        entry.bus.publish(EventInput {
            r#type: EventType::AskQuestion,
            session_id: Some(entry.record.session_id.clone()),
            data: serde_json::to_value(&entry.record).unwrap_or_default(),
        });
        let _ = entry.tx.send((status, ans));
        true
    }

    /// Settle with the user's answer. False when the id is not (or no longer)
    /// waiting.
    pub fn answer(&self, id: &str, answer: &str) -> bool {
        self.settle(id, AskSettlement::Answered, Some(answer.to_string()))
    }

    /// Dismiss: `ask()` rejects with a catchable `user declined`.
    pub fn decline(&self, id: &str) -> bool {
        self.settle(id, AskSettlement::Declined, None)
    }

    /// A pending question by id — the route's lookup. Settled ones are gone.
    pub fn get(&self, id: &str) -> Option<AskQuestion> {
        self.pending
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.record.clone())
    }

    /// Questions currently awaiting an answer, oldest first, optionally for
    /// one session. This is how a freshly-attached client rebuilds its hold
    /// cards: events are display transport and never replay, so the live
    /// registry is the only place a card can come from.
    pub fn list(&self, session_id: Option<&str>) -> Vec<AskQuestion> {
        let map = self.pending.lock().unwrap();
        let mut rows: Vec<(&PendingAsk,)> = map
            .values()
            .filter(|e| session_id.is_none_or(|s| e.record.session_id == s))
            .map(|e| (e,))
            .collect();
        rows.sort_by_key(|(e,)| (e.record.ts, e.seq));
        rows.into_iter().map(|(e,)| e.record.clone()).collect()
    }

    /// Settle every still-parked hold as `interrupted`, for one session or all
    /// of them. Returns how many were swept.
    ///
    /// This is failure-mode 4 from the header: a program torn down without
    /// unwinding (the wall-clock timeout terminates the worker, not the host
    /// promise) leaves a hold nobody will ever answer. Sweeping it is what
    /// keeps "the hold dies with the turn" true in fact and not just intent.
    pub fn expire(&self, session_id: Option<&str>) -> usize {
        let ids: Vec<String> = {
            let map = self.pending.lock().unwrap();
            map.values()
                .filter(|e| session_id.is_none_or(|s| e.record.session_id == s))
                .map(|e| e.record.id.clone())
                .collect()
        };
        ids.iter()
            .filter(|id| self.settle(id, AskSettlement::Interrupted, None))
            .count()
    }

    /// Live hold count. The leak checks read it.
    pub fn size(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
}

impl Default for AskHolds {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Errors the program catches
// ---------------------------------------------------------------------------

/// The dismissal. The phrase `user declined` is load-bearing: the prompt's ask
/// section tells the model to catch exactly this, and the question is repeated
/// so a program holding several knows which one was dismissed.
fn declined(question: &str) -> BoughError {
    BoughError::ask_declined(format!(
        "user declined to answer: {question} — the question was dismissed, not missed. \
         Proceed on a default you state out loud, or stop cleanly; do not ask again."
    ))
}

/// The interrupt. Distinct from a decline: nobody said no, the turn was stopped.
fn interrupted(question: &str) -> BoughError {
    BoughError::program(format!(
        "ask() interrupted — the turn was stopped before the question was answered: \
         {question}. Nothing was decided; work already done still stands."
    ))
}

// ---------------------------------------------------------------------------
// The settled part
// ---------------------------------------------------------------------------

/// Append one settled question to its supervisor message.
///
/// Two details carry the weight:
///
///   - **`message.pending` is preserved, never set.** A hold that settles as
///     the turn dies would otherwise flip a finished message back to pending,
///     and the UI would show a session busy forever on a turn that already
///     ended.
///   - **It is idempotent on the part's id.** The flush and the sweep can both
///     reach the same part, and a transcript with the same question in it
///     twice is worse than one missing it.
///
/// Returns whether it wrote. A message that no longer exists is not an error
/// worth raising into a program that has already been told its answer.
pub fn append_ask_part(
    db: &dyn Db,
    bus: &Bus,
    session_id: &str,
    message_id: &str,
    part: &Part,
) -> bool {
    let Part::Ask { id: part_id, .. } = part else {
        return false;
    };
    let Ok(Some(message)) = db.get_message(message_id) else {
        return false;
    };
    let duplicate = message
        .parts
        .iter()
        .any(|p| matches!(p, Part::Ask { id, .. } if id == part_id));
    if duplicate {
        return false;
    }
    let mut parts = message.parts.clone();
    parts.push(part.clone());
    if db
        .update_message(message_id, &parts, message.pending)
        .is_err()
    {
        return false;
    }
    bus.publish(EventInput {
        r#type: EventType::MessagePart,
        session_id: Some(session_id.to_string()),
        data: serde_json::to_value(MessagePartData {
            message_id: message_id.to_string(),
            part: part.clone(),
        })
        .unwrap_or_default(),
    });
    true
}

/// The transcript record for one settled hold.
fn ask_part_of(record: &AskQuestion, status: AskSettlement, answer: Option<String>) -> Part {
    Part::Ask {
        id: record.id.clone(),
        question: record.question.clone(),
        options: record.options.clone().filter(|o| !o.is_empty()),
        status: status.into(),
        answer,
    }
}

// ---------------------------------------------------------------------------
// The bridged host function
// ---------------------------------------------------------------------------

/// Options parsed at the bridge. Deliberately lenient about the *contents*: a
/// model that passes `{options: [1, 2]}` meant two choices, and refusing the
/// question over it costs a round to learn nothing. A non-object bag is
/// refused, because that is a call shaped wrongly rather than a value typed
/// loosely.
fn parse_ask_options(opts_json: &str) -> Result<Option<Vec<String>>, BoughError> {
    let text = opts_json.trim();
    if text.is_empty() || text == "null" || text == "undefined" {
        return Ok(None);
    }
    let raw: Value = serde_json::from_str(text).map_err(|err| {
        BoughError::bad_request(format!(
            "ask(question, opts): the options could not be read as JSON ({err}). Pass a \
             plain object, e.g. ask(\"Which environment?\", {{options: [\"dev\", \
             \"prod\"]}})."
        ))
    })?;
    let obj = match raw {
        Value::Null => return Ok(None),
        Value::Object(o) => o,
        _ => return Err(options_shape_error()),
    };
    let options = match obj.get("options") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(options_shape_error()),
    };
    let cleaned: Vec<String> = options
        .iter()
        .map(|v| match v {
            Value::String(s) => s.trim().to_string(),
            other => other.to_string().trim().to_string(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    Ok(if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    })
}

fn options_shape_error() -> BoughError {
    BoughError::bad_request(
        "ask(question, opts): the second argument must be an object like \
         {options: [\"dev\", \"prod\"]} — free text is always possible, so options are \
         a convenience, never a requirement.",
    )
}

/// Seams, so `ask()` is drivable with no worker, no client and no server.
#[derive(Clone, Default)]
pub struct AskDeps {
    /// Absent = the registry on `ctx.app.host`, which is what the HTTP routes
    /// also reach.
    pub holds: Option<Arc<AskHolds>>,
    /// Where a settled question is recorded. Absent = buffered and appended to
    /// the turn's supervisor message (see the module header for why it is
    /// buffered). A test passes a collector to assert on the parts without a
    /// database.
    pub append: Option<Arc<dyn Fn(Part) + Send + Sync>>,
}

struct AskFnState {
    /// Settled parts waiting for the runner's last write.
    buffered: Vec<Part>,
    /// True once the supervisor message is closed and safe to append to
    /// directly.
    closed: bool,
    /// The armed bus subscription, when there is one.
    sub: Option<u64>,
}

struct AskInner {
    ctx: TurnCtx,
    holds: Arc<AskHolds>,
    append: Option<Arc<dyn Fn(Part) + Send + Sync>>,
    st: Mutex<AskFnState>,
}

impl AskInner {
    fn sink(&self, part: Part) {
        if let Some(append) = &self.append {
            append(part);
            return;
        }
        let closed = {
            let mut st = self.st.lock().unwrap();
            if !st.closed {
                st.buffered.push(part.clone());
                false
            } else {
                true
            }
        };
        if closed {
            let db = self.ctx.app.db.lock().unwrap();
            append_ask_part(
                &*db,
                &self.ctx.app.bus,
                &self.ctx.session_id,
                &self.ctx.message_id,
                &part,
            );
        }
    }

    fn flush(&self) {
        let parts: Vec<Part> = {
            let mut st = self.st.lock().unwrap();
            st.closed = true;
            std::mem::take(&mut st.buffered)
        };
        for part in parts {
            self.sink(part);
        }
    }

    fn disarm(&self) {
        let sub = self.st.lock().unwrap().sub.take();
        if let Some(id) = sub {
            self.ctx.app.bus.unsubscribe(id);
        }
    }
}

/// The bridged `ask(question, optsJson)` for one turn.
///
/// The answer comes back as a PLAIN string, not JSON — `ask` is the one
/// bridged function besides `view`/`patch` whose payload is already text, so a
/// program gets the user's words with no unwrapping.
pub struct AskHostFn {
    inner: Arc<AskInner>,
}

/// Build `ask(question, optsJson)` for one turn.
pub fn create_ask_host_fn(ctx: &TurnCtx, deps: AskDeps) -> AskHostFn {
    let holds = deps.holds.unwrap_or_else(|| ctx.app.host.asks.clone());
    AskHostFn {
        inner: Arc::new(AskInner {
            ctx: ctx.clone(),
            holds,
            append: deps.append,
            st: Mutex::new(AskFnState {
                buffered: vec![],
                closed: false,
                sub: None,
            }),
        }),
    }
}

impl AskHostFn {
    /// Watch this turn's own lifecycle, from the first question onwards.
    ///
    /// Armed lazily, so a turn that never asks anything never subscribes, and
    /// removed on `turn.finished` — which the runner emits on every path it
    /// can end by. That is also where the sweep lives: whatever is still
    /// parked when the turn ends can never be answered, so it is settled as
    /// `interrupted` and its part written straight through.
    fn arm(&self) {
        // The lock is held across the subscribe so the id is stored before any
        // other thread's fan-out can want to disarm it. The listener only
        // takes this lock for its own two event types, so the synchronous
        // publish inside `subscribe`'s critical section cannot deadlock.
        let mut st = self.inner.st.lock().unwrap();
        if st.sub.is_some() {
            return;
        }
        let weak: Weak<AskInner> = Arc::downgrade(&self.inner);
        let id = self.inner.ctx.app.bus.subscribe(Arc::new(move |event| {
            let Some(inner) = weak.upgrade() else { return };
            if event.r#type == EventType::MessageFinished {
                let finished: Option<MessageFinishedData> =
                    serde_json::from_value(event.data.clone()).ok();
                if finished.map(|f| f.message_id) == Some(inner.ctx.message_id.clone()) {
                    inner.flush();
                }
                return;
            }
            if event.r#type == EventType::TurnFinished
                && event.session_id.as_deref() == Some(inner.ctx.session_id.as_str())
            {
                // Unsubscribe FIRST: the sweep below settles holds, which
                // appends parts, and re-entering this listener from inside
                // itself would be a needless second pass over an empty buffer.
                inner.disarm();
                // A turn that ended without `message.finished` should not
                // exist, but if one does, the buffered parts belong on the
                // message rather than in memory.
                inner.flush();
                inner.holds.expire(Some(&inner.ctx.session_id));
            }
        }));
        st.sub = Some(id);
    }

    pub async fn ask(&self, question: &str, opts_json: &str) -> Result<String, BoughError> {
        let options = parse_ask_options(opts_json)?;
        let text = question.trim().to_string();
        if text.is_empty() {
            return Err(BoughError::bad_request(
                "ask(): the question is empty. Ask something a human can answer in one \
                 line, e.g. ask(\"Deploy to prod or staging?\", {options: [\"prod\", \
                 \"staging\"]}).",
            ));
        }
        // Refuse before announcing a card nobody can answer: the turn is
        // already over.
        if self.inner.ctx.cancel.is_cancelled() {
            return Err(interrupted(&text));
        }

        self.arm();
        let raised = self.inner.holds.raise(
            &self.inner.ctx.app.bus,
            AskInput {
                session_id: self.inner.ctx.session_id.clone(),
                message_id: self.inner.ctx.message_id.clone(),
                question: text,
                options,
            },
            Some(&self.inner.ctx.cancel),
        );
        let record = raised.record.clone();

        match raised.settled().await {
            (AskSettlement::Answered, ans) => {
                let given = ans.unwrap_or_default();
                self.inner.sink(ask_part_of(
                    &record,
                    AskSettlement::Answered,
                    Some(given.clone()),
                ));
                Ok(given)
            }
            // The settlement says which of the two happened — "you dismissed
            // it" and "the turn was stopped" are different facts for both the
            // user and the next round.
            (AskSettlement::Declined, _) => {
                self.inner
                    .sink(ask_part_of(&record, AskSettlement::Declined, None));
                Err(declined(&record.question))
            }
            (AskSettlement::Interrupted, _) => {
                self.inner
                    .sink(ask_part_of(&record, AskSettlement::Interrupted, None));
                Err(interrupted(&record.question))
            }
        }
    }

    /// The `HostFns.ask` adapter: JSON-string args in protocol order.
    pub fn into_host_fn(self) -> crate::types::HostFn {
        use futures::FutureExt;
        let this = Arc::new(self);
        Arc::new(move |args: Vec<String>| {
            let this = this.clone();
            async move {
                let question = args.first().cloned().unwrap_or_default();
                let opts = args.get(1).cloned().unwrap_or_else(|| "{}".to_string());
                this.ask(&question, &opts).await
            }
            .boxed()
        })
    }
}

// ---------------------------------------------------------------------------
// tests — ported from `src/hostfn/ask.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::RwLock;

    use serde_json::json;

    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{Message, Role, Session, SessionKind};
    use crate::turn::queue::TurnRegistry;
    use crate::types::{AppCtx, HostState, SharedDb};

    const SESSION: &str = "s1";
    const MESSAGE: &str = "m1";

    struct Fixture {
        db: SharedDb,
        bus: Arc<Bus>,
        holds: Arc<AskHolds>,
        /// Every `ask.question` payload the bus carried, in order.
        questions: Arc<Mutex<Vec<AskQuestion>>>,
        ctx: TurnCtx,
    }

    impl Fixture {
        /// The supervisor message as it stands in the database right now.
        fn message(&self) -> Message {
            self.db
                .lock()
                .unwrap()
                .get_message(MESSAGE)
                .unwrap()
                .unwrap()
        }

        /// Its `ask` parts, in order.
        fn ask_parts(&self) -> Vec<Part> {
            self.message()
                .parts
                .into_iter()
                .filter(|p| matches!(p, Part::Ask { .. }))
                .collect()
        }

        fn questions(&self) -> Vec<AskQuestion> {
            self.questions.lock().unwrap().clone()
        }

        /// The exact two events the turn runner emits when it ends, in the
        /// order it emits them: the message is closed first, then the turn.
        fn finish_turn(&self, status: &str) {
            {
                let db = self.db.lock().unwrap();
                let current = db.get_message(MESSAGE).unwrap().unwrap();
                db.update_message(MESSAGE, &current.parts, false).unwrap();
            }
            self.bus.publish(EventInput {
                r#type: EventType::MessageFinished,
                session_id: Some(SESSION.to_string()),
                data: json!({"messageId": MESSAGE}),
            });
            self.bus.publish(EventInput {
                r#type: EventType::TurnFinished,
                session_id: Some(SESSION.to_string()),
                data: json!({"turnId": "t1", "sessionId": SESSION, "status": status}),
            });
        }
    }

    fn session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            title: id.to_string(),
            kind: SessionKind::Root,
            created_at: 1_000,
            parent_id: None,
            origin_id: None,
            origin_message_id: None,
            workspace: None,
            origin_dir: None,
            base: None,
            model: None,
            effort: None,
            draft: None,
            context_tokens: None,
            cached_tokens: None,
            last_llm_at: None,
            outcome_ok: None,
            description: None,
        }
    }

    fn fixture() -> Fixture {
        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let bus = Arc::new(Bus::with_error_hook(system_clock(), Arc::new(|_e, _ev| {})));
        let holds = Arc::new(AskHolds::new());
        let questions: Arc<Mutex<Vec<AskQuestion>>> = Arc::new(Mutex::new(vec![]));
        let sink = questions.clone();
        bus.subscribe(Arc::new(move |e| {
            if e.r#type == EventType::AskQuestion {
                if let Ok(q) = serde_json::from_value::<AskQuestion>(e.data.clone()) {
                    sink.lock().unwrap().push(q);
                }
            }
        }));

        {
            let d = db.lock().unwrap();
            d.create_session(session(SESSION)).unwrap();
            d.create_message(Message {
                id: MESSAGE.to_string(),
                session_id: SESSION.to_string(),
                role: Role::Supervisor,
                parts: vec![],
                pending: true,
                created_at: 1_000,
            })
            .unwrap();
        }

        let app = AppCtx {
            db: db.clone(),
            bus: bus.clone(),
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now: system_clock(),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: Arc::new(TurnRegistry::new()),
            model_defaults_path: None,
        };
        let ctx = TurnCtx {
            app,
            session_id: SESSION.to_string(),
            turn_id: "t1".to_string(),
            message_id: MESSAGE.to_string(),
            workspace: "/tmp".to_string(),
            model: "test-model".to_string(),
            cancel: CancellationToken::new(),
            exits: Arc::new(Mutex::new(vec![])),
            record: None,
            reads: Arc::new(Mutex::new(vec![])),
            touched: Arc::new(Mutex::new(vec![])),
            round_refs: Arc::new(Mutex::new(vec![])),
            mcp_grant: None,
            depth: 0,
        };
        Fixture {
            db,
            bus,
            holds,
            questions,
            ctx,
        }
    }

    fn input(session_id: &str, message_id: &str, question: &str) -> AskInput {
        AskInput {
            session_id: session_id.to_string(),
            message_id: message_id.to_string(),
            question: question.to_string(),
            options: None,
        }
    }

    fn part_fields(
        p: &Part,
    ) -> (
        String,
        String,
        Option<Vec<String>>,
        AskStatus,
        Option<String>,
    ) {
        match p {
            Part::Ask {
                id,
                question,
                options,
                status,
                answer,
            } => (
                id.clone(),
                question.clone(),
                options.clone(),
                *status,
                answer.clone(),
            ),
            other => panic!("not an ask part: {other:?}"),
        }
    }

    // ---- the registry -------------------------------------------------------

    #[tokio::test]
    async fn raise_answer_resolves_and_the_same_id_is_re_emitted_as_answered() {
        let f = fixture();
        let raised = f.holds.raise(
            &f.bus,
            AskInput {
                options: Some(vec!["dev".into(), "prod".into()]),
                ..input(SESSION, MESSAGE, "Which env?")
            },
            None,
        );
        let record = raised.record.clone();

        // Registered, listed, announced pending — in that order, so a
        // listener that answers synchronously finds the hold rather than
        // racing it.
        assert_eq!(f.holds.get(&record.id).unwrap().question, "Which env?");
        assert_eq!(
            f.holds
                .list(Some(SESSION))
                .iter()
                .map(|q| q.id.clone())
                .collect::<Vec<_>>(),
            vec![record.id.clone()]
        );
        assert_eq!(f.questions()[0].status, AskQuestionStatus::Pending);
        assert_eq!(
            f.questions()[0].options,
            Some(vec!["dev".to_string(), "prod".to_string()])
        );

        assert!(f.holds.answer(&record.id, "prod"));
        assert_eq!(raised.answer().await.unwrap(), "prod");

        // Settled: gone from the registry, final event on the SAME id so the
        // hold card updates in place rather than a second card appearing
        // beside a stale one.
        assert!(f.holds.get(&record.id).is_none());
        assert_eq!(f.holds.size(), 0);
        assert_eq!(f.questions()[1].id, record.id);
        assert_eq!(f.questions()[1].status, AskQuestionStatus::Answered);
        assert_eq!(f.questions()[1].answer.as_deref(), Some("prod"));

        // A second settle is a no-op: two clients answering at once is a
        // race, not an error, and the first one wins.
        assert!(!f.holds.answer(&record.id, "dev"));
        assert_eq!(f.questions().len(), 2);
    }

    #[tokio::test]
    async fn raise_decline_rejects_catchably_with_user_declined() {
        let f = fixture();
        let raised = f
            .holds
            .raise(&f.bus, input(SESSION, MESSAGE, "Drop the table?"), None);
        let id = raised.record.id.clone();
        assert!(f.holds.decline(&id));

        let err = raised.answer().await.unwrap_err();
        assert_eq!(err.name(), "AskDeclinedError");
        // The phrase is load-bearing, and the question is repeated so a
        // program holding several knows which one was dismissed.
        assert!(err.to_string().contains("user declined"), "{err}");
        assert!(err.to_string().contains("Drop the table?"), "{err}");
        assert_eq!(f.questions()[1].status, AskQuestionStatus::Declined);
        assert_eq!(f.holds.size(), 0);
    }

    #[tokio::test]
    async fn ac_an_interrupt_settles_the_hold_rather_than_hanging() {
        let f = fixture();
        let cancel = CancellationToken::new();
        let raised = f.holds.raise(
            &f.bus,
            input(SESSION, MESSAGE, "Which branch?"),
            Some(&cancel),
        );
        assert_eq!(f.holds.size(), 1);

        cancel.cancel();

        // Settled, not hanging: the settlement arrives and the hold is out of
        // the registry.
        let err = raised.answer().await.unwrap_err();
        assert_eq!(err.name(), "ProgramError");
        assert!(err.to_string().contains("interrupted"), "{err}");
        // Distinguishable from a decline — "you stopped it" and "the user
        // said no" call for different moves from the program.
        assert_ne!(err.name(), "AskDeclinedError");
        assert_eq!(f.holds.size(), 0);
        assert_eq!(f.questions()[1].status, AskQuestionStatus::Interrupted);
    }

    #[tokio::test]
    async fn an_already_aborted_signal_settles_immediately_without_registering() {
        let f = fixture();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let raised = f
            .holds
            .raise(&f.bus, input(SESSION, MESSAGE, "Which?"), Some(&cancel));
        raised.answer().await.unwrap_err();
        assert_eq!(f.holds.size(), 0);
        assert_eq!(
            f.questions().last().unwrap().status,
            AskQuestionStatus::Interrupted
        );
    }

    #[tokio::test]
    async fn expire_clears_one_sessions_holds_and_leaves_the_others() {
        let f = fixture();
        let a = f.holds.raise(&f.bus, input("sA", "m1", "a?"), None);
        let b = f.holds.raise(&f.bus, input("sB", "m2", "b?"), None);

        assert_eq!(f.holds.expire(Some("sA")), 1);
        a.answer().await.unwrap_err();
        assert_eq!(
            f.holds
                .list(None)
                .iter()
                .map(|q| q.id.clone())
                .collect::<Vec<_>>(),
            vec![b.record.id.clone()]
        );

        assert_eq!(f.holds.expire(None), 1);
        b.answer().await.unwrap_err();
        assert_eq!(f.holds.size(), 0);
        // Sweeping an empty registry is a no-op, not a second round of events.
        assert_eq!(f.holds.expire(None), 0);
    }

    #[tokio::test]
    async fn list_is_oldest_first_and_scoped_so_a_client_can_rebuild_its_cards() {
        let t = Arc::new(AtomicU64::new(0));
        let tc = t.clone();
        let holds = Arc::new(AskHolds::with_clock(Arc::new(move || {
            tc.fetch_add(1, Ordering::SeqCst) as i64 + 1
        })));
        let bus = Arc::new(Bus::with_error_hook(system_clock(), Arc::new(|_e, _ev| {})));
        let one = holds.raise(&bus, input("sA", "m", "1"), None);
        let two = holds.raise(&bus, input("sB", "m", "2"), None);
        let three = holds.raise(&bus, input("sA", "m", "3"), None);

        assert_eq!(
            holds
                .list(Some("sA"))
                .iter()
                .map(|q| q.id.clone())
                .collect::<Vec<_>>(),
            vec![one.record.id.clone(), three.record.id.clone()]
        );
        assert_eq!(
            holds
                .list(None)
                .iter()
                .map(|q| q.question.clone())
                .collect::<Vec<_>>(),
            vec!["1", "2", "3"]
        );
        holds.expire(None);
        one.answer().await.unwrap_err();
        two.answer().await.unwrap_err();
        three.answer().await.unwrap_err();
    }

    // ---- AC: a restart leaves nothing pending -------------------------------

    #[tokio::test]
    async fn ac_a_restart_leaves_nothing_pending_there_is_nothing_to_heal() {
        let f = fixture();
        let before = Arc::new(AskHolds::new());
        let raised = before.raise(&f.bus, input(SESSION, MESSAGE, "Which env?"), None);
        let record_id = raised.record.id.clone();
        assert_eq!(before.size(), 1);

        // The process ends. Nothing is written, nothing is flushed — the
        // registry simply ceases to exist along with the turn that owned the
        // hold.
        let after = AskHolds::new();
        assert_eq!(after.size(), 0);
        assert_eq!(after.list(None).len(), 0);
        assert!(after.get(&record_id).is_none());
        // …and the answer route finds nothing to settle, which is what makes
        // the 404 in the questions routes the honest answer rather than a
        // lost update.
        assert!(!after.answer(&record_id, "prod"));
        assert!(!after.decline(&record_id));
        assert_eq!(after.expire(None), 0);

        // The message carries no half-written hold either: the durable record
        // is only ever written once the question has SETTLED.
        assert_eq!(f.ask_parts().len(), 0);

        before.expire(None);
        raised.answer().await.unwrap_err();
    }

    // ---- the settled part ---------------------------------------------------

    #[test]
    fn append_ask_part_preserves_pending_and_is_idempotent_on_the_id() {
        let f = fixture();
        let part = Part::Ask {
            id: "q1".to_string(),
            question: "Which env?".to_string(),
            options: Some(vec!["dev".into(), "prod".into()]),
            status: AskStatus::Answered,
            answer: Some("prod".to_string()),
        };

        {
            let db = f.db.lock().unwrap();
            assert!(append_ask_part(&*db, &f.bus, SESSION, MESSAGE, &part));
        }
        assert!(
            f.message().pending,
            "an append during the turn leaves it pending"
        );
        // A second append of the same question is refused: a transcript with
        // the question in it twice is worse than one missing it.
        {
            let db = f.db.lock().unwrap();
            assert!(!append_ask_part(&*db, &f.bus, SESSION, MESSAGE, &part));
        }
        assert_eq!(f.ask_parts().len(), 1);

        // Once the runner has closed the message, a late append must NOT
        // reopen it — a message left pending is a session the UI shows as
        // busy forever.
        {
            let db = f.db.lock().unwrap();
            let current = db.get_message(MESSAGE).unwrap().unwrap();
            db.update_message(MESSAGE, &current.parts, false).unwrap();
            let late = Part::Ask {
                id: "q2".to_string(),
                question: "Which env?".to_string(),
                options: None,
                status: AskStatus::Answered,
                answer: None,
            };
            assert!(append_ask_part(&*db, &f.bus, SESSION, MESSAGE, &late));
        }
        assert!(!f.message().pending);
        assert_eq!(f.ask_parts().len(), 2);

        // A message that no longer exists is not an error worth raising into
        // a program that has already been given its answer.
        let db = f.db.lock().unwrap();
        assert!(!append_ask_part(&*db, &f.bus, SESSION, "gone", &part));
    }

    // ---- the bridged host function ------------------------------------------

    #[tokio::test]
    async fn ask_resolves_with_the_answer_and_records_it_on_the_message() {
        let f = fixture();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );

        let opts_json = json!({"options": ["dev", "prod"]}).to_string();
        let parked = ask.ask("Which env?", &opts_json);
        tokio::pin!(parked);
        // Drive the future far enough to raise the hold.
        assert!(futures::poll!(parked.as_mut()).is_pending());
        let live = f.holds.list(Some(SESSION));
        assert_eq!(live.len(), 1);
        assert_eq!(
            live[0].options,
            Some(vec!["dev".to_string(), "prod".to_string()])
        );

        f.holds.answer(&live[0].id, "prod");
        assert_eq!(parked.await.unwrap(), "prod");

        // Buffered until the runner's last write: the runner owns the parts
        // array in memory and rewrites it wholesale, so a part written now
        // would be erased by the very next append.
        assert_eq!(f.ask_parts().len(), 0);

        f.finish_turn("done");
        let parts = f.ask_parts();
        assert_eq!(parts.len(), 1);
        let (id, question, options, status, answer) = part_fields(&parts[0]);
        assert_eq!(id, live[0].id);
        assert_eq!(question, "Which env?");
        assert_eq!(options, Some(vec!["dev".to_string(), "prod".to_string()]));
        assert_eq!(status, AskStatus::Answered);
        assert_eq!(answer.as_deref(), Some("prod"));
        assert!(!f.message().pending);
    }

    #[tokio::test]
    async fn a_part_written_during_the_turn_would_be_erased_the_buffer_is_why_it_is_not() {
        let f = fixture();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );
        let parked = ask.ask("Which env?", "{}");
        tokio::pin!(parked);
        assert!(futures::poll!(parked.as_mut()).is_pending());
        f.holds.answer(&f.holds.list(None)[0].id, "prod");
        parked.await.unwrap();

        // The runner appends the program's tool_result from its own in-memory
        // array, which has never seen the ask part. This is the write that
        // would clobber it.
        {
            let db = f.db.lock().unwrap();
            db.update_message(
                MESSAGE,
                &[Part::Text {
                    text: "done".into(),
                }],
                true,
            )
            .unwrap();
        }

        f.finish_turn("done");
        // Survived, because it was flushed after that write rather than
        // before it.
        let parts = f.ask_parts();
        assert_eq!(parts.len(), 1);
        assert_eq!(part_fields(&parts[0]).3, AskStatus::Answered);
    }

    #[tokio::test]
    async fn ask_rejects_catchably_on_decline_and_records_the_dismissal() {
        let f = fixture();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );
        let parked = ask.ask("Drop the table?", "{}");
        tokio::pin!(parked);
        assert!(futures::poll!(parked.as_mut()).is_pending());
        f.holds.decline(&f.holds.list(None)[0].id);

        let err = parked.await.unwrap_err();
        assert_eq!(err.name(), "AskDeclinedError");
        assert!(err.to_string().contains("user declined"), "{err}");

        f.finish_turn("done");
        let parts = f.ask_parts();
        assert_eq!(parts.len(), 1);
        let (_, _, _, status, answer) = part_fields(&parts[0]);
        assert_eq!(status, AskStatus::Declined);
        assert_eq!(answer, None);
    }

    #[tokio::test]
    async fn ac_interrupting_the_turn_settles_a_parked_ask_rather_than_hanging_it() {
        let f = fixture();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );
        let parked = ask.ask("Which branch?", "{}");
        tokio::pin!(parked);
        assert!(futures::poll!(parked.as_mut()).is_pending());
        assert_eq!(f.holds.size(), 1);

        f.ctx.cancel.cancel();

        let err = parked.await.unwrap_err();
        assert!(err.to_string().contains("interrupted"), "{err}");
        assert_eq!(f.holds.size(), 0);

        f.finish_turn("interrupted");
        assert_eq!(part_fields(&f.ask_parts()[0]).3, AskStatus::Interrupted);
    }

    #[tokio::test]
    async fn ac_a_hold_still_parked_when_the_turn_ends_is_swept_not_left_haunting() {
        let f = fixture();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );
        // No cancel and no answer: the shape a wall-clock timeout leaves
        // behind, where the worker is gone but the host promise was never
        // unwound.
        let parked = ask.ask("Which env?", "{}");
        tokio::pin!(parked);
        assert!(futures::poll!(parked.as_mut()).is_pending());
        assert_eq!(f.holds.size(), 1);

        f.finish_turn("done");

        let err = parked.await.unwrap_err();
        assert!(err.to_string().contains("interrupted"), "{err}");
        assert_eq!(f.holds.size(), 0, "the hold is gone from the registry");
        assert_eq!(
            f.holds.list(Some(SESSION)).len(),
            0,
            "and from every client's card list"
        );
        // The final event says how it ended, so a card that was showing
        // "pending" closes.
        assert_eq!(
            f.questions().last().unwrap().status,
            AskQuestionStatus::Interrupted
        );
        // Swept after the message closed, so its part applies straight
        // through.
        assert_eq!(f.ask_parts().len(), 1);
        assert_eq!(part_fields(&f.ask_parts()[0]).3, AskStatus::Interrupted);
        assert!(!f.message().pending, "and the message is not reopened");
    }

    #[tokio::test]
    async fn the_turns_bus_subscription_is_released_when_the_turn_ends() {
        let f = fixture();
        let before = f.bus.size();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );
        // Nothing is subscribed until the first question — a turn that never
        // asks pays nothing.
        assert_eq!(f.bus.size(), before);

        let parked = ask.ask("Which env?", "{}");
        tokio::pin!(parked);
        assert!(futures::poll!(parked.as_mut()).is_pending());
        assert_eq!(f.bus.size(), before + 1);
        f.finish_turn("done");
        parked.await.unwrap_err();
        assert_eq!(f.bus.size(), before, "no listener leak per turn");
    }

    #[tokio::test]
    async fn two_questions_in_one_turn_both_land_in_the_order_they_were_asked() {
        let f = fixture();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );

        let opts_json = json!({"options": ["dev", "prod"]}).to_string();
        let first = ask.ask("Which env?", &opts_json);
        tokio::pin!(first);
        assert!(futures::poll!(first.as_mut()).is_pending());
        f.holds.answer(&f.holds.list(None)[0].id, "prod");
        assert_eq!(first.await.unwrap(), "prod");

        let second = ask.ask("Proceed?", "{}");
        tokio::pin!(second);
        assert!(futures::poll!(second.as_mut()).is_pending());
        f.holds.decline(&f.holds.list(None)[0].id);
        second.await.unwrap_err();

        f.finish_turn("done");
        let landed: Vec<(String, AskStatus)> = f
            .ask_parts()
            .iter()
            .map(|p| {
                let (_, q, _, s, _) = part_fields(p);
                (q, s)
            })
            .collect();
        assert_eq!(
            landed,
            vec![
                ("Which env?".to_string(), AskStatus::Answered),
                ("Proceed?".to_string(), AskStatus::Declined),
            ]
        );
    }

    #[tokio::test]
    async fn ask_refuses_before_announcing_a_card_nobody_can_answer() {
        let f = fixture();
        f.ctx.cancel.cancel();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );
        let err = ask.ask("Which env?", "{}").await.unwrap_err();
        assert!(err.to_string().contains("interrupted"), "{err}");
        // Nothing was announced and nothing parked: the turn was already over.
        assert_eq!(f.questions().len(), 0);
        assert_eq!(f.holds.size(), 0);
    }

    #[tokio::test]
    async fn options_are_read_leniently_a_malformed_bag_is_refused_with_the_fix() {
        let f = fixture();
        let appended: Arc<Mutex<Vec<Part>>> = Arc::new(Mutex::new(vec![]));
        let sink = appended.clone();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                append: Some(Arc::new(move |p| sink.lock().unwrap().push(p))),
            },
        );

        // Non-strings become strings, blanks are dropped: a model that wrote
        // `[1, 2]` meant two choices, and refusing the question over it costs
        // a round to learn nothing.
        let opts_json = json!({"options": [1, " prod ", "", "  "]}).to_string();
        let parked = ask.ask("Pick", &opts_json);
        tokio::pin!(parked);
        assert!(futures::poll!(parked.as_mut()).is_pending());
        assert_eq!(
            f.holds.list(None)[0].options,
            Some(vec!["1".to_string(), "prod".to_string()])
        );
        f.holds.answer(&f.holds.list(None)[0].id, "prod");
        parked.await.unwrap();
        assert_eq!(
            part_fields(&appended.lock().unwrap()[0]).2,
            Some(vec!["1".to_string(), "prod".to_string()])
        );

        // No options at all is free text, and the part carries no empty array.
        let free = ask.ask("Anything?", "{}");
        tokio::pin!(free);
        assert!(futures::poll!(free.as_mut()).is_pending());
        assert_eq!(f.holds.list(None)[0].options, None);
        f.holds.answer(&f.holds.list(None)[0].id, "sure");
        free.await.unwrap();
        assert_eq!(part_fields(&appended.lock().unwrap()[1]).2, None);

        // A bag that is not an object at all is a call shaped wrongly.
        let bad = ask.ask("Pick", "\"dev\"").await.unwrap_err();
        assert!(bad.to_string().contains("options"), "{bad}");
        // …and so is an empty question.
        let empty = ask.ask("   ", "{}").await.unwrap_err();
        assert!(empty.to_string().contains("question is empty"), "{empty}");

        f.finish_turn("done");
    }

    #[tokio::test]
    async fn a_settled_ask_never_reopens_a_message_the_runner_already_closed() {
        let f = fixture();
        let ask = create_ask_host_fn(
            &f.ctx,
            AskDeps {
                holds: Some(f.holds.clone()),
                ..Default::default()
            },
        );
        let parked = ask.ask("Which env?", "{}");
        tokio::pin!(parked);
        assert!(futures::poll!(parked.as_mut()).is_pending());
        // The turn dies first — message closed, turn finished — and only then
        // does the sweep settle the hold and write its part.
        f.finish_turn("interrupted");
        parked.await.unwrap_err();
        assert!(!f.message().pending);
        assert_eq!(f.ask_parts().len(), 1);
    }
}
