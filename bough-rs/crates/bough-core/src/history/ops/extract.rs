//! Extract (port of `src/history/extract.ts`) — copy hand-picked messages of a
//! session's VISIBLE thread into a fresh ROOT conversation.
//!
//! THE INVARIANT THIS HOLDS: **extract is the one selection op that is not
//! bounded by the session's own messages.** Fork and compaction reconstruct a
//! thread through parent-chain math — they branch a SIBLING at
//! `target.parent_id` and let `thread_for` re-supply the shared ancestors — so a
//! pick reaching into ancestor history is a 400 there, because those rows
//! belong to a different session and the branch cannot cut them out. Extract
//! has no such math to satisfy: the new session is a ROOT with `parent_id =
//! None`, inheriting nothing, and every message it will ever have is a copy
//! this operation writes. So the picks are resolved against `thread_for` —
//! ancestors root→parent, then own — and any message the user can SEE in the
//! transcript is fair game. That is the whole point of the operation, and it is
//! the thing fork cannot do.
//!
//! SECOND: **only the picked messages carry over, in THREAD ORDER.** The client
//! sends a selection, not a sequence — a user shift-clicking upward would
//! otherwise seed the new root with its turns reversed.
//!
//! WHAT THE NEW ROOT INHERITS: the source's workspace verbatim (the extracted
//! conversation is about the same code and continues in the SAME checkout,
//! edited in place), with `origin_dir`, `base` (the sha the Changes rail
//! measures from) and the model/effort pins.
//!
//! WHAT IT IS NOT: a move. The source keeps every one of its turns, untouched —
//! which is why the test asserts it is JSON-identical afterwards.

use crate::errors::{BoughError, ErrorKind};
use crate::schema::parts::{Message, Session, SessionKind};
use crate::schema::requests::{ExtractBody, PartPick};
use crate::types::{AppCtx, Db};

use super::seed::{
    base_title, inherit_pins, open_branch, resolve_picks, with_db, BranchCtx, BranchSpec,
};

#[derive(Debug)]
pub struct ExtractResult {
    /// The new root, as storage kept it (pins included).
    pub session: Session,
    /// Its messages, in the order they were seeded — thread order, not pick order.
    pub messages: Vec<Message>,
}

fn extract_err(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Extract, message)
}

/// Copy the picked messages of `session_id`'s thread into a new root
/// conversation.
///
/// 404 for an unknown session, 400 for a pick that is not a message of the
/// visible thread or a part index out of range.
///
/// Every validation runs BEFORE `open_branch`, and that ordering is
/// load-bearing: the seeder publishes `session.created` the moment it opens, so
/// a check that ran afterwards would leave an empty half-seeded root in the
/// user's session list every time a client sent a bad pick.
pub fn extract(
    ctx: &AppCtx,
    session_id: &str,
    args: &ExtractBody,
) -> Result<ExtractResult, BoughError> {
    let source = with_db(&ctx.db, |d| d.get_session(session_id))?
        .ok_or_else(|| extract_err(404, format!("session {session_id} not found")))?;

    // THE VISIBLE thread — ancestors root→parent, then own. Not `messages_for`:
    // the whole difference between this operation and fork is that an inherited
    // turn is pickable.
    let thread = with_db(&ctx.db, |d| d.thread_for(session_id))?;
    if thread.is_empty() {
        return Err(extract_err(
            400,
            format!("session {session_id} has an empty thread — there is nothing to extract"),
        ));
    }
    with_db(&ctx.db, |d| {
        assert_thread_messages(d, &source, &args.picks, &thread)
    })?;
    let picked = resolve_picks(&thread, &args.picks, |m| extract_err(400, m))?;

    let runtime = with_db(&ctx.db, |d| d.get_session_runtime(session_id))?;
    let branch_ctx = BranchCtx::from(ctx);
    let seeder = open_branch(
        branch_ctx.clone(),
        BranchSpec {
            // A ROOT. Not a sibling and not a child: the new conversation
            // inherits no thread, which is precisely what lets it carry an
            // ancestor's turns without carrying the ancestor.
            parent_id: None,
            title: format!("extract · {}", base_title(&source.title)),
            kind: Some(SessionKind::Root),
            // A root sharing the source's checkout must share the sha its
            // change set is measured from, or the Changes rail shows nothing
            // for work that is plainly in the tree.
            workspace: runtime.workspace.clone(),
            base: runtime.base.clone(),
            origin_dir: source.origin_dir.clone(),
            // Lineage for the tree: the session picked FROM, and the last
            // picked message.
            origin_id: Some(source.id.clone()),
            origin_message_id: Some(thread[picked[picked.len() - 1].idx].id.clone()),
        },
    )?;

    let mut messages = Vec::with_capacity(picked.len());
    for p in &picked {
        messages.push(seeder.copy(&p.view)?);
    }
    let session = inherit_pins(&branch_ctx, &source, seeder.session.clone())?;
    Ok(ExtractResult { session, messages })
}

