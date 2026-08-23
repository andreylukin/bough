//! Branch seeding (port of `src/history/branch.ts`) — the one mechanism under
//! every history operation that spins up a new session carrying copies of an
//! existing one's turns: fork, compaction, extract, handoff, and — via a
//! directly constructed [`Seeder`] — move-into, which appends onto a session
//! that already exists.
//!
//! THE INVARIANT THIS HOLDS: **a seeded message is stamped with the real
//! clock, never with an advanced artificial one.** Messages order by
//! `(created_at, rowid)` (`Db::messages_for`), so insertion order is what
//! separates two writes that land in the same millisecond — and a branch is
//! *always* followed immediately by something else: fork's "edit & resend"
//! starts a real turn microseconds after the last seeded copy. Stamping the
//! seed with a counter that runs ahead of the wall clock (`base + i`, "one ms
//! per message") would put that turn's user message *before* the end of the
//! seed and reorder history under the user; stamping it behind would do the
//! same to the copies. Reading the clock once per message and letting `rowid`
//! break the tie is the only version that cannot go wrong in either direction.
//!
//! The clock is nevertheless injected (`BranchCtx::now`) — that is what lets
//! the tests pin every write in the whole scenario to one millisecond and
//! prove the tie-break actually carries the ordering. Injected, but never
//! *advanced*: `add()` calls `now()` once and stores exactly what it returned.
//!
//! Second: **thread-through-parents.** A branch parented at the TARGET'S
//! PARENT inherits every shared ancestor's messages for free (`thread_for` =
//! ancestors root→parent, then own), so only the target's own turns are ever
//! copied. Callers pass `parent_id: target.parent_id`, not `target.id`.
//!
//! Third: **every seeded message is announced as `message.started`**, and the
//! session is announced (`session.created`) *before* any message — a
//! `message.started` for a session the client has never heard of is a message
//! it has nowhere to put.

use serde_json::Value;
use uuid::Uuid;

use crate::bus::Bus;
use crate::errors::BoughError;
use crate::schema::events::{EventInput, EventType};
use crate::schema::parts::{Message, Part, Role, Session, SessionKind};
use crate::schema::requests::PartPick;
use crate::types::{AppCtx, Clock, Db, SharedDb};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Part picks — the selection helpers the pick-driven ops share
// ---------------------------------------------------------------------------

/// Merge duplicate picks by message id: a whole-message pick wins over a
/// partial one, and partial picks union their indexes (sorted). `None` = the
/// whole message. First-occurrence order is preserved (the TS `Map`).
///
/// Duplicates are not a client bug to reject: a UI that offers both "this
/// turn" and "this section" will send both for the same message the moment a
/// user selects a section and then the turn around it, and the obvious intent
/// is the union.
pub fn merge_picks(picks: &[PartPick]) -> Vec<(String, Option<Vec<u32>>)> {
    let mut merged: Vec<(String, Option<Vec<u32>>)> = Vec::new();
    for p in picks {
        let existing = merged.iter_mut().find(|(id, _)| *id == p.message_id);
        match (&p.parts, existing) {
            (None, Some(entry)) => entry.1 = None,
            (None, None) => merged.push((p.message_id.clone(), None)),
            (Some(_), Some((_, None))) => {} // already picked whole — a partial can't narrow it
            (Some(idx), Some((_, Some(set)))) => set.extend(idx.iter().copied()),
            (Some(idx), None) => merged.push((p.message_id.clone(), Some(idx.clone()))),
        }
    }
    for (_, sel) in merged.iter_mut() {
        if let Some(set) = sel {
            set.sort_unstable();
            set.dedup();
        }
    }
    merged
}

/// A message's parts narrowed to the picked indexes (`None` = all of them), or
/// `None` (outer) when an index is out of range — the caller turns that into
/// its own 400.
///
/// Returning `None` rather than erroring keeps this pure and keeps the error
/// text with the operation that has the vocabulary for it (fork says one thing
/// about a bad pick, extract another).
pub fn pick_parts(m: &Message, indexes: Option<&[u32]>) -> Option<Vec<Part>> {
    let Some(indexes) = indexes else {
        return Some(m.parts.clone());
    };
    if indexes.iter().any(|&i| i as usize >= m.parts.len()) {
        return None;
    }
    Some(
        indexes
            .iter()
            .map(|&i| m.parts[i as usize].clone())
            .collect(),
    )
}

