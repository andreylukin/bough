//! Move-into (port of `src/history/move.ts`) — append copies of hand-picked
//! messages from one session's thread onto an EXISTING session. Extract's
//! sibling: extract lands the picks in a new root, move-into lands them at the
//! end of a branch you already have.
//!
//! THE INVARIANT THIS HOLDS: **"move" is a lie the name tells and the
//! implementation never does. It is a COPY.** bough never rewrites history in
//! place, so nothing is deleted from the source, nothing is re-parented, and
//! the source's rows come out of this byte-identical. The target gains new
//! messages with fresh ids, announced one by one like any seeded branch, and the
//! user ends up with the picks in both places. That is the honest reading of the
//! operation, and the reason it is safe to offer on a transcript someone is
//! still working in.
//!
//! The picks are resolved against the SOURCE'S VISIBLE THREAD (`thread_for` —
//! ancestors root→parent, then own), the same reach extract has and for the
//! same reason: this operation writes copies and reconstructs nothing through
//! parent-chain math, so an inherited turn is as copyable as an own one.
//!
//! THREE THINGS IT REFUSES, all for the same reason — the copies land at the END
//! of the target's own messages, and an append is only sound when nothing else
//! is deciding what belongs there:
//!
//!   - **Into itself.** A session cannot append its own turns to its own tail.
//!   - **Into a session running a turn** (409). One turn per session: a live
//!     turn is appending to that same tail, and interleaving seeded copies into
//!     it produces a transcript whose order neither party chose — and which the
//!     turn will then replay to the model as though it had been there all along.
//!   - **Into an ANCESTOR of the source** (400). The target's messages come
//!     BEFORE the source's own in `thread_for`, so appending to an ancestor
//!     silently rewrites the middle of the source's visible thread. The source's
//!     rows would still be untouched and the invariant above still technically
//!     true, which is precisely what makes this the dangerous case rather than
//!     the obvious one.

use crate::errors::{BoughError, ErrorKind};
use crate::schema::parts::{Message, Session};
use crate::schema::requests::{MoveBody, PartPick};
use crate::types::{AppCtx, Db};

use super::seed::{resolve_picks, with_db, BranchCtx, Seeder};

#[derive(Debug)]
pub struct MoveResult {
    /// The target, unchanged as a row — only its message list grew.
    pub session: Session,
    /// The copies appended, in the order they were seeded (thread order).
    pub messages: Vec<Message>,
}

fn move_err(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Move, message)
}

/// Append copies of `args.source_id`'s picked thread messages to `target_id`.
///
/// 404 for an unknown target or source, 400 for a target that cannot receive
/// this source's picks or a pick outside the source's thread, 409 for a target
/// that is running a turn.
///
/// Every check runs before the first `copy`, so a refused move writes nothing at
/// all rather than leaving half a selection appended with no way to finish it.
pub fn move_into(ctx: &AppCtx, target_id: &str, args: &MoveBody) -> Result<MoveResult, BoughError> {
    let target = with_db(&ctx.db, |d| d.get_session(target_id))?
        .ok_or_else(|| move_err(404, format!("target session {target_id} not found")))?;
    let source = with_db(&ctx.db, |d| d.get_session(&args.source_id))?
        .ok_or_else(|| move_err(404, format!("source session {} not found", args.source_id)))?;

    if args.source_id == target_id {
        return Err(move_err(
            400,
            format!(
                "source and target are both {target_id} — move-into COPIES the picks onto the \
                 end of the target, so this would append the session's own turns to itself. \
                 Pick a different target, or extract the selection into a new root instead."
            ),
        ));
    }
    // The ancestor case: `thread_for(source)` is ancestors first, so an append
    // here lands in the MIDDLE of what the source displays and replays.
    let chain = with_db(&ctx.db, |d| d.ancestor_chain(&args.source_id))?;
    if chain.iter().any(|s| s.id == target_id) {
        return Err(move_err(
            400,
            format!(
                "session {target_id} is an ancestor of {source_id}: {source_id} inherits its \
                 messages, so appending there would insert turns into the middle of the \
                 source's own thread. Extract the selection into a new root instead.",
                source_id = args.source_id
            ),
        ));
    }
    // One turn per session — a running turn owns the tail this appends to.
    if with_db(&ctx.db, |d| d.busy_session_ids())?.contains(target_id) {
        return Err(move_err(
            409,
            format!(
                "session {target_id} is running a turn — move-into appends to the end of its \
                 transcript and would interleave with what the turn is writing there. Wait for \
                 the turn to finish (or interrupt it) and send this again."
            ),
        ));
    }

    // THE SOURCE'S VISIBLE thread, ancestors included — same reach as extract.
    let thread = with_db(&ctx.db, |d| d.thread_for(&args.source_id))?;
    if thread.is_empty() {
        return Err(move_err(
            400,
            format!(
                "session {} has an empty thread — there is nothing to copy",
                args.source_id
            ),
        ));
    }
    with_db(&ctx.db, |d| {
        assert_thread_messages(d, &source, &args.picks, &thread)
    })?;
    let picked = resolve_picks(&thread, &args.picks, |m| move_err(400, m))?;

    // Appended in thread order, each a fresh message announced over the bus —
    // the same `Seeder` a branch uses, constructed directly because the session
    // already exists. Timestamps come from the real clock, never an advanced
    // one, so a turn started immediately afterwards still sorts after these.
    let seeder = Seeder::new(BranchCtx::from(ctx), target.clone());
    let mut messages = Vec::with_capacity(picked.len());
    for p in &picked {
        messages.push(seeder.copy(&p.view)?);
    }
    Ok(MoveResult {
        session: target,
        messages,
    })
}

