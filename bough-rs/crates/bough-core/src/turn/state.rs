//! The turn's persisted state machine, and the boot recovery that depends on
//! it (port of `src/turn/state.ts`).
//!
//! THE INVARIANT: **a session is never busy forever.** `busy_session_ids()`
//! reads `turns WHERE status = 'running'`, so a `running` row is what blocks a
//! session. If the process dies mid-turn that row survives, and the session is
//! wedged: every later post queues behind a turn that no longer exists, and
//! the transcript ends on a `pending` supervisor message that will never
//! finish. Recovery at boot is the only thing that can observe this.
//!
//! `step` is not telemetry — it is the evidence a restart reads.
//!
//! **Orphan-and-surface, not resume.** A checkpoint is deliberately not enough
//! to re-enter the loop from: re-running a program because a checkpoint says
//! it started would duplicate every side effect it had. Surfacing the
//! interruption is the honest answer.

use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::json;
use uuid::Uuid;

use crate::errors::BoughError;
use crate::schema::events::{EventInput, EventType, MessageFinishedData, MessagePartData};
use crate::schema::parts::{Part, Turn, TurnStatus, Usage};
use crate::types::{BusPort, Clock, Db, Patch, TurnPatch};

/// A turn that has ended, whatever the outcome (TS `FinalTurnStatus` =
/// `Exclude<TurnStatus, "running">`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalTurnStatus {
    Done,
    Error,
    Interrupted,
    Orphaned,
}

impl From<FinalTurnStatus> for TurnStatus {
    fn from(s: FinalTurnStatus) -> TurnStatus {
        match s {
            FinalTurnStatus::Done => TurnStatus::Done,
            FinalTurnStatus::Error => TurnStatus::Error,
            FinalTurnStatus::Interrupted => TurnStatus::Interrupted,
            FinalTurnStatus::Orphaned => TurnStatus::Orphaned,
        }
    }
}

/// The checkpoint a turn starts on, before the first round.
pub const INITIAL_STEP: &str = "start";

/// What an orphaned turn's message ends on.
///
/// It says the SERVER restarted, not that the turn failed: the distinction is
/// the whole point of the status. Work the turn had already done — files
/// written, commands run, commits made — still stands, and a user told only
/// "failed" will redo it.
pub const ORPHAN_NOTE: &str = "⚠︎ Interrupted: the server restarted before this turn finished. \
     Anything it had already done (files written, commands run) still stands — check the \
     changes, then continue.";

/// The `error` recorded on an orphaned turn row.
pub const ORPHAN_ERROR: &str = "the server restarted while this turn was running";

// ---------------------------------------------------------------------------
// Checkpointing
// ---------------------------------------------------------------------------

/// Open a turn row, `running`, against the pending supervisor message.
///
/// `now` is injected: every timestamp in the tree comes from the injected
/// clock, and a turn's `created_at` is one a test wants to pin.
pub fn start_turn(
    db: &dyn Db,
    session_id: &str,
    message_id: &str,
    now: &Clock,
) -> Result<Turn, BoughError> {
    let at = now();
    db.create_turn(Turn {
        id: Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
        status: TurnStatus::Running,
        step: INITIAL_STEP.to_string(),
        created_at: at,
        updated_at: at,
        error: None,
        usage: None,
    })
}

/// Record progress. Every call bumps `updated_at` (the db does it from its own
/// clock), which is the part that matters: a checkpoint's job is to say *when*
/// the turn last moved.
///
/// `usage` REPLACES the row's usage rather than accumulating — the runner
/// carries the turn's running total and hands the whole of it over each time.
/// Accumulating here as well would double-count every round after the first.
pub fn checkpoint(
    db: &dyn Db,
    turn_id: &str,
    step: &str,
    usage: Option<&Usage>,
) -> Result<(), BoughError> {
    db.update_turn(
        turn_id,
        TurnPatch { step: Some(step.to_string()), usage: usage.cloned(), ..Default::default() },
    )
}