/// One resolved pick: where the message sat in the thread, and the view to copy.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPick {
    /// Index in the thread the picks were resolved against — the sort key.
    pub idx: usize,
    /// The message narrowed to its picked parts. Never a reference to the original.
    pub view: Message,
}

/// Resolve part-picks against a thread: merge duplicates, validate membership
/// and part ranges, and return the picked views **in thread order**.
///
/// Order is restored here rather than trusted from the request because the
/// client sends a selection, not a sequence — a user shift-clicking upward
/// would otherwise seed a branch with its turns reversed.
///
/// `err` wraps a message in the caller's error (`ForkError`, `ExtractError`,
/// …), so one router catch renders it with the right status and this stays
/// free of HTTP.
pub fn resolve_picks(
    thread: &[Message],
    picks: &[PartPick],
    err: impl Fn(&str) -> BoughError,
) -> Result<Vec<ResolvedPick>, BoughError> {
    let mut resolved: Vec<ResolvedPick> = Vec::new();
    for (id, sel) in merge_picks(picks) {
        let Some(i) = thread.iter().position(|m| m.id == id) else {
            return Err(err("picks must be messages of the source thread"));
        };
        let Some(parts) = pick_parts(&thread[i], sel.as_deref()) else {
            return Err(err(&format!("part index out of range for message {id}")));
        };
        resolved.push(ResolvedPick {
            idx: i,
            view: Message {
                parts,
                ..thread[i].clone()
            },
        });
    }
    resolved.sort_by_key(|r| r.idx);
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Titles
// ---------------------------------------------------------------------------

/// A session title with accumulated branch prefixes stripped.
///
/// Branching a branch composes titles — fork a fork and you get
/// "fork · fork · X" — which is noise in every picker within two operations.
/// Callers prefix the BASE title instead, so the label always says what the
/// session is, once. (Note: `compacted` is deliberately NOT in the strip list.)
pub fn base_title(title: &str) -> String {
    let mut rest = title;
    loop {
        let mut stripped = false;
        for prefix in ["fork · ", "extract · ", "subagent · ", "handoff · "] {
            if let Some(r) = rest.strip_prefix(prefix) {
                rest = r;
                stripped = true;
                break;
            }
        }
        if !stripped {
            return rest.to_string();
        }
    }
}

// ---------------------------------------------------------------------------
// Opening a branch
// ---------------------------------------------------------------------------

/// What seeding needs from the world — a narrower view of [`AppCtx`], so a
/// test can hand over a db, a bus and a clock and nothing else.
#[derive(Clone)]
pub struct BranchCtx {
    pub db: SharedDb,
    pub bus: Arc<Bus>,
    /// Injected clock. Never advanced by the seeder — see the module header.
    pub now: Clock,
}

impl From<&AppCtx> for BranchCtx {
    fn from(ctx: &AppCtx) -> BranchCtx {
        BranchCtx {
            db: ctx.db.clone(),
            bus: ctx.bus.clone(),
            now: ctx.now.clone(),
        }
    }
}

/// What a branch is opened with. Optional fields are present-only-when-set on
/// the stored row, so the row and the events describing it carry exactly the
/// lineage it has and nothing that reads as an explicit null.
#[derive(Clone, Debug, Default)]
pub struct BranchSpec {
    /// The TARGET'S PARENT for fork and compaction — that is what makes the
    /// branch a sibling that inherits the shared ancestors for free. `None`
    /// for a fresh root (extract, handoff).
    pub parent_id: Option<String>,
    pub title: String,
    pub kind: Option<SessionKind>,
    /// Inherited when set: a fork works the same checkout, in place.
    pub workspace: Option<String>,
    /// The project dir the lineage is for; inherited, never re-derived.
    pub origin_dir: Option<String>,
    /// The sha the workspace's change set is measured from — a branch that
    /// inherits its target's workspace must inherit this too, or the fork
    /// shows no changes for work that is plainly in the tree.
    pub base: Option<String>,
    /// Lineage: the session this branched from (fork source / compacted session).
    pub origin_id: Option<String>,
    /// Lineage: the at-message (fork) / last picked message (compaction).
    pub origin_message_id: Option<String>,
}

/// Non-empty or absent — the TS truthiness gate (`spec.workspace ? … : {}`).
fn present(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

/// Create the branch session, publish `session.created`, and return a
/// [`Seeder`] for it.
///
/// The session is announced *before* any message is seeded, because the events
/// are consumed in order: a `message.started` for a session the client has
/// never heard of is a message it has nowhere to put.
pub fn open_branch(ctx: BranchCtx, spec: BranchSpec) -> Result<Seeder, BoughError> {
    let session = Session {
        id: Uuid::new_v4().to_string(),
        parent_id: spec.parent_id,
        title: spec.title,
        kind: spec.kind.unwrap_or(SessionKind::Root),
        created_at: (ctx.now)(),
        workspace: present(spec.workspace),
        origin_dir: present(spec.origin_dir),
        base: present(spec.base),
        origin_id: present(spec.origin_id),
        origin_message_id: present(spec.origin_message_id),
        model: None,
        effort: None,
        draft: None,
        context_tokens: None,
        cached_tokens: None,
        last_llm_at: None,
        outcome_ok: None,
        description: None,
    };
    // Announce what STORAGE kept, not the argument — `create_session` reads
    // the row back, so the event and a later `GET /sessions/:id` cannot
    // disagree.
    let stored = with_db(&ctx.db, |d| d.create_session(session))?;
    ctx.bus.publish(event(
        EventType::SessionCreated,
        &stored.id,
        to_value(&stored),
    ));
    Ok(Seeder::new(ctx, stored))
}

/// Appends seeded messages to a session, announcing each one.
///
/// Constructed directly (rather than only through [`open_branch`]) by
/// move-into, which seeds an existing session — the append behaviour is
/// identical, only the session's origin differs.
pub struct Seeder {
    ctx: BranchCtx,
    pub session: Session,
}

impl Seeder {
    pub fn new(ctx: BranchCtx, session: Session) -> Seeder {
        Seeder { ctx, session }
    }

    /// Append a message with the given role and parts, announce it, and return
    /// it as stored.
    ///
    /// `now()` is read here, once, per message. Nothing derives a timestamp
    /// from the previous one: that is the whole ordering invariant (see the
    /// module header).
    pub fn add(&self, role: Role, parts: Vec<Part>) -> Result<Message, BoughError> {
        let stored = with_db(&self.ctx.db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: self.session.id.clone(),
                role,
                parts,
                // A seeded message is history, complete on arrival. `pending`
                // is the supervisor's streaming flag; setting it would leave
                // the branch looking like a turn that never finished, and
                // nothing exists to close it.
                pending: false,
                created_at: (self.ctx.now)(),
            })
        })?;
        // Keyword search is maintained on insert. A rebuild indexes every row,
        // so skipping this would make the incremental and rebuilt indexes
        // disagree.
        index_quietly(&self.ctx.db, &stored);
        self.ctx.bus.publish(event(
            EventType::MessageStarted,
            &self.session.id,
            to_value(&stored),
        ));
        Ok(stored)
    }

    /// Append a copy of an existing message: new id, same role, deep-copied
    /// parts.
    ///
    /// The deep copy is not ceremony. The caller usually hands over a message
    /// it read from the source session, and shared structure would let a later
    /// edit on either side reach into the other's transcript — history is a
    /// tree precisely because nothing is ever rewritten in place. (`Part` is
    /// fully typed in Rust, so `Clone` produces exactly what a JSON round-trip
    /// would, without TS's stray-non-JSON-value hazard.)
    pub fn copy(&self, m: &Message) -> Result<Message, BoughError> {
        self.add(m.role, m.parts.clone())
    }
}