/// Reject a pick that is not a message of the source's visible thread.
///
/// Naming where the message actually lives is the difference between an error
/// the user can act on and one that reads as a client bug — the id is real and
/// they are looking at it somewhere.
fn assert_thread_messages(
    db: &dyn Db,
    source: &Session,
    picks: &[PartPick],
    thread: &[Message],
) -> Result<(), BoughError> {
    for p in picks {
        if thread.iter().any(|m| m.id == p.message_id) {
            continue;
        }
        let Some(foreign) = db.get_message(&p.message_id)? else {
            return Err(move_err(400, format!("no message {} exists", p.message_id)));
        };
        return Err(move_err(
            400,
            format!(
                "message {} belongs to session {}, which is not in source {}'s thread — pass {} \
                 as sourceId, or pick messages the source can see (its own turns and its \
                 ancestors')",
                p.message_id, foreign.session_id, source.id, foreign.session_id
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (port of `src/history/move.test.ts`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::ops::testkit::{
        message, scripted_ctx, session_with, text, texts_of, Fixture, SessionOver,
    };
    use crate::schema::parts::{Part, Role, SessionKind};
    use serde_json::json;

    fn snapshot(f: &Fixture, session_id: &str) -> String {
        let session = with_db(&f.ctx.db, |d| d.get_session(session_id)).unwrap();
        let messages = with_db(&f.ctx.db, |d| d.messages_for(session_id)).unwrap();
        serde_json::to_string(&json!({ "session": session, "messages": messages })).unwrap()
    }

    struct Pair {
        source: Session,
        target: Session,
        source_messages: Vec<Message>,
        target_messages: Vec<Message>,
    }

    /// Two unrelated roots: one to copy from, one to append onto.
    fn pair(f: &Fixture) -> Pair {
        let source = session_with(
            f,
            SessionOver {
                title: "the investigation".into(),
                ..Default::default()
            },
        );
        let source_messages = vec![
            text(f, &source.id, Role::User, "why does the ticker fire twice?"),
            text(
                f,
                &source.id,
                Role::Supervisor,
                "catch-up advances from now, not the stale value",
            ),
        ];
        let target = session_with(
            f,
            SessionOver {
                title: "the fix".into(),
                ..Default::default()
            },
        );
        let target_messages = vec![text(f, &target.id, Role::User, "let's write it up")];
        Pair {
            source,
            target,
            source_messages,
            target_messages,
        }
    }

    fn whole(messages: &[Message]) -> Vec<PartPick> {
        messages
            .iter()
            .map(|m| PartPick {
                message_id: m.id.clone(),
                parts: None,
            })
            .collect()
    }

    fn own_texts(f: &Fixture, session_id: &str) -> Vec<String> {
        texts_of(&with_db(&f.ctx.db, |d| d.messages_for(session_id)).unwrap())
    }

    // ---- the copy -----------------------------------------------------------

    #[test]
    fn move_into_appends_copies_at_the_end_of_the_targets_own_messages() {
        let f = scripted_ctx();
        let p = pair(&f);

        let out = move_into(
            &f.ctx,
            &p.target.id,
            &MoveBody {
                source_id: p.source.id.clone(),
                picks: whole(&p.source_messages),
            },
        )
        .unwrap();

        assert_eq!(out.session.id, p.target.id);
        assert_eq!(
            own_texts(&f, &p.target.id),
            vec![
                "let's write it up",
                "why does the ticker fire twice?",
                "catch-up advances from now, not the stale value"
            ]
        );
        // Fresh ids and fresh session — copies, not the originals re-parented.
        assert!(out.messages.iter().all(|m| m.session_id == p.target.id));
        assert!(!out
            .messages
            .iter()
            .any(|m| p.source_messages.iter().any(|s| s.id == m.id)));
        // Roles ride along: a copied supervisor turn stays a supervisor turn.
        assert_eq!(
            out.messages.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![Role::User, Role::Supervisor]
        );
        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&p.target.id))
                .unwrap()
                .len(),
            p.target_messages.len() + 2
        );
    }

    #[test]
    fn the_source_is_byte_identical_afterwards_this_is_a_copy_not_a_move() {
        let f = scripted_ctx();
        let p = pair(&f);
        let before = snapshot(&f, &p.source.id);

        move_into(
            &f.ctx,
            &p.target.id,
            &MoveBody {
                source_id: p.source.id.clone(),
                picks: whole(&p.source_messages),
            },
        )
        .unwrap();

        assert_eq!(snapshot(&f, &p.source.id), before);
    }

    #[test]
    fn picks_reach_into_the_sources_ancestors_and_land_in_thread_order() {
        let f = scripted_ctx();
        let parent = session_with(
            &f,
            SessionOver {
                title: "the origin".into(),
                ..Default::default()
            },
        );
        let inherited = text(
            &f,
            &parent.id,
            Role::Supervisor,
            "the parser skips comments",
        );
        let source = session_with(
            &f,
            SessionOver {
                title: "fork · the origin".into(),
                kind: SessionKind::Fork,
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        );
        let own = text(
            &f,
            &source.id,
            Role::Supervisor,
            "and now template literals",
        );
        let target = session_with(
            &f,
            SessionOver {
                title: "the writeup".into(),
                ..Default::default()
            },
        );

        let out = move_into(
            &f.ctx,
            &target.id,
            &MoveBody {
                source_id: source.id.clone(),
                // Sent bottom-up: the selection is not a sequence.
                picks: whole(&[own.clone(), inherited.clone()]),
            },
        )
        .unwrap();

        assert_eq!(
            texts_of(&out.messages),
            vec!["the parser skips comments", "and now template literals"]
        );
        // The ancestor kept its message; nothing was moved out of it.
        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&parent.id))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_part_level_pick_copies_a_turns_prose_without_its_tool_calls() {
        let f = scripted_ctx();
        let p = pair(&f);
        let turn = message(
            &f,
            &p.source.id,
            Role::Supervisor,
            vec![
                Part::Text {
                    text: "The ticker advances next_run_at from now.".into(),
                },
                Part::ToolCall {
                    id: "t1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "await bash('rg tick')" }),
                },
                Part::ToolResult {
                    call_id: "t1".into(),
                    output: json!("schedules.ts:88"),
                    is_error: false,
                    interrupted: None,
                },
            ],
        );

        let out = move_into(
            &f.ctx,
            &p.target.id,
            &MoveBody {
                source_id: p.source.id.clone(),
                picks: vec![PartPick {
                    message_id: turn.id.clone(),
                    parts: Some(vec![0]),
                }],
            },
        )
        .unwrap();

        assert_eq!(
            out.messages[0].parts,
            vec![Part::Text {
                text: "The ticker advances next_run_at from now.".into()
            }]
        );
        assert_eq!(
            with_db(&f.ctx.db, |d| d.get_message(&turn.id))
                .unwrap()
                .unwrap()
                .parts
                .len(),
            3
        );
    }

    #[test]
    fn each_copy_is_announced_as_message_started_on_the_target() {
        let f = scripted_ctx();
        let p = pair(&f);
        f.clear_events();

        move_into(
            &f.ctx,
            &p.target.id,
            &MoveBody {
                source_id: p.source.id.clone(),
                picks: whole(&p.source_messages),
            },
        )
        .unwrap();

        // No session.created: move-into creates nothing.
        assert_eq!(f.event_types(), vec!["message.started", "message.started"]);
        let events = f.events.lock().unwrap();
        assert!(events
            .iter()
            .all(|e| e.session_id.as_deref() == Some(p.target.id.as_str())));
    }

    // ---- the refusals -------------------------------------------------------

    #[test]
    fn a_session_cannot_receive_its_own_turns() {
        let f = scripted_ctx();
        let p = pair(&f);

        let err = move_into(
            &f.ctx,
            &p.source.id,
            &MoveBody {
                source_id: p.source.id.clone(),
                picks: whole(&[p.source_messages[0].clone()]),
            },
        )
        .unwrap_err();

        assert_eq!(err.status(), 400);
        assert!(
            err.to_string().contains("source and target are both"),
            "{err}"
        );
        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&p.source.id))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn an_ancestor_of_the_source_is_refused_it_would_rewrite_the_sources_thread() {
        let f = scripted_ctx();
        let parent = session_with(
            &f,
            SessionOver {
                title: "the origin".into(),
                ..Default::default()
            },
        );
        text(&f, &parent.id, Role::User, "first");
        let child = session_with(
            &f,
            SessionOver {
                title: "fork · the origin".into(),
                kind: SessionKind::Fork,
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        );
        let own = text(&f, &child.id, Role::Supervisor, "second");
        let thread_before: Vec<String> = with_db(&f.ctx.db, |d| d.thread_for(&child.id))
            .unwrap()
            .iter()
            .map(|m| m.id.clone())
            .collect();

        let err = move_into(
            &f.ctx,
            &parent.id,
            &MoveBody {
                source_id: child.id.clone(),
                picks: whole(&[own]),
            },
        )
        .unwrap_err();

        assert_eq!(err.status(), 400);
        assert!(err.to_string().contains("is an ancestor of"), "{err}");
        // Nothing was written, so the source's visible thread is untouched.
        assert_eq!(
            with_db(&f.ctx.db, |d| d.thread_for(&child.id))
                .unwrap()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            thread_before
        );
    }

    #[test]
    fn a_target_running_a_turn_is_a_409_not_an_interleaved_transcript() {
        let f = scripted_ctx();
        let p = pair(&f);
        // One turn per session: a live turn owns the tail this would append to.
        with_db(&f.ctx.db, |d| {
            d.create_turn(crate::schema::parts::Turn {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: p.target.id.clone(),
                message_id: p.target_messages[0].id.clone(),
                status: crate::schema::parts::TurnStatus::Running,
                step: "streaming".into(),
                error: None,
                usage: None,
                created_at: 1_000,
                updated_at: 1_000,
            })
        })
        .unwrap();

        let err = move_into(
            &f.ctx,
            &p.target.id,
            &MoveBody {
                source_id: p.source.id.clone(),
                picks: whole(&[p.source_messages[0].clone()]),
            },
        )
        .unwrap_err();

        assert_eq!(err.status(), 409);
        assert!(err.to_string().contains("running a turn"), "{err}");
        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&p.target.id))
                .unwrap()
                .len(),
            p.target_messages.len()
        );
    }

    #[test]
    fn an_unknown_target_or_source_is_a_404_and_writes_nothing() {
        let f = scripted_ctx();
        let p = pair(&f);

        let no_target = move_into(
            &f.ctx,
            "no-such-target",
            &MoveBody {
                source_id: p.source.id.clone(),
                picks: whole(&[p.source_messages[0].clone()]),
            },
        )
        .unwrap_err();
        assert_eq!(no_target.status(), 404);

        let no_source = move_into(
            &f.ctx,
            &p.target.id,
            &MoveBody {
                source_id: "no-such-source".into(),
                picks: whole(&[p.source_messages[0].clone()]),
            },
        )
        .unwrap_err();
        assert_eq!(no_source.status(), 404);

        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&p.target.id))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_pick_outside_the_sources_thread_is_a_400_naming_where_it_lives() {
        let f = scripted_ctx();
        let p = pair(&f);
        let other = session_with(
            &f,
            SessionOver {
                title: "elsewhere".into(),
                ..Default::default()
            },
        );
        let stray = text(&f, &other.id, Role::User, "different work");

        let err = move_into(
            &f.ctx,
            &p.target.id,
            &MoveBody {
                source_id: p.source.id.clone(),
                picks: whole(&[stray]),
            },
        )
        .unwrap_err();

        assert_eq!(err.status(), 400);
        assert!(err.to_string().contains(&other.id), "{err}");
        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&p.target.id))
                .unwrap()
                .len(),
            1
        );
    }
}
