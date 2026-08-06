//! Fork-at-message, and "edit & resend" (port of `src/history/fork.ts`) — the
//! backend for the UI's "edit any past turn to branch from it" affordance.
//!
//! THE INVARIANT THIS HOLDS: **the source session is byte-identical
//! afterwards.** History is a tree and nothing is ever destructively
//! rewritten, so a fork is only ever a sequence of WRITES TO THE BRANCH: it
//! reads the target's own messages, copies them into a session that did not
//! exist a moment ago, and never updates, deletes or re-parents a single row
//! of the thing it forked. "Edit & resend" is the case that makes this worth
//! stating in capitals — the user's mental model there is "change what I said
//! and try again", which reads as a mutation and is implemented as one
//! nowhere: the edited text is a NEW message on a NEW session.
//!
//! Second: **why a fork is a SIBLING rather than a child.** The branch is
//! parented at `target.parent_id`, not at the target, so `thread_for` hands it
//! every shared ancestor for free and only the target's OWN turns are ever
//! copied. Parenting it at the target would inherit the very messages the
//! fork exists to cut away. That is also why a fork point outside the
//! session's own messages is a **400 and not a deeper walk**: the ancestor is
//! a different session's rows and the fork does not own them.
//!
//! The four cuts, all of which seed the copies STRICTLY BEFORE `atMessageId`
//! first:
//!
//!   - `editedText`      — append the replacement as a new user message and run
//!                         a real turn from there. It may only replace a USER
//!                         message (400 otherwise).
//!   - (nothing)         — also copy the at-message itself: a branch point
//!                         sitting ready for new input, with no turn run.
//!   - `exclusive`       — skip that copy; the branch ends strictly before the
//!                         at-message.
//!   - `atPart`          — cut INSIDE the at-message: copy it truncated to
//!                         `parts[0..=atPart]`. Here `editedText` is a fresh
//!                         user message appended after the cut, so any
//!                         at-message role is allowed.
//!
//! `exclusive` is meaningful only for the plain branch-point case, and is a
//! no-op otherwise rather than a third error. A cut that strands a `tool_call`
//! without its `tool_result` is legal and expected — `turn/replay` closes it
//! with a synthetic "(interrupted)" result.

use std::sync::Arc;

use crate::errors::{BoughError, ErrorKind};
use crate::schema::parts::{Message, Part, Role, Session, SessionKind};
use crate::schema::requests::ForkBody;
use crate::types::AppCtx;

use super::seed::{base_title, inherit_pins, open_branch, with_db, BranchCtx, BranchSpec};

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// How the resend's turn is started. Production reads `ctx.turn_starter()`;
/// a test injects a closure (which may itself run a real turn and stash the
/// handle it wants to await).
pub type ForkStarter = Arc<dyn Fn(&AppCtx, &Session, &Message) + Send + Sync>;

/// Injection for [`fork`]. Absent `start` = `ctx.turn_starter()`. Absent there
/// too = the branch is seeded, not run.
#[derive(Clone, Default)]
pub struct ForkDeps {
    pub start: Option<ForkStarter>,
}

#[derive(Debug)]
pub struct ForkResult {
    /// The branch, as storage kept it (pins included).
    pub session: Session,
    /// The branch's own messages, in the order they were seeded.
    pub messages: Vec<Message>,
    /// A real turn was started on the branch. False for every cut that only
    /// seeds — and false, deliberately, when `editedText` was given but no
    /// starter is wired: the edited message is on the branch either way, and
    /// the caller must not be told a turn is coming that is not.
    pub turn_started: bool,
}

fn fork_err(message: impl Into<String>) -> BoughError {
    BoughError::http(400, ErrorKind::Fork, message)
}

// ---------------------------------------------------------------------------
// The operation
// ---------------------------------------------------------------------------