// ---------------------------------------------------------------------------
// Shared branch helpers
// ---------------------------------------------------------------------------

/// Carry the source's per-session model/effort pins onto the branch.
///
/// It matters most for "edit & resend": a controlled comparison — same
/// history, one changed message — and a branch that fell back to the global
/// default would answer it on a different model with nothing in the UI saying
/// so. Announced as `session.updated` rather than folded into the create,
/// because [`open_branch`] has already published `session.created`; a client
/// reconciles by id and ends up with the same row either way.
///
/// ONE copy here on purpose — the TS tree had three private clones of this in
/// fork/compact/extract.
pub fn inherit_pins(
    ctx: &BranchCtx,
    source: &Session,
    branch: Session,
) -> Result<Session, BoughError> {
    if source.model.is_none() && source.effort.is_none() {
        return Ok(branch);
    }
    if let Some(model) = &source.model {
        with_db(&ctx.db, |d| {
            d.set_session_model(&branch.id, Some(model.as_str()))
        })?;
    }
    if let Some(effort) = &source.effort {
        with_db(&ctx.db, |d| {
            d.set_session_effort(&branch.id, Some(effort.as_str()))
        })?;
    }
    let Some(stored) = with_db(&ctx.db, |d| d.get_session(&branch.id))? else {
        return Ok(branch);
    };
    ctx.bus.publish(event(
        EventType::SessionUpdated,
        &stored.id,
        to_value(&stored),
    ));
    Ok(stored)
}

