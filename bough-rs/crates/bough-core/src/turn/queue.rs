//! TurnRegistry + failure classification + abortable delay (port of
//! `src/turn/queue.ts`).
//!
//! THE INVARIANT: **a session runs at most one turn at a time, and no user
//! input is ever lost to that rule.** `begin` claims synchronously (throws
//! before the placeholder message exists); `end` is identity-checked (a late
//! `end` from a superseded turn must not unregister its replacement);
//! `interrupt` aborts the token then fires a snapshot of cascade hooks (a
//! throwing hook is swallowed — a child that is already gone is not an error,
//! it is the goal). Hooks fire even when the session itself is idle: its turn
//! may have ended while a detached subagent it spawned runs on.
//!
//! Below the registry: the derived queue and the round-level retry ring
//! (row 1.21).
//!
//! - **A message posted mid-turn queues, it does not race.** The queue is
//!   *derived from the database* ([`has_unanswered_input`]), not from an
//!   in-memory flag — a flag would be lost across a restart, stranding the
//!   message forever. The explicit `enqueue` is only a nudge.
//! - **A round that fails is retried, not executed.** A tool call whose input
//!   was cut off mid-stream re-streams immediately; executing it would run
//!   the wrong program against the user's checkout. A provider outage waits.
//!   Retries are bounded, and an exhausted one is a turn error.

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::errors::{BoughError, ErrorKind};
use crate::llm::retry::is_retryable;
use crate::schema::parts::Role;
use crate::types::Db;

/// A cascade hook a detached child registers; fired on explicit interrupt only.
pub type InterruptHook = Arc<dyn Fn() + Send + Sync>;

/// One claimed turn. `end` is identity-checked via `id`: the claim object is
/// what proves the caller is the turn that holds the session.
#[derive(Debug)]
pub struct TurnClaim {
    pub session_id: String,
    pub cancel: CancellationToken,
    id: u64,
}

/// The one-turn-per-session claim table plus interrupt cascades. Mutex-atomic
/// check+take — Bun's single-thread atomicity does not come free in Rust.
pub struct TurnRegistry {
    /// session id → (claim id, the live turn's interrupt token).
    running: Mutex<HashMap<String, (u64, CancellationToken)>>,
    /// The explicit drain nudge; the derived check is the truth (row 1.21).
    queued: Mutex<HashSet<String>>,
    /// session id → registered cascade hooks, in registration order.
    hooks: Mutex<HashMap<String, Vec<(u64, InterruptHook)>>>,
    next_id: AtomicU64,
}