/// Options for [`finish_turn`].
#[derive(Clone, Debug, Default)]
pub struct FinishOpts {
    pub error: Option<String>,
    pub usage: Option<Usage>,
    pub step: Option<String>,
}

/// Close a turn. `error` is written on every path (`opts.error ?? null` in
/// TS, [`Patch::Clear`] here), so a turn that fails and is later re-driven
/// does not keep a stale message from the previous attempt.
pub fn finish_turn(
    db: &dyn Db,
    turn_id: &str,
    status: FinalTurnStatus,
    opts: FinishOpts,
) -> Result<(), BoughError> {
    db.update_turn(
        turn_id,
        TurnPatch {
            status: Some(status.into()),
            step: opts.step,
            error: match opts.error {
                Some(e) => Patch::Set(e),
                None => Patch::Clear,
            },
            usage: opts.usage,
        },
    )
}

// ---------------------------------------------------------------------------
// Boot recovery
// ---------------------------------------------------------------------------

/// One recovered turn, for the caller's log line and the stranded-subagent
/// notice.
#[derive(Clone, Debug, PartialEq)]
pub struct OrphanedTurn {
    pub turn_id: String,
    pub session_id: String,
    pub message_id: String,
    /// The checkpoint the turn died on — where it got to.
    pub step: String,
    /// True when the supervisor message was still `pending` and has now been
    /// closed.
    pub closed_message: bool,
}

/// Hooks for [`recover_orphaned_turns`].
///
/// `on_orphan` exists so a stranded **subagent**'s parent can be told,
/// distinguishably, that its child was orphaned. A hook that panics is
/// isolated: one unnotifiable parent must not abandon the remaining orphans,
/// which would leave those sessions wedged — exactly the failure this whole
/// module exists to prevent.
#[derive(Default)]
pub struct RecoverOptions<'a> {
    pub on_orphan: Option<&'a dyn Fn(&OrphanedTurn)>,
    /// Where a throwing `on_orphan` is reported. Defaults to `tracing::error!`.
    pub on_hook_error: Option<&'a dyn Fn(&str, &OrphanedTurn)>,
}