/// Reject a pick that is not a message of the visible thread, in terms of the
/// move that works.
///
/// The distinction that matters here is the mirror image of fork's: fork's
/// interesting rejection is a message the user CAN see (an ancestor's) which
/// fork cannot use, and extract accepts exactly those. What extract rejects is a
/// message from somewhere else in the tree entirely — a sibling fork, a
/// subagent — and the move that works is to run the extract from a session
/// whose thread contains it.
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
            return Err(extract_err(
                400,
                format!("no message {} exists", p.message_id),
            ));
        };
        return Err(extract_err(
            400,
            format!(
                "message {} belongs to session {}, which is not in {}'s thread — extract can \
                 copy any message the session can SEE (its own turns and its ancestors'), so \
                 run the extract from {} or from a session that inherits it",
                p.message_id, foreign.session_id, source.id, foreign.session_id
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (port of `src/history/extract.test.ts`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::ops::fork::{fork, ForkDeps};
    use crate::history::ops::testkit::{
        message, scripted_ctx, session_with, text, texts_of, Fixture, SessionOver,
    };
    use crate::schema::parts::{Part, Role};
    use crate::schema::requests::ForkBody;
    use serde_json::json;

    fn snapshot(f: &Fixture, session_id: &str) -> String {
        let session = with_db(&f.ctx.db, |d| d.get_session(session_id)).unwrap();
        let messages = with_db(&f.ctx.db, |d| d.messages_for(session_id)).unwrap();
        serde_json::to_string(&json!({ "session": session, "messages": messages })).unwrap()
    }

    fn session_count(f: &Fixture) -> usize {
        with_db(&f.ctx.db, |d| d.list_sessions()).unwrap().len()
    }

    struct Lineage {
        parent: Session,
        child: Session,
        parent_messages: Vec<Message>,
        child_messages: Vec<Message>,
    }

    /// A parent session and a child whose thread inherits it — the shape that
    /// makes the ancestor claim meaningful. The child is a `fork` so the
    /// comparison against `fork()` is on the operation a user would run.
    fn lineage(f: &Fixture) -> Lineage {
        let parent = session_with(
            f,
            SessionOver {
                title: "the original work".into(),
                workspace: Some("/tmp/checkout".into()),
                origin_dir: Some("/tmp/checkout".into()),
                base: Some("abc123".into()),
                ..Default::default()
            },
        );
        let parent_messages = vec![
            text(
                f,
                &parent.id,
                Role::User,
                "how does the parser handle comments?",
            ),
            text(
                f,
                &parent.id,
                Role::Supervisor,
                "it skips them in the balanced-brace scan",
            ),
        ];
        let child = session_with(
            f,
            SessionOver {
                title: "fork · the original work".into(),
                kind: SessionKind::Fork,
                parent_id: Some(parent.id.clone()),
                workspace: Some("/tmp/checkout".into()),
                origin_dir: Some("/tmp/checkout".into()),
                base: Some("abc123".into()),
                ..Default::default()
            },
        );
        let child_messages = vec![
            text(f, &child.id, Role::User, "now add nested template literals"),
            text(
                f,
                &child.id,
                Role::Supervisor,
                "done — meta.ts handles them",
            ),
        ];
        Lineage {
            parent,
            child,
            parent_messages,
            child_messages,
        }
    }

    fn whole(ids: &[&Message]) -> Vec<PartPick> {
        ids.iter()
            .map(|m| PartPick {
                message_id: m.id.clone(),
                parts: None,
            })
            .collect()
    }

    // ---- AC 1: an ancestor message, which fork cannot touch -----------------

    #[test]
    fn extract_copies_an_ancestors_message_the_thing_fork_refuses() {
        let f = scripted_ctx();
        let l = lineage(&f);
        let ancestor = &l.parent_messages[1];

        // The message IS in the child's visible thread: that is the whole
        // premise.
        assert!(with_db(&f.ctx.db, |d| d.thread_for(&l.child.id))
            .unwrap()
            .iter()
            .any(|m| m.id == ancestor.id));

        // Fork refuses it, naming the ancestor.
        let refused = fork(
            &f.ctx,
            &l.child.id,
            &ForkBody {
                at_message_id: ancestor.id.clone(),
                edited_text: None,
                at_part: None,
                exclusive: None,
                summarize_abandoned: None,
            },
            ForkDeps::default(),
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("belongs to ancestor session"),
            "{refused}"
        );

        // Extract takes it, together with one of the child's own — a selection
        // spanning the inheritance boundary.
        let out = extract(
            &f.ctx,
            &l.child.id,
            &ExtractBody {
                picks: whole(&[ancestor, &l.child_messages[1]]),
            },
        )
        .unwrap();

        assert_eq!(out.session.kind, SessionKind::Root);
        // A ROOT: it inherits nothing, which is what lets it carry an
        // ancestor's turn without carrying the ancestor.
        assert_eq!(out.session.parent_id, None);
        assert_eq!(
            texts_of(&out.messages),
            vec![
                "it skips them in the balanced-brace scan",
                "done — meta.ts handles them"
            ]
        );
        // The new session's THREAD is exactly its own messages.
        assert_eq!(
            with_db(&f.ctx.db, |d| d.thread_for(&out.session.id))
                .unwrap()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            out.messages
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>()
        );

        // Copies, not moves: fresh ids, and the originals are still where they
        // were.
        assert_ne!(out.messages[0].id, ancestor.id);
        assert_eq!(
            with_db(&f.ctx.db, |d| d.get_message(&ancestor.id))
                .unwrap()
                .unwrap()
                .session_id,
            l.parent.id
        );
        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&l.parent.id))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&l.child.id))
                .unwrap()
                .len(),
            2
        );

        // Lineage points at the session extracted FROM and the last picked
        // message.
        assert_eq!(out.session.origin_id.as_deref(), Some(l.child.id.as_str()));
        assert_eq!(
            out.session.origin_message_id.as_deref(),
            Some(l.child_messages[1].id.as_str())
        );
    }

    #[test]
    fn the_extracted_root_keeps_the_sources_workspace_base_origin_dir_and_pins() {
        let f = scripted_ctx();
        let l = lineage(&f);
        with_db(&f.ctx.db, |d| {
            d.set_session_model(&l.child.id, Some("openai:gpt-5"))
        })
        .unwrap();
        with_db(&f.ctx.db, |d| {
            d.set_session_effort(&l.child.id, Some("high"))
        })
        .unwrap();

        let out = extract(
            &f.ctx,
            &l.child.id,
            &ExtractBody {
                picks: whole(&[&l.child_messages[0]]),
            },
        )
        .unwrap();

        // The same checkout, worked in place — with the sha its change set is
        // measured from.
        assert_eq!(out.session.workspace.as_deref(), Some("/tmp/checkout"));
        assert_eq!(out.session.base.as_deref(), Some("abc123"));
        assert_eq!(out.session.origin_dir.as_deref(), Some("/tmp/checkout"));
        // A model id is a provider routing decision: the extracted conversation
        // must not silently move to another vendor's default.
        assert_eq!(out.session.model.as_deref(), Some("openai:gpt-5"));
        assert_eq!(out.session.effort.as_deref(), Some("high"));
        // Read back, not just echoed.
        assert_eq!(
            with_db(&f.ctx.db, |d| d.get_session(&out.session.id))
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("openai:gpt-5")
        );
        // Titled off the BASE title: extracting a fork must not compound into
        // "extract · fork · X".
        assert_eq!(out.session.title, "extract · the original work");
    }

    // ---- AC 2: part-level picks ---------------------------------------------

    #[test]
    fn a_part_level_pick_copies_a_turns_prose_without_its_tool_calls() {
        let f = scripted_ctx();
        let source = session_with(&f, SessionOver::default());
        text(&f, &source.id, Role::User, "find the retry bound");
        let turn = message(
            &f,
            &source.id,
            Role::Supervisor,
            vec![
                Part::Reasoning {
                    text: "check the runner first".into(),
                    meta: None,
                    model: None,
                },
                Part::Text {
                    text: "Retries are bounded at 3 in turn/runner.ts.".into(),
                },
                Part::ToolCall {
                    id: "t1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "await bash('rg retry')" }),
                },
                Part::ToolResult {
                    call_id: "t1".into(),
                    output: json!("runner.ts:148: MAX_RETRIES = 3"),
                    is_error: false,
                    interrupted: None,
                },
                Part::Text {
                    text: "An exhausted retry surfaces as a turn error.".into(),
                },
            ],
        );

        let out = extract(
            &f.ctx,
            &source.id,
            // The two prose parts only — indexes 1 and 4.
            &ExtractBody {
                picks: vec![PartPick {
                    message_id: turn.id.clone(),
                    parts: Some(vec![1, 4]),
                }],
            },
        )
        .unwrap();

        assert_eq!(out.messages.len(), 1);
        let copied = &out.messages[0];
        assert_eq!(copied.role, Role::Supervisor);
        assert_eq!(
            copied.parts,
            vec![
                Part::Text {
                    text: "Retries are bounded at 3 in turn/runner.ts.".into()
                },
                Part::Text {
                    text: "An exhausted retry surfaces as a turn error.".into()
                },
            ]
        );
        // What storage kept, not just what was returned.
        assert_eq!(
            with_db(&f.ctx.db, |d| d.messages_for(&out.session.id)).unwrap()[0].parts,
            copied.parts
        );
        // The original turn still has all five of its parts.
        assert_eq!(
            with_db(&f.ctx.db, |d| d.get_message(&turn.id))
                .unwrap()
                .unwrap()
                .parts
                .len(),
            5
        );
    }

    // ---- selection semantics ------------------------------------------------

    #[test]
    fn picks_are_copied_in_thread_order_whatever_order_they_were_selected_in() {
        let f = scripted_ctx();
        let l = lineage(&f);

        // Sent bottom-up and interleaved across the inheritance boundary.
        let out = extract(
            &f.ctx,
            &l.child.id,
            &ExtractBody {
                picks: whole(&[
                    &l.child_messages[1],
                    &l.parent_messages[0],
                    &l.child_messages[0],
                ]),
            },
        )
        .unwrap();

        assert_eq!(
            texts_of(&out.messages),
            vec![
                "how does the parser handle comments?",
                "now add nested template literals",
                "done — meta.ts handles them"
            ]
        );
    }

    #[test]
    fn a_whole_message_pick_wins_over_a_partial_one_for_the_same_message() {
        let f = scripted_ctx();
        let source = session_with(&f, SessionOver::default());
        let turn = message(
            &f,
            &source.id,
            Role::Supervisor,
            vec![
                Part::Text { text: "one".into() },
                Part::Text { text: "two".into() },
            ],
        );

        let out = extract(
            &f.ctx,
            &source.id,
            &ExtractBody {
                picks: vec![
                    PartPick {
                        message_id: turn.id.clone(),
                        parts: Some(vec![0]),
                    },
                    PartPick {
                        message_id: turn.id.clone(),
                        parts: None,
                    },
                ],
            },
        )
        .unwrap();

        assert_eq!(out.messages.len(), 1);
        assert_eq!(texts_of(&out.messages), vec!["one | two"]);
    }

    // ---- the source is untouched --------------------------------------------

    #[test]
    fn extract_leaves_the_source_and_its_ancestor_byte_identical() {
        let f = scripted_ctx();
        let l = lineage(&f);
        let before = [snapshot(&f, &l.parent.id), snapshot(&f, &l.child.id)];

        extract(
            &f.ctx,
            &l.child.id,
            &ExtractBody {
                picks: vec![
                    PartPick {
                        message_id: l.parent_messages[0].id.clone(),
                        parts: None,
                    },
                    PartPick {
                        message_id: l.parent_messages[1].id.clone(),
                        parts: Some(vec![0]),
                    },
                    PartPick {
                        message_id: l.child_messages[0].id.clone(),
                        parts: None,
                    },
                ],
            },
        )
        .unwrap();

        assert_eq!(
            [snapshot(&f, &l.parent.id), snapshot(&f, &l.child.id)],
            before
        );
    }

    // ---- events -------------------------------------------------------------

    #[test]
    fn the_new_root_is_announced_before_the_copies_that_go_into_it() {
        let f = scripted_ctx();
        let l = lineage(&f);
        f.clear_events();

        let out = extract(
            &f.ctx,
            &l.child.id,
            &ExtractBody {
                picks: whole(&[&l.child_messages[0], &l.child_messages[1]]),
            },
        )
        .unwrap();

        assert_eq!(
            f.event_types(),
            vec!["session.created", "message.started", "message.started"]
        );
        // A `message.started` for a session the client has never heard of is a
        // message it has nowhere to put — hence the order.
        let events = f.events.lock().unwrap();
        assert!(events
            .iter()
            .all(|e| e.session_id.as_deref() == Some(out.session.id.as_str())));
    }

    // ---- refusals -----------------------------------------------------------

    #[test]
    fn an_unknown_session_is_a_404_and_writes_nothing() {
        let f = scripted_ctx();
        let l = lineage(&f);
        let before = session_count(&f);

        let err = extract(
            &f.ctx,
            "no-such-session",
            &ExtractBody {
                picks: whole(&[&l.child_messages[0]]),
            },
        )
        .unwrap_err();
        assert_eq!(err.status(), 404);
        assert_eq!(session_count(&f), before);
    }

    #[test]
    fn a_message_from_outside_the_thread_is_a_400_naming_where_it_lives() {
        let f = scripted_ctx();
        let l = lineage(&f);
        // A sibling branch: real message, real session, not in this session's
        // thread.
        let other = session_with(
            &f,
            SessionOver {
                title: "unrelated".into(),
                ..Default::default()
            },
        );
        let stray = text(&f, &other.id, Role::User, "different work entirely");
        let before = session_count(&f);

        let err = extract(
            &f.ctx,
            &l.child.id,
            &ExtractBody {
                picks: whole(&[&stray]),
            },
        )
        .unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(err.to_string().contains(&other.id));
        // Validation runs before the branch opens, so a bad pick leaves no
        // empty root behind in the user's session list.
        assert_eq!(session_count(&f), before);
    }

    #[test]
    fn a_nonexistent_message_id_and_a_part_index_out_of_range_are_both_400() {
        let f = scripted_ctx();
        let source = session_with(&f, SessionOver::default());
        let turn = text(&f, &source.id, Role::User, "only one part here");

        let missing = extract(
            &f.ctx,
            &source.id,
            &ExtractBody {
                picks: vec![PartPick {
                    message_id: "nope".into(),
                    parts: None,
                }],
            },
        )
        .unwrap_err();
        assert!(
            missing.to_string().contains("no message nope exists"),
            "{missing}"
        );

        let range = extract(
            &f.ctx,
            &source.id,
            &ExtractBody {
                picks: vec![PartPick {
                    message_id: turn.id.clone(),
                    parts: Some(vec![3]),
                }],
            },
        )
        .unwrap_err();
        assert!(
            range.to_string().contains("part index out of range"),
            "{range}"
        );
    }

    #[test]
    fn a_session_with_an_empty_thread_has_nothing_to_extract() {
        let f = scripted_ctx();
        let empty = session_with(
            &f,
            SessionOver {
                title: "brand new".into(),
                ..Default::default()
            },
        );
        let err = extract(
            &f.ctx,
            &empty.id,
            &ExtractBody {
                picks: vec![PartPick {
                    message_id: "anything".into(),
                    parts: None,
                }],
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty thread"), "{err}");
    }
}