impl TurnRegistry {
    pub fn new() -> Self {
        TurnRegistry {
            running: Mutex::new(HashMap::new()),
            queued: Mutex::new(HashSet::new()),
            hooks: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Is a turn in flight for this session?
    pub fn is_running(&self, session_id: &str) -> bool {
        self.running.lock().unwrap().contains_key(session_id)
    }

    /// Sessions with a live turn. The prompt's running-subagent note reads this.
    pub fn running_sessions(&self) -> Vec<String> {
        self.running.lock().unwrap().keys().cloned().collect()
    }

    /// Claim the session and return the turn's interrupt.
    ///
    /// Errors when one is already running rather than replacing it: silently
    /// overwriting the token would leave the first turn unstoppable — its
    /// abort handle gone while it kept writing to the same message.
    pub fn begin(&self, session_id: &str) -> Result<TurnClaim, BoughError> {
        let mut running = self.running.lock().unwrap();
        if running.contains_key(session_id) {
            return Err(BoughError::http(
                500,
                ErrorKind::Turn,
                format!("a turn is already running for session {session_id}"),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        running.insert(session_id.to_string(), (id, cancel.clone()));
        Ok(TurnClaim { session_id: session_id.to_string(), cancel, id })
    }

    /// Release the session. Identity-checked: a late `end` from a turn that
    /// was already superseded must not unregister the turn that replaced it.
    pub fn end(&self, claim: &TurnClaim) {
        let mut running = self.running.lock().unwrap();
        if running.get(&claim.session_id).map(|(id, _)| *id) == Some(claim.id) {
            running.remove(&claim.session_id);
        }
    }

    /// Stop the session's turn and cascade to its detached children.
    ///
    /// Returns false only when there was nothing to stop. Hooks fire even when
    /// the session itself is idle. The claim stays registered until the turn
    /// unwinds and calls `end` — a double-tap finds it again, which is fine.
    pub fn interrupt(&self, session_id: &str) -> bool {
        let token = self
            .running
            .lock()
            .unwrap()
            .get(session_id)
            .map(|(_, t)| t.clone());
        if let Some(token) = &token {
            token.cancel();
        }
        // Snapshot: a hook that unregisters itself must not mutate the set
        // mid-walk.
        let snapshot: Vec<InterruptHook> = self
            .hooks
            .lock()
            .unwrap()
            .get(session_id)
            .map(|hooks| hooks.iter().map(|(_, h)| h.clone()).collect())
            .unwrap_or_default();
        let had_hooks = !snapshot.is_empty();
        for hook in snapshot {
            // A child that is already gone is not an error — it is the goal.
            let _ = catch_unwind(AssertUnwindSafe(|| hook()));
        }
        token.is_some() || had_hooks
    }

    /// Register a cascade hook for `session_id`; the returned id unregisters
    /// it via [`TurnRegistry::off_interrupt`].
    pub fn on_interrupt(&self, session_id: &str, hook: InterruptHook) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.hooks
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .push((id, hook));
        id
    }

    /// Unregister a hook. Idempotent.
    pub fn off_interrupt(&self, session_id: &str, hook_id: u64) {
        let mut hooks = self.hooks.lock().unwrap();
        if let Some(set) = hooks.get_mut(session_id) {
            set.retain(|(id, _)| *id != hook_id);
            if set.is_empty() {
                hooks.remove(session_id);
            }
        }
    }

    /// Mark that a drain is owed for this session regardless of what the db says.
    pub fn enqueue(&self, session_id: &str) {
        self.queued.lock().unwrap().insert(session_id.to_string());
    }

    /// Take-and-clear the nudge.
    pub fn drain(&self, session_id: &str) -> bool {
        self.queued.lock().unwrap().remove(session_id)
    }

    /// Discard a pending nudge without acting on it.
    pub fn clear_queued(&self, session_id: &str) {
        self.queued.lock().unwrap().remove(session_id);
    }
}

impl Default for TurnRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// The derived queue
// ---------------------------------------------------------------------------

/// Does this session hold input nothing has answered yet?
///
/// True when a `user` or `system` message lands after the session's last
/// `supervisor` message — which is exactly the shape a mid-turn post leaves.
///
/// Scoped to the session's OWN messages, not the inherited thread: an
/// ancestor's trailing user message was answered on the branch that owns it,
/// and treating it as unanswered here would make every fresh fork start a
/// turn nobody asked for. A `system` note owes a turn exactly like a user
/// message — that is how a finished background child wakes its spawner.
///
/// This terminates: the drained turn appends its own supervisor message on
/// every exit path, so the next check finds nothing after it.
pub fn has_unanswered_input(db: &dyn Db, session_id: &str) -> Result<bool, BoughError> {
    let own = db.messages_for(session_id)?;
    for m in own.iter().rev() {
        match m.role {
            Role::Supervisor => return Ok(false),
            Role::User | Role::System => return Ok(true),
        }
    }
    Ok(false)
}

/// Should a fresh turn start now that one has ended? The nudge is taken either
/// way, so a caller that decides not to drain does not leave it armed for later.
pub fn should_drain(
    db: &dyn Db,
    session_id: &str,
    registry: &TurnRegistry,
) -> Result<bool, BoughError> {
    let nudged = registry.drain(session_id);
    if nudged {
        return Ok(true);
    }
    has_unanswered_input(db, session_id)
}

// ---------------------------------------------------------------------------
// The retry ring
// ---------------------------------------------------------------------------

/// Re-attempts of one round, above whatever the provider client already does
/// internally. Two is enough to ride out a multi-minute network flap while a
/// truly dead network still fails the turn in minutes rather than hanging.
pub const MAX_ROUND_RETRIES: u32 = 2;

/// How long to wait before re-attempting a round the provider could not
/// deliver. The client's own backoff has already spent ~30s by the time a
/// failure reaches here, so this is the "wait for the network to come back" tier.
pub const OUTAGE_DELAY_MS: u64 = 60_000;

static TRUNCATED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)truncated mid-call").unwrap());

/// A tool call whose input never finished arriving.
///
/// The stream layer raises this rather than falling back to `{}`, and it is
/// the failure the ring exists for: the round's *content* was fine, the
/// transport cut it. Re-streaming immediately almost always lands it intact.
pub fn is_truncated_tool_call(err: &BoughError) -> bool {
    matches!(err, BoughError::Llm { .. }) && TRUNCATED.is_match(&err.to_string())
}

/// True when the failure is the user's own stop, which is never retried.
///
/// The typed replacement for TS `errName(err)` ∈ {`AbortError`,
/// `APIUserAbortError`}: the llm layer's `aborted()` constructor is an
/// `LlmError` with status 499, and nothing else carries that status.
pub fn is_abort(err: &BoughError) -> bool {
    matches!(err, BoughError::Llm { status: 499, .. })
}

/// What to do about a round that failed.
#[derive(Clone, Debug, PartialEq)]
pub struct RetryDecision {
    pub retry: bool,
    /// Milliseconds to wait first. Zero for a truncation.
    pub delay_ms: u64,
    /// One short line for `message.retry`, shown to the user as-is.
    pub reason: String,
}

/// Knobs for [`classify_round_failure`]; tests turn the outage delay down so
/// a test is not a minute.
#[derive(Clone, Debug, Default)]
pub struct ClassifyOpts {
    pub max_retries: Option<u32>,
    pub outage_delay_ms: Option<u64>,
}

/// Classify a failed round.
///
/// `attempt` is 1-based and counts attempts already made. Aborts stop
/// immediately — a user interrupt is an answer, not an error — and so does
/// anything the provider layer classes as the caller's own mistake, because
/// retrying a bad request six times only delays the message that explains it.
pub fn classify_round_failure(
    err: &BoughError,
    attempt: u32,
    opts: &ClassifyOpts,
) -> RetryDecision {
    let max_retries = opts.max_retries.unwrap_or(MAX_ROUND_RETRIES);
    let truncated = is_truncated_tool_call(err);
    let reason = if truncated {
        "the model's tool call was cut off mid-stream — re-running the round rather than \
         executing a truncated program"
            .to_string()
    } else {
        short_reason(err)
    };

    if is_abort(err) || attempt > max_retries || !(truncated || is_retryable(err)) {
        return RetryDecision { retry: false, delay_ms: 0, reason };
    }
    RetryDecision {
        retry: true,
        delay_ms: if truncated { 0 } else { opts.outage_delay_ms.unwrap_or(OUTAGE_DELAY_MS) },
        reason,
    }
}

/// One line, no newlines, bounded — this goes straight into an event payload.
pub fn short_reason(err: &BoughError) -> String {
    short_reason_text(&err.to_string(), 120)
}

/// The same fold over a raw string (the TS overload with an explicit `max`).
pub fn short_reason_text(raw: &str, max: usize) -> String {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let clipped: String = flat.chars().take(max - 1).collect();
        format!("{clipped}…")
    } else {
        flat
    }
}

// ---------------------------------------------------------------------------
// Abortable delay
// ---------------------------------------------------------------------------

/// The typed replacement for the TS
/// `DOMException("interrupted while waiting to retry", "AbortError")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayInterrupted;

impl std::fmt::Display for DelayInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("interrupted while waiting to retry")
    }
}