/// Mark every still-`running` turn `orphaned`, close its pending message, and
/// announce both. Returns what was recovered. Call once at server start,
/// **before the listener binds** — a client that connects first would
/// otherwise fetch a session that looks busy and render a turn in flight.
///
/// Ordering per orphan is load-bearing: (1) the turn row first — until that
/// lands the session is still busy, and every later step can fail without
/// re-wedging it; (2) the message, note APPENDED not substituted; (3) the
/// events — `turn.finished` even when the message was already closed, because
/// that event is what a client keys "is this session busy" off; (4) the hook.
///
/// Idempotent: a second call finds nothing, because the first left no
/// `running` rows.
pub fn recover_orphaned_turns(
    db: &dyn Db,
    bus: &dyn BusPort,
    opts: RecoverOptions<'_>,
) -> Result<Vec<OrphanedTurn>, BoughError> {
    let stranded = db.turns_by_status(TurnStatus::Running)?;
    let mut recovered: Vec<OrphanedTurn> = Vec::new();

    for turn in stranded {
        // The row first. Until this lands the session is still busy, and every
        // step below can fail without re-wedging it.
        finish_turn(
            db,
            &turn.id,
            FinalTurnStatus::Orphaned,
            FinishOpts { error: Some(ORPHAN_ERROR.to_string()), ..Default::default() },
        )?;

        let message = db.get_message(&turn.message_id)?;
        let mut closed_message = false;
        if let Some(message) = message {
            if message.pending {
                let note = Part::Text { text: ORPHAN_NOTE.to_string() };
                let mut parts = message.parts.clone();
                parts.push(note.clone());
                db.update_message(&message.id, &parts, false)?;
                bus.publish(EventInput {
                    r#type: EventType::MessagePart,
                    session_id: Some(message.session_id.clone()),
                    data: serde_json::to_value(MessagePartData {
                        message_id: message.id.clone(),
                        part: note,
                    })
                    .unwrap_or_default(),
                });
                bus.publish(EventInput {
                    r#type: EventType::MessageFinished,
                    session_id: Some(message.session_id.clone()),
                    data: serde_json::to_value(MessageFinishedData {
                        message_id: message.id.clone(),
                    })
                    .unwrap_or_default(),
                });
                closed_message = true;
            }
        }

        // Emitted even when the message was already closed: `turn.finished` is
        // what a client keys its "is this session busy" state off, and a turn
        // that ends in a state nobody is told about is the same hang in the UI
        // instead of the db.
        bus.publish(EventInput {
            r#type: EventType::TurnFinished,
            session_id: Some(turn.session_id.clone()),
            data: json!({
                "turnId": turn.id,
                "sessionId": turn.session_id,
                "status": "orphaned",
                "error": ORPHAN_ERROR,
            }),
        });

        let orphan = OrphanedTurn {
            turn_id: turn.id.clone(),
            session_id: turn.session_id.clone(),
            message_id: turn.message_id.clone(),
            step: turn.step.clone(),
            closed_message,
        };
        if let Some(on_orphan) = opts.on_orphan {
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| on_orphan(&orphan))) {
                let msg = panic_text(&panic);
                match opts.on_hook_error {
                    Some(report) => report(&msg, &orphan),
                    None => {
                        tracing::error!("orphan hook threw for turn {}: {msg}", orphan.turn_id)
                    }
                }
            }
        }
        recovered.push(orphan);
    }

    Ok(recovered)
}

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic".to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests — port of `src/turn/state.test.ts`. The crash is simulated the honest
// way: a turn is really started against a real database FILE, the runner is
// abandoned mid-round (its LLM never answers), and then a fresh handle is
// opened — which is what a process death and a restart actually leave behind.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::events::BoughEvent;
    use crate::schema::parts::{Message, Role, Session, SessionKind};
    use crate::turn::runner::{begin_turn, RUN_STEPS};
    use crate::turn::testkit::{answering_llm, stub_deps, test_ctx, wedged_llm};
    use crate::types::{AppCtx, SharedDb};
    use std::sync::{Arc, Mutex};

    fn mem_db() -> SqliteDb {
        SqliteDb::new(":memory:", DbOptions::default()).unwrap()
    }

    fn seed_session(db: &dyn Db) -> Session {
        seed_session_kind(db, SessionKind::Root)
    }

    fn seed_session_kind(db: &dyn Db, kind: SessionKind) -> Session {
        db.create_session(Session {
            id: Uuid::new_v4().to_string(),
            title: "crash test".into(),
            kind,
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
        .unwrap()
    }

    /// `at` defaults to the real clock in the crash test: messages order by
    /// `(created_at, rowid)`, and the runner stamps its supervisor placeholder
    /// from the real clock too.
    fn user_message(db: &dyn Db, session_id: &str, text: &str, at: i64) -> Message {
        db.create_message(Message {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            role: Role::User,
            parts: vec![Part::Text { text: text.to_string() }],
            pending: false,
            created_at: at,
        })
        .unwrap()
    }

    fn quiet_bus() -> Arc<Bus> {
        Arc::new(Bus::new(crate::types::system_clock()))
    }

    fn now_ms() -> i64 {
        (crate::types::system_clock())()
    }

    // ---- checkpointing ------------------------------------------------------

    #[test]
    fn a_turn_opens_running_and_every_checkpoint_records_where_it_got_to() {
        let db = mem_db();
        let session = seed_session(&db);
        let message = user_message(&db, &session.id, "hi", 2_000);

        let clock: Clock = Arc::new(|| 5_000);
        let turn = start_turn(&db, &session.id, &message.id, &clock).unwrap();
        assert_eq!(turn.status, TurnStatus::Running);
        assert_eq!(turn.step, INITIAL_STEP);
        assert_eq!(turn.created_at, 5_000);
        assert_eq!(
            db.busy_session_ids().unwrap().into_iter().collect::<Vec<_>>(),
            vec![session.id.clone()]
        );

        checkpoint(&db, &turn.id, "round:1", None).unwrap();
        assert_eq!(db.get_turn(&turn.id).unwrap().unwrap().step, "round:1");

        checkpoint(
            &db,
            &turn.id,
            "tool:run_steps",
            Some(&Usage {
                input_tokens: 10,
                output_tokens: 2,
                cost_usd: Some(0.5),
                ..Default::default()
            }),
        )
        .unwrap();
        let mid = db.get_turn(&turn.id).unwrap().unwrap();
        assert_eq!(mid.step, "tool:run_steps");
        assert_eq!(mid.usage.as_ref().unwrap().input_tokens, 10);
        assert_eq!(mid.status, TurnStatus::Running, "a checkpoint never ends a turn");

        // Usage REPLACES rather than accumulates — the runner carries the
        // running total.
        checkpoint(
            &db,
            &turn.id,
            "round:2",
            Some(&Usage { input_tokens: 25, output_tokens: 5, ..Default::default() }),
        )
        .unwrap();
        assert_eq!(db.get_turn(&turn.id).unwrap().unwrap().usage.unwrap().input_tokens, 25);

        finish_turn(&db, &turn.id, FinalTurnStatus::Done, FinishOpts::default()).unwrap();
        assert_eq!(db.get_turn(&turn.id).unwrap().unwrap().status, TurnStatus::Done);
        assert_eq!(db.busy_session_ids().unwrap().len(), 0, "a finished turn frees its session");
    }

    #[test]
    fn finishing_clears_a_stale_error_rather_than_leaving_the_previous_attempts() {
        let db = mem_db();
        let session = seed_session(&db);
        let message = user_message(&db, &session.id, "hi", 2_000);
        let turn = start_turn(&db, &session.id, &message.id, &crate::types::system_clock()).unwrap();

        finish_turn(
            &db,
            &turn.id,
            FinalTurnStatus::Error,
            FinishOpts { error: Some("provider exploded".into()), ..Default::default() },
        )
        .unwrap();
        assert_eq!(
            db.get_turn(&turn.id).unwrap().unwrap().error.as_deref(),
            Some("provider exploded")
        );
        finish_turn(&db, &turn.id, FinalTurnStatus::Done, FinishOpts::default()).unwrap();
        assert_eq!(db.get_turn(&turn.id).unwrap().unwrap().error, None);
    }

    // ---- the crash ----------------------------------------------------------

    #[tokio::test]
    async fn a_mid_turn_crash_leaves_a_session_that_is_usable_after_restart() {
        let dir = std::env::temp_dir().join(format!("bough-state-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bough.db");
        let path = path.to_str().unwrap().to_string();

        // ── before the crash ──
        let db1: SharedDb =
            Arc::new(Mutex::new(SqliteDb::new(&path, DbOptions::default()).unwrap()));
        let session = {
            let g = db1.lock().unwrap();
            seed_session(&*g)
        };
        {
            let g = db1.lock().unwrap();
            user_message(&*g, &session.id, "start something long", now_ms());
        }

        // A model that never answers: the turn is genuinely mid-round when the
        // process "dies". Nothing awaits it, and its task is abandoned along
        // with the rest.
        let ctx1: AppCtx = test_ctx(db1.clone(), wedged_llm());
        let started = begin_turn(&ctx1, &session.id, stub_deps()).unwrap();
        // Let the loop reach its first round.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let (pending, busy, wedged_turn) = {
            let g = db1.lock().unwrap();
            (
                g.get_message(&started.message.id).unwrap().unwrap().pending,
                g.busy_session_ids().unwrap().contains(&session.id),
                g.turn_for_message(&started.message.id).unwrap().unwrap(),
            )
        };
        assert!(pending);
        assert!(busy);
        assert_eq!(wedged_turn.status, TurnStatus::Running);

        // ── the crash: the "process" goes away with the row still `running`.
        // The abandoned task keeps its own handle open, exactly as the TS
        // test's abandoned promise did; a second connection reads the file.

        // ── the restart ──
        let db2 = SqliteDb::new(&path, DbOptions::default()).unwrap();
        let bus2 = quiet_bus();
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = events.clone();
        bus2.subscribe(Arc::new(move |e| sink.lock().unwrap().push(e.clone())));

        let recovered =
            recover_orphaned_turns(&db2, bus2.as_ref(), RecoverOptions::default()).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].turn_id, wedged_turn.id);
        assert_eq!(recovered[0].session_id, session.id);
        assert_eq!(recovered[0].step, INITIAL_STEP, "it says where the turn got to");
        assert!(recovered[0].closed_message);

        // The row, the message, and the session.
        let row = db2.get_turn(&wedged_turn.id).unwrap().unwrap();
        assert_eq!(row.status, TurnStatus::Orphaned);
        assert_eq!(row.error.as_deref(), Some(ORPHAN_ERROR));
        let closed = db2.get_message(&started.message.id).unwrap().unwrap();
        assert!(!closed.pending, "no stuck pending message");
        assert_eq!(
            closed.parts.last(),
            Some(&Part::Text { text: ORPHAN_NOTE.to_string() })
        );
        assert_eq!(db2.busy_session_ids().unwrap().len(), 0, "the session unblocks");

        // The client is told, so a reconnecting UI stops showing a turn in
        // flight.
        {
            let events = events.lock().unwrap();
            assert_eq!(
                events.iter().map(|e| e.r#type.as_str()).collect::<Vec<_>>(),
                vec!["message.part", "message.finished", "turn.finished"]
            );
            assert_eq!(events.last().unwrap().data["status"], "orphaned");
        }

        // ── the session is usable ──
        let db2: SharedDb = Arc::new(Mutex::new(db2));
        let ctx2: AppCtx =
            test_ctx(db2.clone(), answering_llm("Picking up where we left off."));
        {
            let g = db2.lock().unwrap();
            user_message(&*g, &session.id, "try again", now_ms());
        }
        let outcome =
            begin_turn(&ctx2, &session.id, stub_deps()).unwrap().done.await.unwrap().unwrap();

        assert_eq!(outcome.status, crate::turn::runner::TurnOutcomeStatus::Done);
        {
            let g = db2.lock().unwrap();
            assert!(!g.get_message(&outcome.message_id).unwrap().unwrap().pending);
            // Two supervisor messages: the orphaned one and the new one, in order.
            let own = g.messages_for(&session.id).unwrap();
            assert_eq!(
                own.iter().map(|m| m.role).collect::<Vec<_>>(),
                vec![Role::User, Role::Supervisor, Role::User, Role::Supervisor]
            );
            assert!(own.iter().all(|m| !m.pending));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_is_idempotent_and_touches_nothing_that_already_ended() {
        let db = mem_db();
        let bus = quiet_bus();
        let session = seed_session(&db);

        let finished = user_message(&db, &session.id, "one", 2_000);
        let clock_a: Clock = Arc::new(|| 2_100);
        let done_turn = start_turn(&db, &session.id, &finished.id, &clock_a).unwrap();
        finish_turn(&db, &done_turn.id, FinalTurnStatus::Done, FinishOpts::default()).unwrap();

        let stranded_message = db
            .create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session.id.clone(),
                role: Role::Supervisor,
                parts: vec![Part::Text { text: "partial answer".into() }],
                pending: true,
                created_at: 3_000,
            })
            .unwrap();
        let clock_b: Clock = Arc::new(|| 3_000);
        let stranded = start_turn(&db, &session.id, &stranded_message.id, &clock_b).unwrap();

        assert_eq!(
            recover_orphaned_turns(&db, bus.as_ref(), RecoverOptions::default()).unwrap().len(),
            1
        );
        assert_eq!(
            recover_orphaned_turns(&db, bus.as_ref(), RecoverOptions::default()).unwrap().len(),
            0,
            "a second boot finds nothing"
        );

        assert_eq!(
            db.get_turn(&done_turn.id).unwrap().unwrap().status,
            TurnStatus::Done,
            "a finished turn is untouched"
        );
        assert_eq!(db.get_turn(&stranded.id).unwrap().unwrap().status, TurnStatus::Orphaned);
        // The partial answer survives — the note is appended, not substituted.
        let parts = db.get_message(&stranded_message.id).unwrap().unwrap().parts;
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], Part::Text { text: "partial answer".into() });
    }

    #[test]
    fn a_message_already_closed_still_gets_its_turn_finished_and_the_hook_still_fires() {
        let db = mem_db();
        let bus = quiet_bus();
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e| sink.lock().unwrap().push(e.clone())));
        let session = seed_session(&db);

        // The message was closed but the row never was — the crash landed
        // between the two writes.
        let message = db
            .create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session.id.clone(),
                role: Role::Supervisor,
                parts: vec![Part::Text { text: "answered".into() }],
                pending: false,
                created_at: 3_000,
            })
            .unwrap();
        let turn = start_turn(&db, &session.id, &message.id, &crate::types::system_clock()).unwrap();

        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let s = seen.clone();
        let on_orphan = move |o: &OrphanedTurn| s.lock().unwrap().push(o.turn_id.clone());
        let recovered = recover_orphaned_turns(
            &db,
            bus.as_ref(),
            RecoverOptions { on_orphan: Some(&on_orphan), on_hook_error: None },
        )
        .unwrap();

        assert!(!recovered[0].closed_message);
        assert_eq!(*seen.lock().unwrap(), vec![turn.id.clone()]);
        assert_eq!(
            events.lock().unwrap().iter().map(|e| e.r#type.as_str()).collect::<Vec<_>>(),
            vec!["turn.finished"]
        );
        assert_eq!(
            db.get_message(&message.id).unwrap().unwrap().parts.len(),
            1,
            "a closed message is not appended to"
        );
    }

    #[test]
    fn a_throwing_orphan_hook_does_not_abandon_the_remaining_orphans() {
        let db = mem_db();
        let bus = quiet_bus();
        let a = seed_session(&db);
        let b = seed_session(&db);
        for s in [&a, &b] {
            let m = db
                .create_message(Message {
                    id: Uuid::new_v4().to_string(),
                    session_id: s.id.clone(),
                    role: Role::Supervisor,
                    parts: vec![],
                    pending: true,
                    created_at: 3_000,
                })
                .unwrap();
            start_turn(&db, &s.id, &m.id, &crate::types::system_clock()).unwrap();
        }

        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let e = errors.clone();
        let on_orphan = |_: &OrphanedTurn| panic!("the parent notice failed");
        let on_hook_error = move |msg: &str, _: &OrphanedTurn| e.lock().unwrap().push(msg.into());
        let recovered = recover_orphaned_turns(
            &db,
            bus.as_ref(),
            RecoverOptions { on_orphan: Some(&on_orphan), on_hook_error: Some(&on_hook_error) },
        )
        .unwrap();

        assert_eq!(recovered.len(), 2);
        assert_eq!(errors.lock().unwrap().len(), 2);
        assert_eq!(db.busy_session_ids().unwrap().len(), 0, "every session unblocked regardless");
    }

    #[test]
    fn recovery_leaves_the_run_steps_transcript_replayable() {
        // A turn that died between a tool call and its result: replay has to
        // close the pair, and recovery must not invent one on the message
        // itself.
        let db = mem_db();
        let bus = quiet_bus();
        let session = seed_session(&db);
        let message = db
            .create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session.id.clone(),
                role: Role::Supervisor,
                parts: vec![Part::ToolCall {
                    id: "c1".into(),
                    name: RUN_STEPS.into(),
                    input: json!({"code": "x"}),
                }],
                pending: true,
                created_at: 3_000,
            })
            .unwrap();
        start_turn(&db, &session.id, &message.id, &crate::types::system_clock()).unwrap();
        recover_orphaned_turns(&db, bus.as_ref(), RecoverOptions::default()).unwrap();

        let parts = db.get_message(&message.id).unwrap().unwrap().parts;
        assert!(matches!(parts[0], Part::ToolCall { .. }));
        assert!(matches!(parts[1], Part::Text { .. }));
        assert_eq!(parts.len(), 2);
    }

    use uuid::Uuid;
}