/// Fork `session_id` at `body.at_message_id`. `ForkError` (400) for a fork
/// point this session cannot cut at, `NotFoundError` (404) for an unknown
/// session.
///
/// Every validation happens BEFORE `open_branch`, and that ordering is
/// load-bearing: the seeder announces `session.created` the moment it is
/// opened, so a check that ran afterwards would leave an empty half-seeded
/// branch in the user's session list every time a client sent a bad fork
/// point.
pub fn fork(
    ctx: &AppCtx,
    session_id: &str,
    body: &ForkBody,
    deps: ForkDeps,
) -> Result<ForkResult, BoughError> {
    let source = with_db(&ctx.db, |d| d.get_session(session_id))?
        .ok_or_else(|| BoughError::not_found(format!("session {session_id} not found")))?;

    // The session's OWN messages. Not `thread_for`: inherited ancestors are a
    // different session's rows, and a branch cannot cut into them.
    let own = with_db(&ctx.db, |d| d.messages_for(session_id))?;
    let at_idx = own
        .iter()
        .position(|m| m.id == body.at_message_id)
        .ok_or_else(|| fork_err(bad_fork_point(ctx, &source, &body.at_message_id)))?;
    let at = &own[at_idx];

    let edited = body.edited_text.is_some();
    // Trimmed like the HTTP post path, and empty is refused for the same
    // reason it is there: an empty user message is a turn asked to answer
    // nothing, and it replays as an empty text block several providers reject
    // outright.
    let edited_text = body.edited_text.as_deref().unwrap_or("").trim().to_string();
    if edited && edited_text.is_empty() {
        return Err(fork_err(
            "editedText is empty — send the replacement text, or omit editedText entirely \
             to branch at that message and leave the composer ready for new input.",
        ));
    }
    // Without `atPart`, `editedText` REPLACES the at-message, which is only
    // coherent for a user turn: an edited supervisor message would be a
    // sentence the model never wrote, replayed to it next turn as though it
    // had. With `atPart` it is a fresh message appended after the cut, so any
    // role is fine.
    if edited && body.at_part.is_none() && at.role != Role::User {
        return Err(fork_err(format!(
            "editedText can only replace a user message, and {} is a {} message. Fork it \
             without editedText to branch from it, or pass the user turn you meant to edit.",
            body.at_message_id,
            role_str(at.role),
        )));
    }
    if let Some(at_part) = body.at_part {
        if at_part as usize >= at.parts.len() {
            return Err(fork_err(format!(
                "atPart {} is out of range for message {}, which has {} part(s) — the last \
                 cut point is {}.",
                at_part,
                body.at_message_id,
                at.parts.len(),
                at.parts.len() - 1,
            )));
        }
    }

    // Titled after the branch point so several forks of one session stay
    // tellable apart in the pickers, falling back to the source's BASE title —
    // a fork of a fork must not compound into "fork · fork · X".
    let excerpt = excerpt_of(at);
    let title_tail = if excerpt.is_empty() {
        base_title(&source.title)
    } else {
        excerpt
    };
    let branch_ctx = BranchCtx::from(ctx);
    let seeder = open_branch(
        branch_ctx.clone(),
        BranchSpec {
            parent_id: source.parent_id.clone(),
            title: format!("fork · {title_tail}"),
            kind: Some(SessionKind::Fork),
            workspace: source.workspace.clone(),
            origin_dir: source.origin_dir.clone(),
            // A branch sharing its target's checkout must share the sha that
            // checkout's change set is measured from.
            base: source.base.clone(),
            origin_id: Some(source.id.clone()), // lineage: the forked-from session…
            origin_message_id: Some(body.at_message_id.clone()), // …and the at-message
        },
    )?;
    let branch = inherit_pins(&branch_ctx, &source, seeder.session.clone())?;

    // The prefix, strictly before the fork point. Every mode starts here.
    let mut messages: Vec<Message> = Vec::new();
    for m in &own[0..at_idx] {
        messages.push(seeder.copy(m)?);
    }

    if let Some(at_part) = body.at_part {
        // Mid-message cut: the at-message survives truncated to the cut point.
        let truncated = Message {
            parts: at.parts[0..=(at_part as usize)].to_vec(),
            ..at.clone()
        };
        messages.push(seeder.copy(&truncated)?);
    } else if !edited && !body.exclusive.unwrap_or(false) {
        // Plain branch point: include the fork-point message, ready for new
        // input — unless the caller asked for an exclusive cut to re-send it
        // itself.
        messages.push(seeder.copy(at)?);
    }

    if !edited {
        return Ok(ForkResult {
            session: branch,
            messages,
            turn_started: false,
        });
    }

    // Edit & resend. The user message goes through the seeder like every other
    // seeded message — announced and indexed the same way — and then the
    // ordinary turn path runs it. A branch created microseconds ago cannot be
    // busy, so there is no queue check here; the ordering that keeps this
    // message after the copies is the seeder's.
    let user = seeder.add(Role::User, vec![Part::Text { text: edited_text }])?;
    messages.push(user.clone());

    // An unwired starter degrades to "the branch exists carrying the edited
    // message", never to a crash.
    if let Some(start) = &deps.start {
        start(ctx, &branch, &user);
    } else if let Some(starter) = ctx.turn_starter() {
        starter.start_turn(ctx, &branch, &user);
    } else {
        return Ok(ForkResult {
            session: branch,
            messages,
            turn_started: false,
        });
    }

    Ok(ForkResult {
        session: branch,
        messages,
        turn_started: true,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Supervisor => "supervisor",
        Role::System => "system",
    }
}

/// Why this message cannot be a fork point, in terms of the move that works.
///
/// The interesting case: the id is real and the user can see it in this
/// session's transcript, because the transcript is ancestors ++ own. A bare
/// "not found" would send them looking for a client bug; naming the ancestor
/// names the session they should be forking.
fn bad_fork_point(ctx: &AppCtx, source: &Session, at_message_id: &str) -> String {
    let message = with_db(&ctx.db, |d| d.get_message(at_message_id))
        .ok()
        .flatten();
    let Some(message) = message else {
        return format!("no message {at_message_id} exists");
    };
    if message.session_id == source.id {
        // Belongs here but is not in `messages_for` — an ordering or storage
        // defect, not a user error. Say what was actually observed rather than
        // blaming the request.
        return format!(
            "message {at_message_id} is not in session {}'s message list",
            source.id
        );
    }
    let inherited = with_db(&ctx.db, |d| d.ancestor_chain(&source.id))
        .map(|chain| chain.iter().any(|s| s.id == message.session_id))
        .unwrap_or(false);
    if inherited {
        format!(
            "message {at_message_id} belongs to ancestor session {}, whose history this \
             session inherits but does not own — fork {} instead",
            message.session_id, message.session_id
        )
    } else {
        format!(
            "message {at_message_id} belongs to session {}, not {} — fork a session at one \
             of its own messages",
            message.session_id, source.id
        )
    }
}

/// The at-message's first line of text, for the branch title.
///
/// Cut at a WORD boundary with an ellipsis, because this string is a heading.
/// A hard 48-char slice produced titles like "fork · Create a Python file that
/// implements a binary se" — every fork read as a sentence that got
/// interrupted. The ellipsis is the difference between "shortened" and
/// "broken".
fn excerpt_of(at: &Message) -> String {
    let line = at
        .parts
        .iter()
        .find_map(|p| match p {
            Part::Text { text } => Some(text.lines().next().unwrap_or("").trim().to_string()),
            _ => None,
        })
        .unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= 48 {
        return line;
    }
    let cut: String = chars[0..48].iter().collect();
    // Back up to the last space, unless that would throw away most of the
    // excerpt — one 48-character word is better shown truncated than reduced
    // to nothing.
    let space = cut.rfind(' ');
    let kept = match space {
        Some(i) if i > 24 => &cut[..i],
        _ => cut.as_str(),
    };
    let kept = kept.strip_suffix([',', ';', ':', '.']).unwrap_or(kept);
    format!("{kept}…")
}

// ---------------------------------------------------------------------------
// Tests (port of `src/history/fork.test.ts` — the core half; route tests live
// in `bough-server::history_ops`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::history::ops::seed::tests::texts_of;
    use crate::schema::events::BoughEvent;
    use crate::turn::queue::TurnRegistry;
    use crate::turn::runner::{begin_turn, StartedTurn, RUN_STEPS};
    use crate::turn::testkit::{scripted_llm, stop, stub_deps, text, ScriptedLlm, ScriptedRound};
    use crate::types::{system_clock, HostState, LlmContentBlock, SharedDb};
    use std::sync::{Mutex, RwLock};
    use uuid::Uuid;

    // ---- fixtures -----------------------------------------------------------

    struct Fixture {
        db: SharedDb,
        events: Arc<Mutex<Vec<BoughEvent>>>,
        ctx: AppCtx,
        /// Every fake round the LLM was asked for, for the replay assertions.
        llm: Arc<ScriptedLlm>,
    }

    /// A model that says one thing and stops, in the same response.
    fn one_round(said: &str) -> Vec<ScriptedRound> {
        vec![ScriptedRound {
            content: vec![text(said), stop("stop-1")],
            ..Default::default()
        }]
    }

    fn fixture(said: &str) -> Fixture {
        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let bus = Arc::new(Bus::new(system_clock()));
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.clone())
        }));
        let llm = scripted_llm(one_round(said));
        let ctx = AppCtx {
            db: db.clone(),
            bus,
            llm: Some(llm.clone()),
            model: Some("claude-opus-4-8".into()),
            effort: None,
            now: system_clock(),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: Arc::new(TurnRegistry::new()),
            model_defaults_path: None,
        };
        Fixture {
            db,
            events,
            ctx,
            llm,
        }
    }

    /// The starter the resend tests inject: a real turn whose handle they can
    /// await.
    fn real_turn() -> (ForkStarter, Arc<Mutex<Vec<StartedTurn>>>) {
        let handles: Arc<Mutex<Vec<StartedTurn>>> = Arc::new(Mutex::new(vec![]));
        let sink = handles.clone();
        let starter: ForkStarter = Arc::new(move |ctx, session, _message| {
            let started = begin_turn(ctx, &session.id, stub_deps()).unwrap();
            sink.lock().unwrap().push(started);
        });
        (starter, handles)
    }

    async fn await_turn(handles: &Arc<Mutex<Vec<StartedTurn>>>) {
        let started = handles.lock().unwrap().pop().expect("a turn was started");
        started.done.await.unwrap().unwrap();
    }

    fn session(db: &SharedDb, over: Session) -> Session {
        with_db(db, |d| d.create_session(over)).unwrap()
    }

    fn base_session(title: &str) -> Session {
        Session {
            id: Uuid::new_v4().to_string(),
            title: title.to_string(),
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
        }
    }

    fn message(db: &SharedDb, session_id: &str, role: Role, parts: Vec<Part>, at: i64) -> Message {
        with_db(db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role,
                parts,
                pending: false,
                created_at: at,
            })
        })
        .unwrap()
    }

    fn text_message(db: &SharedDb, session_id: &str, role: Role, body: &str, at: i64) -> Message {
        message(
            db,
            session_id,
            role,
            vec![Part::Text {
                text: body.to_string(),
            }],
            at,
        )
    }

    struct Scenario {
        parent: Session,
        target: Session,
        own: Vec<Message>,
    }

    /// A parent with shared history, and the session about to be forked:
    ///
    ///   parent : "ancestor question" / "ancestor answer"
    ///   target : "first ask" / "first answer" / "second ask" / "second answer"
    ///
    /// The fork point in most tests is `own[2]` — "second ask", a user turn
    /// with a supervisor answer after it, so a cut that failed to stop would
    /// be visible.
    fn scenario(f: &Fixture) -> Scenario {
        let parent = session(&f.db, base_session("parent"));
        text_message(&f.db, &parent.id, Role::User, "ancestor question", 1_000);
        text_message(
            &f.db,
            &parent.id,
            Role::Supervisor,
            "ancestor answer",
            1_001,
        );
        let target = session(
            &f.db,
            Session {
                parent_id: Some(parent.id.clone()),
                workspace: Some("/tmp/checkout".into()),
                origin_dir: Some("/tmp/checkout".into()),
                base: Some("abc123".into()),
                ..base_session("rename the router")
            },
        );
        text_message(&f.db, &target.id, Role::User, "first ask", 1_002);
        text_message(&f.db, &target.id, Role::Supervisor, "first answer", 1_003);
        text_message(&f.db, &target.id, Role::User, "second ask", 1_004);
        text_message(&f.db, &target.id, Role::Supervisor, "second answer", 1_005);
        let own = with_db(&f.db, |d| d.messages_for(&target.id)).unwrap();
        Scenario {
            parent,
            target,
            own,
        }
    }

    /// Everything about the source that a fork must not disturb.
    fn snapshot(db: &SharedDb, session_id: &str) -> String {
        let session = with_db(db, |d| d.get_session(session_id)).unwrap();
        let messages = with_db(db, |d| d.messages_for(session_id)).unwrap();
        serde_json::to_string(&(session, messages)).unwrap()
    }

    fn own_texts(f: &Fixture, session_id: &str) -> Vec<String> {
        texts_of(&with_db(&f.db, |d| d.messages_for(session_id)).unwrap())
    }

    fn body(at: &str) -> ForkBody {
        ForkBody {
            at_message_id: at.to_string(),
            at_part: None,
            edited_text: None,
            exclusive: None,
            summarize_abandoned: None,
        }
    }

    // ---- mode 1: editedText — edit & resend ---------------------------------

    #[tokio::test]
    async fn edited_text_seeds_the_prefix_appends_the_replacement_and_runs_a_real_turn() {
        let f = fixture("fresh answer");
        let s = scenario(&f);
        let before = snapshot(&f.db, &s.target.id);
        let (start, handles) = real_turn();

        let result = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                edited_text: Some("second ask, rephrased".into()),
                ..body(&s.own[2].id) // "second ask"
            },
            ForkDeps { start: Some(start) },
        )
        .unwrap();

        assert!(result.turn_started);
        await_turn(&handles).await;

        // The at-message is REPLACED, not copied: the prefix, then the edit,
        // then the model's fresh answer. Nothing from "second ask" onward
        // survives.
        assert_eq!(
            own_texts(&f, &result.session.id),
            vec![
                "first ask",
                "first answer",
                "second ask, rephrased",
                "fresh answer"
            ]
        );
        assert_eq!(
            with_db(&f.db, |d| d.messages_for(&result.session.id))
                .unwrap()
                .iter()
                .map(|m| m.role)
                .collect::<Vec<_>>(),
            vec![Role::User, Role::Supervisor, Role::User, Role::Supervisor]
        );

        // A SIBLING: parented at the target's parent, so the ancestors are
        // inherited rather than copied, and the branch's thread is the whole
        // conversation.
        assert_eq!(
            result.session.parent_id.as_deref(),
            Some(s.parent.id.as_str())
        );
        assert_eq!(result.session.kind, SessionKind::Fork);
        assert_eq!(
            texts_of(&with_db(&f.db, |d| d.thread_for(&result.session.id)).unwrap()),
            vec![
                "ancestor question",
                "ancestor answer",
                "first ask",
                "first answer",
                "second ask, rephrased",
                "fresh answer"
            ]
        );

        // Lineage, for the tree view: what it branched from, and where.
        assert_eq!(
            result.session.origin_id.as_deref(),
            Some(s.target.id.as_str())
        );
        assert_eq!(
            result.session.origin_message_id.as_deref(),
            Some(s.own[2].id.as_str())
        );
        // The same checkout, worked in place — and the sha its change set is
        // measured from.
        assert_eq!(result.session.workspace.as_deref(), Some("/tmp/checkout"));
        assert_eq!(result.session.origin_dir.as_deref(), Some("/tmp/checkout"));
        assert_eq!(result.session.base.as_deref(), Some("abc123"));
        // Titled after the branch point.
        assert_eq!(result.session.title, "fork · second ask");

        // THE INVARIANT: the source is byte-identical.
        assert_eq!(snapshot(&f.db, &s.target.id), before);
    }

    #[tokio::test]
    async fn the_resent_turn_replays_the_seeded_prefix_and_nothing_after_the_cut() {
        let f = fixture("fresh answer");
        let s = scenario(&f);
        let (start, handles) = real_turn();

        let result = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                edited_text: Some("second ask, rephrased".into()),
                ..body(&s.own[2].id)
            },
            ForkDeps { start: Some(start) },
        )
        .unwrap();
        assert!(result.turn_started);
        await_turn(&handles).await;

        // One round was asked for, and what it carried is the branch's thread:
        // inherited ancestors, the copied prefix, the edit — and no trace of
        // the turn forked away from, which is the whole point of the operation.
        let calls = f.llm.calls();
        assert_eq!(calls.len(), 1);
        let sent: Vec<String> = calls[0]
            .messages
            .iter()
            .flat_map(|m| m.content.iter())
            .map(|b| match b {
                LlmContentBlock::Text { text } => text.clone(),
                other => format!("<{}>", block_type(other)),
            })
            .collect();
        assert_eq!(
            sent,
            vec![
                "ancestor question",
                "ancestor answer",
                "first ask",
                "first answer",
                "second ask, rephrased"
            ]
        );
    }

    fn block_type(b: &LlmContentBlock) -> &'static str {
        match b {
            LlmContentBlock::Text { .. } => "text",
            LlmContentBlock::Reasoning { .. } => "reasoning",
            LlmContentBlock::ToolUse { .. } => "tool_use",
            LlmContentBlock::ToolResult { .. } => "tool_result",
            LlmContentBlock::Image { .. } => "image",
        }
    }

    #[tokio::test]
    async fn edited_text_is_trimmed_and_an_empty_one_is_a_400() {
        let f = fixture("unused");
        let s = scenario(&f);

        let result = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                edited_text: Some("  padded  \n".into()),
                ..body(&s.own[2].id)
            },
            ForkDeps::default(),
        )
        .unwrap();
        assert_eq!(own_texts(&f, &result.session.id).last().unwrap(), "padded");
        // No starter wired: the branch exists carrying the edit, and says so
        // honestly.
        assert!(!result.turn_started);

        let err = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                edited_text: Some("   ".into()),
                ..body(&s.own[2].id)
            },
            ForkDeps::default(),
        )
        .unwrap_err();
        assert_eq!(err.status(), 400);
        assert_eq!(err.name(), "ForkError");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    // ---- mode 2: no editedText — a plain branch point -----------------------

    #[tokio::test]
    async fn without_edited_text_the_at_message_is_copied_too_and_no_turn_runs() {
        let f = fixture("unused");
        let s = scenario(&f);
        let before = snapshot(&f.db, &s.target.id);
        f.events.lock().unwrap().clear();

        let result = fork(
            &f.ctx,
            &s.target.id,
            &body(&s.own[2].id),
            ForkDeps::default(),
        )
        .unwrap();

        assert!(!result.turn_started);
        assert_eq!(
            own_texts(&f, &result.session.id),
            vec!["first ask", "first answer", "second ask"]
        );
        // Seeded history is complete on arrival — a pending copy would look
        // like a turn that never finished, with nothing left to close it.
        let copied = with_db(&f.db, |d| d.messages_for(&result.session.id)).unwrap();
        assert!(copied.iter().all(|m| !m.pending));
        // Copies, not moves: new ids.
        assert!(!copied.iter().any(|m| s.own.iter().any(|o| o.id == m.id)));
        assert_eq!(snapshot(&f.db, &s.target.id), before);

        // The whole branch is announced, session first: a `message.started`
        // for a session the client has never heard of is a message it has
        // nowhere to put.
        assert_eq!(
            f.events
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.r#type.as_str())
                .collect::<Vec<_>>(),
            vec![
                "session.created",
                "message.started",
                "message.started",
                "message.started"
            ]
        );
    }

    // ---- mode 3: exclusive — the cut lands strictly before ------------------

    #[tokio::test]
    async fn exclusive_skips_the_at_message_the_branch_ends_strictly_before_it() {
        let f = fixture("unused");
        let s = scenario(&f);
        let before = snapshot(&f.db, &s.target.id);

        let result = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                exclusive: Some(true),
                ..body(&s.own[2].id)
            },
            ForkDeps::default(),
        )
        .unwrap();

        assert_eq!(
            own_texts(&f, &result.session.id),
            vec!["first ask", "first answer"]
        );
        assert!(!result.turn_started);
        // Still the at-message that lineage points at — that is where the cut
        // was made, whether or not the message itself came along.
        assert_eq!(
            result.session.origin_message_id.as_deref(),
            Some(s.own[2].id.as_str())
        );
        assert_eq!(snapshot(&f.db, &s.target.id), before);
    }

    #[tokio::test]
    async fn exclusive_is_a_no_op_where_the_at_messages_fate_is_already_decided() {
        let f = fixture("fresh answer");
        let s = scenario(&f);
        let (start, handles) = real_turn();

        // With editedText the at-message is replaced…
        let edited = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                edited_text: Some("rephrased".into()),
                exclusive: Some(true),
                ..body(&s.own[2].id)
            },
            ForkDeps { start: Some(start) },
        )
        .unwrap();
        await_turn(&handles).await;
        assert_eq!(
            own_texts(&f, &edited.session.id),
            vec!["first ask", "first answer", "rephrased", "fresh answer"]
        );

        // …and with atPart it is truncated. Neither is contradicted by
        // `exclusive`; both have already said what becomes of it.
        let cut = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                at_part: Some(0),
                exclusive: Some(true),
                ..body(&s.own[2].id)
            },
            ForkDeps::default(),
        )
        .unwrap();
        assert_eq!(
            own_texts(&f, &cut.session.id),
            vec!["first ask", "first answer", "second ask"]
        );
    }

    // ---- mode 4: atPart — the cut lands inside the at-message ---------------

    #[tokio::test]
    async fn at_part_copies_the_at_message_truncated_to_the_cut_point() {
        let f = fixture("unused");
        let s = scenario(&f);
        // A supervisor turn that narrated, ran two programs, and only then
        // answered.
        let rich = message(
            &f.db,
            &s.target.id,
            Role::Supervisor,
            vec![
                Part::Text {
                    text: "looking".into(),
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: RUN_STEPS.into(),
                    input: serde_json::json!({"code": "one()"}),
                },
                Part::ToolResult {
                    call_id: "c1".into(),
                    output: serde_json::json!("boom"),
                    is_error: true,
                    interrupted: None,
                },
                Part::Text {
                    text: "that failed, trying again".into(),
                },
                Part::ToolCall {
                    id: "c2".into(),
                    name: RUN_STEPS.into(),
                    input: serde_json::json!({"code": "two()"}),
                },
            ],
            1_006,
        );
        let before = snapshot(&f.db, &s.target.id);

        // Cut just after the failed tool result — history up to the failure,
        // nothing after.
        let result = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                at_part: Some(2),
                ..body(&rich.id)
            },
            ForkDeps::default(),
        )
        .unwrap();

        let own = with_db(&f.db, |d| d.messages_for(&result.session.id)).unwrap();
        assert_eq!(
            texts_of(&own),
            vec![
                "first ask",
                "first answer",
                "second ask",
                "second answer",
                "looking|<tool_call>|<tool_result>"
            ]
        );
        assert_eq!(own.last().unwrap().parts.len(), 3);
        assert!(!result.turn_started);
        assert_eq!(snapshot(&f.db, &s.target.id), before);

        // Out of range is a 400 naming the last usable cut point rather than a
        // truncation that silently keeps the whole message.
        let err = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                at_part: Some(5),
                ..body(&rich.id)
            },
            ForkDeps::default(),
        )
        .unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(err.to_string().contains("out of range"), "{err}");

        // The boundary itself is legal: the last part is a cut point like any
        // other.
        let boundary = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                at_part: Some(4),
                ..body(&rich.id)
            },
            ForkDeps::default(),
        )
        .unwrap();
        let boundary_own = with_db(&f.db, |d| d.messages_for(&boundary.session.id)).unwrap();
        assert_eq!(boundary_own.last().unwrap().parts.len(), 5);
    }

    #[tokio::test]
    async fn at_part_with_edited_text_appends_a_correction_after_the_cut_whatever_the_role() {
        let f = fixture("different approach it is");
        let s = scenario(&f);
        let rich = message(
            &f.db,
            &s.target.id,
            Role::Supervisor,
            vec![
                Part::Text {
                    text: "looking".into(),
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: RUN_STEPS.into(),
                    input: serde_json::json!({"code": "one()"}),
                },
            ],
            1_006,
        );
        let before = snapshot(&f.db, &s.target.id);
        let (start, handles) = real_turn();

        // The at-message is a SUPERVISOR message — legal here, because with
        // `atPart` the edit is a new message after the cut rather than a
        // replacement for that one.
        let result = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                at_part: Some(1),
                edited_text: Some("don't try it that way".into()),
                ..body(&rich.id)
            },
            ForkDeps { start: Some(start) },
        )
        .unwrap();
        assert!(result.turn_started);
        await_turn(&handles).await;

        assert_eq!(
            own_texts(&f, &result.session.id),
            vec![
                "first ask",
                "first answer",
                "second ask",
                "second answer",
                "looking|<tool_call>",
                "don't try it that way",
                "different approach it is"
            ]
        );

        // The cut stranded a `tool_call` with no `tool_result`, which is
        // exactly what `atPart` is for — and every provider rejects a thread
        // with the pair left open. `turn/replay` closes it with a synthetic
        // result rather than pretending the call succeeded, and that is what
        // makes a mid-message fork replayable at all.
        let calls = f.llm.calls();
        let synthetic = calls[0]
            .messages
            .iter()
            .flat_map(|m| m.content.iter())
            .find_map(|b| match b {
                LlmContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("the stranded tool_call must be paired with a synthetic result");
        assert!(
            synthetic.contains("interrupted"),
            "the synthetic result must say the call never returned, not that it succeeded"
        );
        assert_eq!(snapshot(&f.db, &s.target.id), before);
    }

    // ---- the two 400s -------------------------------------------------------

    #[tokio::test]
    async fn a_400_edited_text_may_not_replace_a_supervisor_turn() {
        let f = fixture("unused");
        let s = scenario(&f);
        let before = snapshot(&f.db, &s.target.id);
        let sessions_before = with_db(&f.db, |d| d.list_sessions()).unwrap().len();
        f.events.lock().unwrap().clear();

        let err = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                edited_text: Some("you said this".into()),
                ..body(&s.own[1].id)
            },
            ForkDeps::default(),
        )
        .unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(
            err.to_string()
                .contains("editedText can only replace a user message"),
            "{err}"
        );

        // Refused BEFORE the branch was opened: a check that ran after
        // `open_branch` would leave an empty half-seeded session in the user's
        // list on every bad request.
        assert_eq!(
            with_db(&f.db, |d| d.list_sessions()).unwrap().len(),
            sessions_before
        );
        assert!(f.events.lock().unwrap().is_empty());
        assert_eq!(snapshot(&f.db, &s.target.id), before);
    }

    #[tokio::test]
    async fn a_400_fork_point_in_ancestor_history_names_the_ancestor_to_fork_instead() {
        let f = fixture("unused");
        let s = scenario(&f);
        let ancestor_message = with_db(&f.db, |d| d.messages_for(&s.parent.id)).unwrap()[0].clone();
        // The user can SEE this message in the target's transcript — the
        // thread is ancestors ++ own — which is why the error has to name the
        // session that owns it.
        assert!(with_db(&f.db, |d| d.thread_for(&s.target.id))
            .unwrap()
            .iter()
            .any(|m| m.id == ancestor_message.id));
        f.events.lock().unwrap().clear();

        let err = fork(
            &f.ctx,
            &s.target.id,
            &body(&ancestor_message.id),
            ForkDeps::default(),
        )
        .unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(
            err.to_string()
                .contains(&format!("fork {} instead", s.parent.id)),
            "{err}"
        );
        assert!(f.events.lock().unwrap().is_empty());

        // An id from an unrelated session is refused too, and says something
        // different.
        let other = session(&f.db, base_session("unrelated"));
        let stranger = text_message(&f.db, &other.id, Role::User, "elsewhere", 1_010);
        let err = fork(
            &f.ctx,
            &s.target.id,
            &body(&stranger.id),
            ForkDeps::default(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("fork a session at one of its own"),
            "{err}"
        );

        // And an id that exists nowhere.
        let err = fork(
            &f.ctx,
            &s.target.id,
            &body("no-such-message"),
            ForkDeps::default(),
        )
        .unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(
            err.to_string()
                .contains("no message no-such-message exists"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_404_forking_a_session_that_does_not_exist() {
        let f = fixture("unused");
        let err = fork(
            &f.ctx,
            "no-such-session",
            &body("whatever"),
            ForkDeps::default(),
        )
        .unwrap_err();
        assert_eq!(err.status(), 404);
    }

    // ---- inheritance and titling --------------------------------------------

    #[tokio::test]
    async fn the_branch_inherits_the_sources_model_and_effort_pins() {
        let f = fixture("unused");
        let s = scenario(&f);
        with_db(&f.db, |d| {
            d.set_session_model(&s.target.id, Some("openai:gpt-5"))
        })
        .unwrap();
        with_db(&f.db, |d| d.set_session_effort(&s.target.id, Some("high"))).unwrap();
        f.events.lock().unwrap().clear();

        let result = fork(
            &f.ctx,
            &s.target.id,
            &body(&s.own[2].id),
            ForkDeps::default(),
        )
        .unwrap();

        // A resend is a controlled comparison — same history, one changed
        // message. Falling back to the global default would answer it on a
        // different model silently.
        assert_eq!(result.session.model.as_deref(), Some("openai:gpt-5"));
        assert_eq!(result.session.effort.as_deref(), Some("high"));
        assert_eq!(
            with_db(&f.db, |d| d.get_session(&result.session.id))
                .unwrap()
                .unwrap(),
            result.session
        );
        // Announced, so a client that only follows events sees the pins too.
        assert_eq!(
            f.events
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.r#type.as_str())
                .collect::<Vec<_>>(),
            vec![
                "session.created",
                "session.updated",
                "message.started",
                "message.started",
                "message.started"
            ]
        );
    }

    #[tokio::test]
    async fn a_fork_of_a_fork_does_not_compound_its_title_and_a_text_free_point_falls_back() {
        let f = fixture("unused");
        let s = scenario(&f);

        let first = fork(
            &f.ctx,
            &s.target.id,
            &body(&s.own[2].id),
            ForkDeps::default(),
        )
        .unwrap();
        assert_eq!(first.session.title, "fork · second ask");

        // Forking the fork: the excerpt is the branch point's text, not
        // "fork · fork · …".
        let first_own = with_db(&f.db, |d| d.messages_for(&first.session.id)).unwrap();
        let second = fork(
            &f.ctx,
            &first.session.id,
            &body(&first_own[0].id),
            ForkDeps::default(),
        )
        .unwrap();
        assert_eq!(second.session.title, "fork · first ask");

        // A fork point with no text at all falls back to the source's BASE
        // title.
        let tool_only = message(
            &f.db,
            &s.target.id,
            Role::Supervisor,
            vec![Part::ToolCall {
                id: "c9".into(),
                name: RUN_STEPS.into(),
                input: serde_json::json!({}),
            }],
            1_006,
        );
        let third = fork(
            &f.ctx,
            &s.target.id,
            &body(&tool_only.id),
            ForkDeps::default(),
        )
        .unwrap();
        assert_eq!(third.session.title, "fork · rename the router");
    }

    #[tokio::test]
    async fn a_long_fork_point_is_shortened_at_a_word_not_cut_mid_word() {
        let f = fixture("unused");
        let s = scenario(&f);
        // The live title that prompted this ended `…implements a binary se`.
        let long = text_message(
            &f.db,
            &s.target.id,
            Role::User,
            "Create a Python file that implements a binary search tree with insert and lookup.",
            1_006,
        );
        let forked = fork(&f.ctx, &s.target.id, &body(&long.id), ForkDeps::default()).unwrap();
        assert_eq!(
            forked.session.title,
            "fork · Create a Python file that implements a binary…"
        );

        // One unbroken 48-character word has no boundary to back up to, and is
        // better shown truncated than reduced to nothing.
        let nospace = text_message(&f.db, &s.target.id, Role::User, &"A".repeat(60), 1_007);
        let forked2 = fork(
            &f.ctx,
            &s.target.id,
            &body(&nospace.id),
            ForkDeps::default(),
        )
        .unwrap();
        assert_eq!(forked2.session.title, format!("fork · {}…", "A".repeat(48)));
    }

    #[tokio::test]
    async fn forking_the_first_message_produces_an_empty_but_real_branch() {
        let f = fixture("unused");
        let s = scenario(&f);

        let exclusive = fork(
            &f.ctx,
            &s.target.id,
            &ForkBody {
                exclusive: Some(true),
                ..body(&s.own[0].id)
            },
            ForkDeps::default(),
        )
        .unwrap();
        assert!(with_db(&f.db, |d| d.messages_for(&exclusive.session.id))
            .unwrap()
            .is_empty());
        // The ancestors are still inherited: an empty branch is not an empty
        // thread.
        assert_eq!(
            texts_of(&with_db(&f.db, |d| d.thread_for(&exclusive.session.id)).unwrap()),
            vec!["ancestor question", "ancestor answer"]
        );
    }

    #[test]
    fn excerpt_strips_a_trailing_punctuation_mark_at_the_cut() {
        let m = Message {
            id: "m".into(),
            session_id: "s".into(),
            role: Role::User,
            parts: vec![Part::Text {
                text: "Fix the parser, then the lexer, then the printer, then done".into(),
            }],
            pending: false,
            created_at: 0,
        };
        let e = excerpt_of(&m);
        assert!(e.ends_with('…'));
        assert!(!e.trim_end_matches('…').ends_with([',', ';', ':', '.']));
    }
}