/// A seeded message that fails to index is a degraded search, never a
/// half-seeded branch: propagating would abandon the copies already written
/// with no way to finish them.
fn index_quietly(db: &SharedDb, message: &Message) {
    if let Err(err) = with_db(db, |d| d.index_message(message)) {
        tracing::error!("failed to index seeded message {}: {err}", message.id);
    }
}

pub(crate) fn with_db<R>(db: &SharedDb, f: impl FnOnce(&dyn Db) -> R) -> R {
    let guard = db.lock().unwrap_or_else(|p| p.into_inner());
    f(&*guard)
}

pub(crate) fn event(r#type: EventType, session_id: &str, data: Value) -> EventInput {
    EventInput {
        r#type,
        session_id: Some(session_id.to_string()),
        data,
    }
}

pub(crate) fn to_value(v: &impl serde::Serialize) -> Value {
    serde_json::to_value(v).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests (port of `src/history/branch.test.ts`)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::events::BoughEvent;
    use crate::turn::runner::{begin_turn, RUN_STEPS};
    use crate::turn::testkit::{answering_llm, stub_deps, test_ctx};
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ---- fixtures -----------------------------------------------------------

    pub(crate) struct TestClock {
        now: AtomicI64,
        calls: AtomicUsize,
    }

    impl TestClock {
        pub(crate) fn set(&self, ms: i64) {
            self.now.store(ms, Ordering::SeqCst);
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    pub(crate) struct Fixture {
        pub db: SharedDb,
        /// Held so the fixture owns the bus the ctx publishes on; the tests
        /// assert through `events` rather than subscribing themselves.
        #[allow(dead_code)]
        pub bus: Arc<Bus>,
        pub events: Arc<Mutex<Vec<BoughEvent>>>,
        pub ctx: BranchCtx,
        /// Reads whatever `now` is set to; the tests move this, never the seeder.
        pub clock: Arc<TestClock>,
    }

    pub(crate) fn fixture() -> Fixture {
        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let bus = Arc::new(Bus::new(crate::types::system_clock()));
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.clone())
        }));
        let clock = Arc::new(TestClock {
            now: AtomicI64::new(1_700_000_000_000),
            calls: AtomicUsize::new(0),
        });
        let c = clock.clone();
        let now: Clock = Arc::new(move || {
            c.calls.fetch_add(1, Ordering::SeqCst);
            c.now.load(Ordering::SeqCst)
        });
        let ctx = BranchCtx {
            db: db.clone(),
            bus: bus.clone(),
            now,
        };
        Fixture {
            db,
            bus,
            events,
            ctx,
            clock,
        }
    }

    pub(crate) fn session(db: &SharedDb, title: &str, parent_id: Option<&str>) -> Session {
        with_db(db, |d| {
            d.create_session(Session {
                id: Uuid::new_v4().to_string(),
                title: title.to_string(),
                kind: SessionKind::Root,
                created_at: 1_000,
                parent_id: parent_id.map(|s| s.to_string()),
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
            })
        })
        .unwrap()
    }

    pub(crate) fn message(
        db: &SharedDb,
        session_id: &str,
        role: Role,
        text: &str,
        created_at: i64,
    ) -> Message {
        with_db(db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role,
                parts: vec![Part::Text {
                    text: text.to_string(),
                }],
                pending: false,
                created_at,
            })
        })
        .unwrap()
    }

    /// The text of every part of a message, joined — enough to identify a copy.
    pub(crate) fn text_of(m: &Message) -> String {
        m.parts
            .iter()
            .map(|p| match p {
                Part::Text { text } | Part::Reasoning { text, .. } => text.clone(),
                Part::ToolCall { .. } => "<tool_call>".to_string(),
                Part::ToolResult { .. } => "<tool_result>".to_string(),
                Part::Image { .. } => "<image>".to_string(),
                Part::Ask { .. } => "<ask>".to_string(),
                Part::Workflow { .. } => "<workflow>".to_string(),
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    pub(crate) fn texts_of(messages: &[Message]) -> Vec<String> {
        messages.iter().map(text_of).collect()
    }

    // ---- the ordering invariant ---------------------------------------------

    #[tokio::test]
    async fn a_seeded_branch_and_the_turn_that_follows_it_order_correctly_in_one_millisecond() {
        let f = fixture();

        // A parent with shared history, and the session about to be forked.
        let parent = session(&f.db, "parent", None);
        message(&f.db, &parent.id, Role::User, "ancestor question", 1_100);
        message(
            &f.db,
            &parent.id,
            Role::Supervisor,
            "ancestor answer",
            1_101,
        );
        let target = session(&f.db, "target", Some(&parent.id));
        message(&f.db, &target.id, Role::User, "own question", 1_200);
        message(&f.db, &target.id, Role::Supervisor, "own answer", 1_201);
        message(
            &f.db,
            &target.id,
            Role::User,
            "the turn being forked away from",
            1_202,
        );

        // Every write from here on lands in the SAME millisecond: the seed,
        // the user message, and the supervisor message the runner creates.
        // `rowid` is the only thing that can order them, which is exactly the
        // case the invariant is about.
        let ms = 1_700_000_000_777;
        f.clock.set(ms);

        let own = with_db(&f.db, |d| d.messages_for(&target.id)).unwrap();
        let seeder = open_branch(
            f.ctx.clone(),
            BranchSpec {
                // Thread-through-parents: parented at the TARGET'S PARENT, so
                // the ancestors are inherited rather than copied.
                parent_id: target.parent_id.clone(),
                title: format!("fork · {}", base_title(&target.title)),
                kind: Some(SessionKind::Fork),
                origin_id: Some(target.id.clone()),
                origin_message_id: Some(own[2].id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        for m in &own[0..2] {
            seeder.copy(m).unwrap();
        }

        // …and immediately, a real turn on the branch — fork's "edit & resend".
        let branch_id = seeder.session.id.clone();
        with_db(&f.db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: branch_id.clone(),
                role: Role::User,
                parts: vec![Part::Text {
                    text: "edited question".to_string(),
                }],
                pending: false,
                created_at: ms,
            })
        })
        .unwrap();
        let mut app_ctx = test_ctx(f.db.clone(), answering_llm("fresh answer"));
        app_ctx.now = f.ctx.now.clone();
        let started = begin_turn(&app_ctx, &branch_id, stub_deps()).unwrap();
        let outcome = started.done.await.unwrap().unwrap();
        assert_eq!(outcome.status.as_str(), "done");

        // ── the branch's own messages, in the order they were written ──
        let branch_own = with_db(&f.db, |d| d.messages_for(&branch_id)).unwrap();
        assert_eq!(
            texts_of(&branch_own),
            vec![
                "own question",
                "own answer",
                "edited question",
                "fresh answer"
            ]
        );

        // ── and the case is genuinely the same-millisecond one ──
        assert_eq!(
            branch_own.iter().map(|m| m.created_at).collect::<Vec<_>>(),
            vec![ms, ms, ms, ms],
            "the seed and the turn must share a millisecond, or this test proves nothing"
        );

        // ── the full thread: inherited ancestors first, then the branch's own ──
        assert_eq!(
            texts_of(&with_db(&f.db, |d| d.thread_for(&branch_id)).unwrap()),
            vec![
                "ancestor question",
                "ancestor answer",
                "own question",
                "own answer",
                "edited question",
                "fresh answer"
            ]
        );

        // ── nothing copied the ancestors, and the source is untouched ──
        assert_eq!(branch_own.len(), 4);
        assert_eq!(
            texts_of(&with_db(&f.db, |d| d.messages_for(&target.id)).unwrap()),
            vec![
                "own question",
                "own answer",
                "the turn being forked away from"
            ]
        );
    }

    #[tokio::test]
    async fn the_same_ordering_holds_on_the_real_clock_with_no_injected_time_at_all() {
        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let target = session(&db, "target", None);
        let now = crate::types::system_clock();
        message(&db, &target.id, Role::User, "one", now());
        message(&db, &target.id, Role::Supervisor, "two", now());

        // No injected `now`: the system clock throughout, seeder and runner alike.
        let ctx = test_ctx(db.clone(), answering_llm("live answer"));
        let seeder = open_branch(
            BranchCtx::from(&ctx),
            BranchSpec {
                parent_id: None,
                title: "fork · target".to_string(),
                kind: Some(SessionKind::Fork),
                origin_id: Some(target.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        for m in with_db(&db, |d| d.messages_for(&target.id)).unwrap() {
            seeder.copy(&m).unwrap();
        }
        with_db(&db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: seeder.session.id.clone(),
                role: Role::User,
                parts: vec![Part::Text {
                    text: "three".to_string(),
                }],
                pending: false,
                created_at: now(),
            })
        })
        .unwrap();
        let started = begin_turn(&ctx, &seeder.session.id, stub_deps()).unwrap();
        started.done.await.unwrap().unwrap();

        assert_eq!(
            texts_of(&with_db(&db, |d| d.messages_for(&seeder.session.id)).unwrap()),
            vec!["one", "two", "three", "live answer"]
        );
    }

    #[test]
    fn the_seeder_stamps_the_clock_it_is_handed_and_never_advances_it() {
        let f = fixture();
        f.clock.set(42);
        let seeder = open_branch(
            f.ctx.clone(),
            BranchSpec {
                title: "b".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        seeder
            .add(
                Role::User,
                vec![Part::Text {
                    text: "a".to_string(),
                }],
            )
            .unwrap();
        seeder
            .add(
                Role::Supervisor,
                vec![Part::Text {
                    text: "b".to_string(),
                }],
            )
            .unwrap();
        seeder
            .add(
                Role::User,
                vec![Part::Text {
                    text: "c".to_string(),
                }],
            )
            .unwrap();

        let stamps: Vec<i64> = with_db(&f.db, |d| d.messages_for(&seeder.session.id))
            .unwrap()
            .iter()
            .map(|m| m.created_at)
            .collect();
        assert_eq!(
            stamps,
            vec![42, 42, 42],
            "no per-message increment may be invented"
        );
        assert_eq!(seeder.session.created_at, 42);
        // One read per write, and the value is stored verbatim — nothing is
        // derived from the previous message's stamp.
        assert_eq!(f.clock.calls(), 4);

        // A clock that moves is followed, not overridden.
        f.clock.set(99);
        let later = seeder
            .add(
                Role::User,
                vec![Part::Text {
                    text: "d".to_string(),
                }],
            )
            .unwrap();
        assert_eq!(later.created_at, 99);
    }

    // ---- announcing ---------------------------------------------------------

    #[test]
    fn the_session_is_announced_before_its_messages_and_every_seeded_message_is_a_message_started()
    {
        let f = fixture();
        let seeder = open_branch(
            f.ctx.clone(),
            BranchSpec {
                parent_id: None,
                title: "extract · thing".to_string(),
                kind: Some(SessionKind::Root),
                workspace: Some("/tmp/checkout".to_string()),
                origin_dir: Some("/tmp/checkout".to_string()),
                base: Some("abc123".to_string()),
                origin_id: Some("src-1".to_string()),
                origin_message_id: Some("msg-1".to_string()),
            },
        )
        .unwrap();
        let first = seeder
            .add(
                Role::User,
                vec![Part::Text {
                    text: "seeded".to_string(),
                }],
            )
            .unwrap();
        let second = seeder
            .add(
                Role::Supervisor,
                vec![Part::Text {
                    text: "also seeded".to_string(),
                }],
            )
            .unwrap();

        let events = f.events.lock().unwrap().clone();
        assert_eq!(
            events.iter().map(|e| e.r#type.as_str()).collect::<Vec<_>>(),
            vec!["session.created", "message.started", "message.started"]
        );
        // The event carries what storage kept, not the argument that was passed in.
        let created: Session = serde_json::from_value(events[0].data.clone()).unwrap();
        let stored = with_db(&f.db, |d| d.get_session(&seeder.session.id))
            .unwrap()
            .unwrap();
        assert_eq!(created, stored);
        assert_eq!(created.kind, SessionKind::Root);
        assert_eq!(created.workspace.as_deref(), Some("/tmp/checkout"));
        assert_eq!(created.base.as_deref(), Some("abc123"));
        assert_eq!(created.origin_id.as_deref(), Some("src-1"));
        assert_eq!(created.origin_message_id.as_deref(), Some("msg-1"));
        assert_eq!(
            events[0].session_id.as_deref(),
            Some(seeder.session.id.as_str())
        );

        assert_eq!(events[1].data, to_value(&first));
        assert_eq!(events[2].data, to_value(&second));
        // Seeded history is complete on arrival — a pending message would look
        // like a turn that never finished, and nothing exists to close it.
        let announced: Message = serde_json::from_value(events[1].data.clone()).unwrap();
        assert!(!announced.pending);
    }

    #[test]
    fn lineage_fields_absent_from_the_spec_stay_absent_from_the_row() {
        let f = fixture();
        let seeder = open_branch(
            f.ctx.clone(),
            BranchSpec {
                title: "bare".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let stored = with_db(&f.db, |d| d.get_session(&seeder.session.id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.workspace, None);
        assert_eq!(stored.origin_id, None);
        assert_eq!(stored.base, None);
        assert_eq!(stored.parent_id, None);
    }

    // ---- copying ------------------------------------------------------------

    #[test]
    fn copy_takes_a_new_id_and_a_deep_copy_of_the_parts() {
        let f = fixture();
        let source = session(&f.db, "source", None);
        let parts = vec![
            Part::Text {
                text: "prose".to_string(),
            },
            Part::ToolCall {
                id: "call-1".to_string(),
                name: RUN_STEPS.to_string(),
                input: serde_json::json!({"code": "console.log(1)"}),
            },
            Part::ToolResult {
                call_id: "call-1".to_string(),
                output: serde_json::json!("1"),
                is_error: false,
                interrupted: None,
            },
        ];
        let original = with_db(&f.db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: source.id.clone(),
                role: Role::Supervisor,
                parts: parts.clone(),
                pending: false,
                created_at: 5,
            })
        })
        .unwrap();

        let seeder = open_branch(
            f.ctx.clone(),
            BranchSpec {
                title: "b".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let copied = seeder.copy(&original).unwrap();

        assert_ne!(copied.id, original.id);
        assert_eq!(copied.session_id, seeder.session.id);
        assert_eq!(copied.role, Role::Supervisor);
        assert_eq!(copied.parts, original.parts);

        // The copy shares no structure with the original: what storage holds
        // for the copy is exactly the parts at copy time, reachable only
        // through its own row.
        let held = with_db(&f.db, |d| d.get_message(&copied.id))
            .unwrap()
            .unwrap();
        assert_eq!(text_of(&held).split('|').next().unwrap(), "prose");
        assert_eq!(held.parts, parts);
    }

    #[test]
    fn a_seeded_message_is_searchable_immediately_and_a_rebuild_agrees() {
        let f = fixture();
        let seeder = open_branch(
            f.ctx.clone(),
            BranchSpec {
                title: "b".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        seeder
            .add(
                Role::User,
                vec![Part::Text {
                    text: "the peculiar zarquon problem".to_string(),
                }],
            )
            .unwrap();

        let incremental = with_db(&f.db, |d| d.search_messages("zarquon", None, None)).unwrap();
        assert_eq!(incremental.len(), 1);
        assert_eq!(incremental[0].session_id, seeder.session.id);

        with_db(&f.db, |d| d.rebuild_search_index()).unwrap();
        assert_eq!(
            with_db(&f.db, |d| d.search_messages("zarquon", None, None)).unwrap(),
            incremental
        );
    }

    // ---- move-into: a Seeder over an existing session -----------------------

    #[test]
    fn a_seeder_constructed_on_an_existing_session_appends_to_it() {
        let f = fixture();
        let target = session(&f.db, "target", None);
        message(&f.db, &target.id, Role::User, "already here", 1);
        f.events.lock().unwrap().clear();

        let source = session(&f.db, "source", None);
        let picked = message(&f.db, &source.id, Role::Supervisor, "moved in", 2);

        f.clock.set(7);
        Seeder::new(f.ctx.clone(), target.clone())
            .copy(&picked)
            .unwrap();

        assert_eq!(
            texts_of(&with_db(&f.db, |d| d.messages_for(&target.id)).unwrap()),
            vec!["already here", "moved in"]
        );
        // No session is created — only the append is announced.
        let events = f.events.lock().unwrap().clone();
        assert_eq!(
            events.iter().map(|e| e.r#type.as_str()).collect::<Vec<_>>(),
            vec!["message.started"]
        );
        assert_eq!(events[0].session_id.as_deref(), Some(target.id.as_str()));
        // And the source keeps its turn: this is a copy, never a move.
        assert_eq!(
            texts_of(&with_db(&f.db, |d| d.messages_for(&source.id)).unwrap()),
            vec!["moved in"]
        );
    }

    // ---- picks --------------------------------------------------------------

    fn pick(id: &str, parts: Option<Vec<u32>>) -> PartPick {
        PartPick {
            message_id: id.to_string(),
            parts,
        }
    }

    #[test]
    fn merge_picks_a_whole_message_pick_wins_partials_union_and_sort() {
        let merged = merge_picks(&[
            pick("a", Some(vec![3, 1])),
            pick("a", Some(vec![2])),
            pick("b", Some(vec![0])),
            pick("b", None),
            pick("c", None),
            pick("c", Some(vec![9])),
        ]);
        let get = |id: &str| merged.iter().find(|(k, _)| k == id).unwrap().1.clone();
        assert_eq!(get("a"), Some(vec![1, 2, 3]));
        assert_eq!(
            get("b"),
            None,
            "a whole-message pick supersedes an earlier partial"
        );
        assert_eq!(get("c"), None, "…and is not narrowed by a later partial");
    }

    #[test]
    fn pick_parts_none_takes_everything_an_out_of_range_index_is_none() {
        let m = Message {
            id: "m".to_string(),
            session_id: "s".to_string(),
            role: Role::Supervisor,
            parts: vec![
                Part::Text {
                    text: "zero".to_string(),
                },
                Part::Reasoning {
                    text: "one".to_string(),
                    meta: None,
                    model: None,
                },
                Part::Text {
                    text: "two".to_string(),
                },
            ],
            pending: false,
            created_at: 0,
        };
        assert_eq!(pick_parts(&m, None), Some(m.parts.clone()));
        assert_eq!(
            pick_parts(&m, Some(&[0, 2])),
            Some(vec![m.parts[0].clone(), m.parts[2].clone()])
        );
        assert_eq!(pick_parts(&m, Some(&[3])), None);
    }

    #[test]
    fn resolve_picks_restores_thread_order_and_reports_bad_picks_through_the_callers_error() {
        let f = fixture();
        let s = session(&f.db, "s", None);
        let a = message(&f.db, &s.id, Role::User, "first", 1);
        let b = message(&f.db, &s.id, Role::Supervisor, "second", 2);
        let c = message(&f.db, &s.id, Role::User, "third", 3);
        let thread = with_db(&f.db, |d| d.messages_for(&s.id)).unwrap();

        let err = |m: &str| BoughError::http(400, crate::errors::ErrorKind::Branch, m);

        // Sent out of order, as a user selecting upward would: order comes
        // from the thread.
        let resolved = resolve_picks(
            &thread,
            &[
                pick(&c.id, None),
                pick(&a.id, None),
                pick(&b.id, Some(vec![0])),
            ],
            err,
        )
        .unwrap();
        assert_eq!(
            resolved.iter().map(|r| r.idx).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            texts_of(&resolved.iter().map(|r| r.view.clone()).collect::<Vec<_>>()),
            vec!["first", "second", "third"]
        );

        let missing = resolve_picks(&thread, &[pick("not-in-thread", None)], err).unwrap_err();
        assert!(missing
            .to_string()
            .contains("must be messages of the source thread"));
        let range = resolve_picks(&thread, &[pick(&a.id, Some(vec![4]))], err).unwrap_err();
        assert!(range.to_string().contains(&a.id));
    }

    #[test]
    fn base_title_strips_accumulated_branch_prefixes_once() {
        assert_eq!(
            base_title("fork · fork · rename the router"),
            "rename the router"
        );
        assert_eq!(base_title("extract · subagent · handoff · thing"), "thing");
        assert_eq!(base_title("rename the router"), "rename the router");
        // Only leading prefixes, and only the known ones — a title that merely
        // mentions one keeps it.
        assert_eq!(
            base_title("why the fork · thing broke"),
            "why the fork · thing broke"
        );
        assert_eq!(base_title("compacted · 3 turns"), "compacted · 3 turns");
    }
}