/// Sleep that a stop cuts short, so the caller unwinds the turn instead of
/// resuming a round the user cancelled — a plain sleep here would make the
/// stop button feel broken for a minute. `ms == 0` with an already-cancelled
/// token still errors; `ms == 0` uncancelled resolves immediately.
pub async fn abortable_delay(
    ms: u64,
    cancel: Option<&CancellationToken>,
) -> Result<(), DelayInterrupted> {
    if ms == 0 {
        return match cancel {
            Some(c) if c.is_cancelled() => Err(DelayInterrupted),
            _ => Ok(()),
        };
    }
    match cancel {
        None => {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            Ok(())
        }
        Some(c) => {
            if c.is_cancelled() {
                return Err(DelayInterrupted);
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => Ok(()),
                _ = c.cancelled() => Err(DelayInterrupted),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_claims_and_a_second_begin_errors() {
        let r = TurnRegistry::new();
        let claim = r.begin("s1").unwrap();
        assert!(r.is_running("s1"));
        let err = r.begin("s1").unwrap_err();
        assert!(err.to_string().contains("already running for session s1"));
        r.end(&claim);
        assert!(!r.is_running("s1"));
        assert!(r.begin("s1").is_ok());
    }

    #[test]
    fn end_is_identity_checked() {
        let r = TurnRegistry::new();
        let stale = r.begin("s1").unwrap();
        // Simulate supersession: the first turn is released, a second claims.
        r.end(&stale);
        let live = r.begin("s1").unwrap();
        // A late end from the superseded claim must not unregister the live one.
        r.end(&stale);
        assert!(r.is_running("s1"), "late end unregistered the replacement");
        r.end(&live);
        assert!(!r.is_running("s1"));
    }

    #[test]
    fn interrupt_aborts_fires_hooks_and_reports() {
        let r = TurnRegistry::new();
        let claim = r.begin("s1").unwrap();
        let fired = Arc::new(Mutex::new(0));
        let f = fired.clone();
        r.on_interrupt(
            "s1",
            Arc::new(move || {
                *f.lock().unwrap() += 1;
            }),
        );
        assert!(r.interrupt("s1"));
        assert!(claim.cancel.is_cancelled());
        assert_eq!(*fired.lock().unwrap(), 1);
        // Double-tap: still a boolean answer, never a failure.
        assert!(r.interrupt("s1"));
    }

    #[test]
    fn interrupt_with_nothing_to_stop_returns_false() {
        let r = TurnRegistry::new();
        assert!(!r.interrupt("idle"));
    }

    #[test]
    fn hooks_fire_even_when_the_session_is_idle() {
        // A detached child outlives its spawner's turn; only the hook can
        // reach it once the turn has ended.
        let r = TurnRegistry::new();
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();
        r.on_interrupt(
            "s1",
            Arc::new(move || {
                *f.lock().unwrap() = true;
            }),
        );
        assert!(r.interrupt("s1"), "hooks alone still count as something to stop");
        assert!(*fired.lock().unwrap());
    }

    #[test]
    fn a_throwing_hook_is_swallowed_and_later_hooks_still_fire() {
        let r = TurnRegistry::new();
        r.on_interrupt("s1", Arc::new(|| panic!("child already gone")));
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();
        r.on_interrupt(
            "s1",
            Arc::new(move || {
                *f.lock().unwrap() = true;
            }),
        );
        assert!(r.interrupt("s1"));
        assert!(*fired.lock().unwrap());
    }

    #[test]
    fn off_interrupt_unregisters() {
        let r = TurnRegistry::new();
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();
        let id = r.on_interrupt(
            "s1",
            Arc::new(move || {
                *f.lock().unwrap() = true;
            }),
        );
        r.off_interrupt("s1", id);
        r.off_interrupt("s1", id); // idempotent
        assert!(!r.interrupt("s1"));
        assert!(!*fired.lock().unwrap());
    }

    #[test]
    fn the_drain_nudge_is_take_and_clear() {
        let r = TurnRegistry::new();
        r.enqueue("s1");
        assert!(r.drain("s1"));
        assert!(!r.drain("s1"), "the nudge must not stay armed");
        r.enqueue("s1");
        r.clear_queued("s1");
        assert!(!r.drain("s1"));
    }

    // ---- queue.test.ts: the registry acceptance shapes ----------------------

    #[test]
    fn an_interrupt_cascades_to_registered_detached_children_even_when_idle() {
        let registry = TurnRegistry::new();
        let stopped = Arc::new(Mutex::new(Vec::<&str>::new()));
        let s = stopped.clone();
        let off =
            registry.on_interrupt("parent", Arc::new(move || s.lock().unwrap().push("child-a")));
        registry.on_interrupt("parent", Arc::new(|| panic!("this child is already gone")));
        let s = stopped.clone();
        registry.on_interrupt("parent", Arc::new(move || s.lock().unwrap().push("child-b")));

        // No turn running: a detached child outlives its spawner's turn
        // (spec §7), and an explicit stop still has to reach it.
        assert!(!registry.is_running("parent"));
        assert!(registry.interrupt("parent"));
        assert_eq!(
            *stopped.lock().unwrap(),
            vec!["child-a", "child-b"],
            "a throwing hook does not stop the cascade"
        );

        registry.off_interrupt("parent", off);
        registry.interrupt("parent");
        assert_eq!(*stopped.lock().unwrap(), vec!["child-a", "child-b", "child-b"]);
        assert!(!registry.interrupt("nobody"));
    }

    #[test]
    fn a_session_cannot_run_two_turns_at_once() {
        let registry = TurnRegistry::new();
        let first = registry.begin("s1").unwrap();
        let err = registry.begin("s1").unwrap_err();
        assert!(err.to_string().contains("already running"), "{err}");
        // A stale end from a superseded turn must not free the session.
        let stale = TurnClaim {
            session_id: "s1".to_string(),
            cancel: CancellationToken::new(),
            id: u64::MAX,
        };
        registry.end(&stale);
        assert!(registry.is_running("s1"), "identity-checked");
        registry.end(&first);
        assert!(!registry.is_running("s1"));
    }

    // ---- the derived queue --------------------------------------------------

    #[test]
    fn the_drain_condition_is_derived_from_the_transcript_not_an_in_memory_flag() {
        use crate::db::sqlite_db::{DbOptions, SqliteDb};
        use crate::schema::parts::{Message, Part, Session, SessionKind};
        use uuid::Uuid;

        let db = SqliteDb::new(":memory:", DbOptions::default()).unwrap();
        let registry = TurnRegistry::new();
        let session = db
            .create_session(Session {
                id: Uuid::new_v4().to_string(),
                title: "t".into(),
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
            })
            .unwrap();
        let post = |role: Role, text: &str, at: i64| {
            db.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session.id.clone(),
                role,
                parts: vec![Part::Text { text: text.to_string() }],
                pending: false,
                created_at: at,
            })
            .unwrap();
        };

        assert!(
            !has_unanswered_input(&db, &session.id).unwrap(),
            "an empty session owes nothing"
        );

        post(Role::User, "hello", 2_000);
        assert!(has_unanswered_input(&db, &session.id).unwrap());

        post(Role::Supervisor, "hi", 3_000);
        assert!(!has_unanswered_input(&db, &session.id).unwrap());

        // A harness note (a subagent's report, a job exit) owes a turn just as
        // a user message does — that is how a finished background child wakes
        // its spawner.
        post(Role::System, "[subagent finished] …", 4_000);
        assert!(has_unanswered_input(&db, &session.id).unwrap());

        // The explicit nudge is take-and-clear, and it is an OR with the
        // derived check.
        registry.enqueue(&session.id);
        assert!(should_drain(&db, &session.id, &registry).unwrap());
        assert!(should_drain(&db, &session.id, &registry).unwrap(), "still owed by the transcript");
    }

    // ---- the retry ring -----------------------------------------------------

    #[test]
    fn classification_what_retries_what_waits_and_what_does_not() {
        let truncated = BoughError::llm(
            "provider: run_steps call arrived with no arguments (truncated mid-call)",
        );
        assert!(is_truncated_tool_call(&truncated));
        let first = classify_round_failure(
            &truncated,
            1,
            &ClassifyOpts { outage_delay_ms: Some(60_000), ..Default::default() },
        );
        assert!(first.retry);
        assert_eq!(first.delay_ms, 0, "a lost frame is not an outage — re-stream now");

        // A provider outage waits, because the client's own backoff is already
        // spent.
        let outage = BoughError::llm_with("provider: 503 upstream unavailable", 503, None);
        let second = classify_round_failure(
            &outage,
            1,
            &ClassifyOpts { outage_delay_ms: Some(60_000), ..Default::default() },
        );
        assert!(second.retry);
        assert_eq!(second.delay_ms, 60_000);

        // A caller's own mistake is not retried: six attempts only delay the
        // message that explains it.
        let bad = BoughError::llm_with("bad request", 400, None);
        assert!(!classify_round_failure(&bad, 1, &ClassifyOpts::default()).retry);

        // The user's stop is an answer, not a failure.
        let abort = crate::llm::sse::aborted("provider");
        assert!(is_abort(&abort));
        assert!(!classify_round_failure(&abort, 1, &ClassifyOpts::default()).retry);

        // Bounded.
        assert!(
            !classify_round_failure(&truncated, MAX_ROUND_RETRIES + 1, &ClassifyOpts::default())
                .retry
        );

        // The reason is one bounded line — it goes straight into an event
        // payload.
        let noisy = BoughError::llm_with(
            format!("provider: 500 {}\nand\nmore", "x".repeat(500)),
            500,
            None,
        );
        let reason = classify_round_failure(&noisy, 1, &ClassifyOpts::default()).reason;
        assert!(reason.chars().count() <= 120);
        assert!(!reason.contains('\n'));
    }

    #[tokio::test]
    async fn a_retry_wait_is_cut_short_by_an_interrupt() {
        let token = CancellationToken::new();
        let waited = abortable_delay(60_000, Some(&token));
        token.cancel();
        assert_eq!(waited.await, Err(DelayInterrupted));

        // Already aborted, and the zero case.
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(abortable_delay(0, Some(&cancelled)).await, Err(DelayInterrupted));
        assert_eq!(abortable_delay(0, None).await, Ok(()));
    }
}
