//! Handoff (port of `src/history/handoff.ts`) — a focused new thread instead of
//! another round of compaction.
//!
//! The user states a GOAL. An LLM reads the source session's whole visible
//! thread and writes the OPENING PROMPT for a fresh conversation: the goal
//! restated as the task, only the context that still matters for it (decisions
//! made, constraints, the state the work is in), and the relevant file paths.
//! Everything unrelated — the dead ends, the resolved back-and-forth, the 40KB
//! of tool output that produced one sentence — is dropped, because the new agent
//! will see nothing but this prompt.
//!
//! THE INVARIANT THIS HOLDS: **nothing is copied and nothing is mutated.**
//! Compaction seeds a branch with copies and one summary in place of a span; a
//! handoff seeds NO messages at all. The distilled context lives entirely in the
//! new session's `draft`, and the source session is not written to in any way.
//! That is what makes handoff safe to run speculatively against a conversation
//! the user is still in the middle of: worst case they get a root with a draft
//! they discard.
//!
//! WHY A DRAFT RATHER THAN A SEEDED USER MESSAGE. The draft is *prefilled
//! composer text*, not a turn: the UI puts it in the composer, the user reads
//! what the model decided to carry over, edits it, and sends. A seeded user
//! message would start the work on the model's own account of what mattered,
//! with no moment for the human to correct it — and the whole reason handoff
//! exists is that the human knows what the next thread is for. Posting the
//! message is what clears the draft, server-side, on the first post.
//!
//! ORDER OF OPERATIONS: draft FIRST, session second. The LLM call completes
//! before a single row is written, so a failed or empty draft leaves no empty
//! root behind — the same rule compaction follows for the same reason.

use crate::errors::{BoughError, ErrorKind};
use crate::llm::{complete_text, CompleteTextOpts};
use crate::schema::events::EventType;
use crate::schema::parts::{Session, SessionKind};
use crate::schema::requests::HandoffBody;
use crate::types::AppCtx;

use super::compact::{llm_for, model_for, render_span, CompactDeps};
use super::explore::{explore_span, ExploreCtx};
use super::seed::{
    base_title, event, inherit_pins, open_branch, to_value, with_db, BranchCtx, BranchSpec,
};

/// The drafter's brief.
///
/// Three paragraphs, and the second and third are hard-won:
///
/// - WITHOUT THE SECOND, THE DRAFT ASKS THE USER A QUESTION. Observed: a short
///   transcript plus a goal it had no context for produced "Once you provide
///   that, I can write a focused opening prompt for the new conversation." —
///   which lands verbatim in the user's composer as if it were the distilled
///   prompt, addressed to nobody. The transcript being thin is not a reason to
///   stop working; it is a reason for a shorter prompt.
/// - THE THIRD IS THE LIVE WORKSPACE. A draft written from the transcript alone
///   asserts whatever the conversation last claimed, and a conversation claims
///   things that were undone three turns later — which matters more here than
///   anywhere else, because the new root inherits NO thread: this draft is the
///   only context the next agent will ever have.
pub const SYSTEM: &str = concat!(
    "You are handing off work from one coding-agent conversation to a new, focused one. ",
    "Given the transcript and the user's goal for the new conversation, write the OPENING ",
    "PROMPT the user will send to start it. The new agent sees nothing but this prompt, so ",
    "make it self-contained: state the goal as the task; carry over only the context that ",
    "matters for it — decisions made, constraints, the current state of the work; list the ",
    "relevant file paths. Drop everything unrelated to the goal, including dead ends and ",
    "resolved back-and-forth. Write as direct instructions to the agent, in the user's ",
    "voice. Output only the prompt text.\n\n",
    "NEVER reply to the user and never ask for more information: you are writing text ",
    "the user will SEND, not a message to them. Do not describe what you are doing, do ",
    "not offer alternatives, and do not preface the prompt. If the transcript holds ",
    "little or nothing relevant to the goal, say so in one line and then state the goal ",
    "as the task — a short prompt is a correct answer, a request for input is not. State ",
    "it as an INSTRUCTION (\"fix the coupon stacking in src/cart.py\"), never as a request ",
    "for details: whoever reads this prompt is the one who will do the work.\n\n",
    "You may be given SCOUT NOTES: what a subagent found in the files this conversation ",
    "touched, read from the checkout as it stands now. Where the notes and the ",
    "transcript disagree about the state of the code, the notes are right — the ",
    "transcript records intentions, some of which were undone later — so carry the paths ",
    "and the state from the notes and the decisions and constraints from the transcript.",
);

const MAX_TOKENS: i64 = 8192;

fn handoff_err(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Handoff, message)
}

/// A goal, shortened to something that fits a tree row. Word-boundary, not
/// mid-word.
fn clip_title(goal: &str, max: usize) -> String {
    let one = goal.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    let cut: String = one.chars().take(max).collect();
    let head = match cut.rfind(' ') {
        Some(space) if space > max / 2 => cut[..space].to_string(),
        _ => cut,
    };
    format!("{}…", head.trim_end())
}

/// Draft the handoff prompt for `session_id` toward `args.goal`, open the new
/// root with the draft attached, and return it.
///
/// 404 for an unknown session, 400 for a thread with nothing in it to hand off,
/// 502 for a model that returned no text.
pub async fn handoff(
    ctx: &AppCtx,
    session_id: &str,
    args: &HandoffBody,
    deps: CompactDeps,
) -> Result<Session, BoughError> {
    let source = with_db(&ctx.db, |d| d.get_session(session_id))?
        .ok_or_else(|| handoff_err(404, format!("session {session_id} not found")))?;

    // The VISIBLE thread — ancestors root→parent, then own. A handoff distills
    // what the user has been looking at, which in a forked session is mostly
    // inherited.
    let thread = with_db(&ctx.db, |d| d.thread_for(session_id))?;
    if thread.is_empty() {
        return Err(handoff_err(
            400,
            format!(
                "session {session_id} has an empty thread — there is nothing to hand off. \
                 Start a new session directly instead."
            ),
        ));
    }

    let model = model_for(ctx, &source);
    let llm = llm_for(ctx, &model);
    // The scout runs BEFORE the draft, and its workspace is read here for the
    // same reason compaction reads it early: everything that can fail happens
    // before a row is written. No workspace means no checkout to scout, and the
    // transcript is all there is.
    let runtime = with_db(&ctx.db, |d| d.get_session_runtime(session_id))?;
    let notes: Option<String> = match runtime.workspace.clone().filter(|w| !w.is_empty()) {
        Some(workspace) => match deps.explore.clone() {
            Some(scout) => scout(thread.clone(), workspace).await,
            None => {
                explore_span(
                    &ExploreCtx {
                        session_id: session_id.to_string(),
                        workspace,
                        llm: None,
                        model: None,
                        registry: None,
                    },
                    &thread,
                )
                .await
            }
        },
        None => None,
    };

    // `render_span` is compaction's transcript renderer, reused rather than
    // reimplemented: a second renderer would drift the moment a part kind is
    // added, and it already clips oversized tool payloads so one 200KB result
    // cannot swallow the prompt.
    let mut prompt_parts = vec![render_span(&thread)];
    if let Some(notes) = notes.as_deref().filter(|n| !n.is_empty()) {
        prompt_parts.push(format!(
            "Scout notes — the files this conversation touched, as they are now:\n{notes}"
        ));
    }
    prompt_parts.push(format!("Goal for the new conversation: {}", args.goal));

    let draft = complete_text(
        &llm,
        CompleteTextOpts {
            model: model.clone(),
            system: SYSTEM.to_string(),
            max_tokens: MAX_TOKENS,
            prompt: prompt_parts.join("\n\n"),
        },
    )
    .await?
    .trim()
    .to_string();
    // An empty draft is not a handoff. The whole content of this operation is
    // the draft — seeding a root without one would hand the user an empty
    // composer and a session they did not ask for. Raised before anything is
    // written, so that session never exists.
    if draft.is_empty() {
        return Err(handoff_err(
            502,
            format!(
                "the model ({model}) returned no draft for a thread of {} message(s) — nothing \
                 was written; retry, or state the goal more concretely",
                thread.len()
            ),
        ));
    }

    let branch_ctx = BranchCtx::from(ctx);
    let seeder = open_branch(
        branch_ctx.clone(),
        BranchSpec {
            parent_id: None, // a ROOT: it inherits no thread, only the draft
            // The GOAL when the source has no title of its own. A conversation
            // whose auto-title never landed produced `handoff · ` — a prefix
            // with nothing after it, which every client then renders as
            // "(untitled)" for the rest of its life, because the row is only
            // ever retitled by a first message and this session's first message
            // is sitting unsent in the composer.
            title: format!("handoff · {}", {
                let base = base_title(&source.title);
                if base.is_empty() {
                    clip_title(&args.goal, 48)
                } else {
                    base
                }
            }),
            kind: Some(SessionKind::Root),
            // The same checkout, worked in place — with the sha its change set
            // is measured from.
            workspace: runtime.workspace.clone(),
            base: runtime.base.clone(),
            origin_dir: source.origin_dir.clone(),
            origin_id: Some(source.id.clone()), // lineage: handed off FROM…
            origin_message_id: Some(thread[thread.len() - 1].id.clone()), // …as of this message
        },
    )?;
    let branch = inherit_pins(&branch_ctx, &source, seeder.session.clone())?;

    with_db(&ctx.db, |d| d.set_session_draft(&branch.id, Some(&draft)))?;
    // Read back rather than patched onto the local value: the row is what a
    // later `GET /sessions/:id` will answer with, and the event and the fetch
    // must not be able to disagree. `open_branch` already published
    // `session.created` (pre-draft), so this announces the draft for live tree
    // views that would otherwise need a refetch.
    let created = with_db(&ctx.db, |d| d.get_session(&branch.id))?.unwrap_or(Session {
        draft: Some(draft),
        ..branch
    });
    ctx.bus.publish(event(
        EventType::SessionUpdated,
        &created.id,
        to_value(&created),
    ));
    Ok(created)
}

// ---------------------------------------------------------------------------
// Tests (port of `src/history/handoff.test.ts`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::ops::compact::Scout;
    use crate::history::ops::testkit::{scripted_ctx, session_with, text, Fixture, SessionOver};
    use crate::schema::parts::{Message, Role};
    use crate::turn::runner::DEFAULT_MODEL;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn snapshot(f: &Fixture, session_id: &str) -> String {
        let session = with_db(&f.ctx.db, |d| d.get_session(session_id)).unwrap();
        let messages = with_db(&f.ctx.db, |d| d.messages_for(session_id)).unwrap();
        serde_json::to_string(&json!({ "session": session, "messages": messages })).unwrap()
    }

    struct Scenario {
        parent: Session,
        source: Session,
        last: Message,
    }

    /// A source with an inherited ancestor turn — a handoff distils the VISIBLE
    /// thread.
    fn scenario(f: &Fixture) -> Scenario {
        let parent = session_with(
            f,
            SessionOver {
                title: "the migration".into(),
                workspace: Some("/tmp/checkout".into()),
                origin_dir: Some("/tmp/checkout".into()),
                base: Some("deadbeef".into()),
                ..Default::default()
            },
        );
        text(
            f,
            &parent.id,
            Role::User,
            "migrate the journal to the new key",
        );
        let source = session_with(
            f,
            SessionOver {
                title: "fork · the migration".into(),
                kind: SessionKind::Fork,
                parent_id: Some(parent.id.clone()),
                workspace: Some("/tmp/checkout".into()),
                origin_dir: Some("/tmp/checkout".into()),
                base: Some("deadbeef".into()),
                ..Default::default()
            },
        );
        text(
            f,
            &source.id,
            Role::Supervisor,
            "the key must hash the RESOLVED model",
        );
        let last = text(f, &source.id, Role::User, "ok — now do the relaunch path");
        Scenario {
            parent,
            source,
            last,
        }
    }

    fn goal(g: &str) -> HandoffBody {
        HandoffBody {
            goal: g.to_string(),
        }
    }

    /// A scout that never runs — every scenario pins a checkout, and the real
    /// scout is `explore.rs`'s business.
    fn no_scout() -> CompactDeps {
        CompactDeps {
            explore: Some(Arc::new(|_span, _ws| Box::pin(async { None }))),
        }
    }

    // ---- the draft ----------------------------------------------------------

    #[tokio::test]
    async fn handoff_opens_a_root_carrying_the_draft_and_seeds_no_messages() {
        let f = scripted_ctx();
        f.llm
            .set_reply("Finish the relaunch path in workflow/relaunch.rs.");
        let s = scenario(&f);

        let created = handoff(
            &f.ctx,
            &s.source.id,
            &goal("finish the relaunch path"),
            no_scout(),
        )
        .await
        .unwrap();

        assert_eq!(created.kind, SessionKind::Root);
        assert_eq!(created.parent_id, None);
        assert_eq!(
            created.draft.as_deref(),
            Some("Finish the relaunch path in workflow/relaunch.rs.")
        );
        // Nothing is copied: the distilled context is the draft and only the draft.
        assert!(with_db(&f.ctx.db, |d| d.messages_for(&created.id))
            .unwrap()
            .is_empty());
        assert!(with_db(&f.ctx.db, |d| d.thread_for(&created.id))
            .unwrap()
            .is_empty());
        // What storage kept, not just what was returned.
        assert_eq!(
            with_db(&f.ctx.db, |d| d.get_session(&created.id))
                .unwrap()
                .unwrap()
                .draft,
            created.draft
        );

        // The same checkout, with the sha the Changes rail measures from.
        assert_eq!(created.workspace.as_deref(), Some("/tmp/checkout"));
        assert_eq!(created.base.as_deref(), Some("deadbeef"));
        assert_eq!(created.origin_dir.as_deref(), Some("/tmp/checkout"));
        // Lineage back to the source, as of its last thread message.
        assert_eq!(created.origin_id.as_deref(), Some(s.source.id.as_str()));
        assert_eq!(
            created.origin_message_id.as_deref(),
            Some(s.last.id.as_str())
        );
        // Titled off the BASE title: handing off a fork must not compound.
        assert_eq!(created.title, "handoff · the migration");
    }

    #[tokio::test]
    async fn the_prompt_carries_the_whole_visible_thread_and_the_stated_goal() {
        let f = scripted_ctx();
        let s = scenario(&f);

        handoff(
            &f.ctx,
            &s.source.id,
            &goal("finish the relaunch path"),
            no_scout(),
        )
        .await
        .unwrap();

        assert_eq!(f.llm.prompts().len(), 1);
        let prompt = &f.llm.prompts()[0];
        // The inherited ancestor turn is in it.
        assert!(prompt.contains("migrate the journal to the new key"));
        assert!(prompt.contains("the key must hash the RESOLVED model"));
        assert!(prompt.contains("Goal for the new conversation: finish the relaunch path"));
        // The system prompt is the one that says "write the opening prompt".
        assert!(f.llm.systems()[0].contains("OPENING PROMPT"));
    }

    #[tokio::test]
    async fn the_draft_is_trimmed_and_an_empty_one_is_a_502_that_writes_nothing() {
        let f = scripted_ctx();
        f.llm.set_reply("  \n  Fix the ticker.  \n");
        let s = scenario(&f);
        let created = handoff(&f.ctx, &s.source.id, &goal("fix the ticker"), no_scout())
            .await
            .unwrap();
        assert_eq!(created.draft.as_deref(), Some("Fix the ticker."));

        let g = scripted_ctx();
        g.llm.set_reply("   \n ");
        let blank = scenario(&g);
        let before = with_db(&g.ctx.db, |d| d.list_sessions()).unwrap().len();
        let err = handoff(&g.ctx, &blank.source.id, &goal("anything"), no_scout())
            .await
            .unwrap_err();
        assert_eq!(err.status(), 502);
        // The LLM call completes before the first write, so a failed draft
        // leaves no empty root behind for the user to find.
        assert_eq!(
            with_db(&g.ctx.db, |d| d.list_sessions()).unwrap().len(),
            before
        );
    }

    // ---- the source is untouched --------------------------------------------

    #[tokio::test]
    async fn handoff_leaves_the_source_and_its_ancestor_byte_identical() {
        let f = scripted_ctx();
        let s = scenario(&f);
        let before = [snapshot(&f, &s.parent.id), snapshot(&f, &s.source.id)];

        handoff(
            &f.ctx,
            &s.source.id,
            &goal("carry on elsewhere"),
            no_scout(),
        )
        .await
        .unwrap();

        assert_eq!(
            [snapshot(&f, &s.parent.id), snapshot(&f, &s.source.id)],
            before
        );
    }

    // ---- model resolution ---------------------------------------------------

    #[tokio::test]
    async fn the_sessions_own_pin_decides_the_model_then_the_global_default() {
        let mut f = scripted_ctx();
        let s = scenario(&f);

        // No pin, no ctx default → the built-in.
        handoff(&f.ctx, &s.source.id, &goal("a"), no_scout())
            .await
            .unwrap();
        assert_eq!(f.llm.models()[0], DEFAULT_MODEL);

        // ctx default.
        f.ctx.model = Some("openai:gpt-5".into());
        handoff(&f.ctx, &s.source.id, &goal("b"), no_scout())
            .await
            .unwrap();
        assert_eq!(f.llm.models()[1], "openai:gpt-5");

        // A session pin wins over it — a model id is a provider routing
        // decision, and this user may hold only that provider's key.
        with_db(&f.ctx.db, |d| {
            d.set_session_model(&s.source.id, Some("vendor/some-model"))
        })
        .unwrap();
        handoff(&f.ctx, &s.source.id, &goal("c"), no_scout())
            .await
            .unwrap();
        assert_eq!(f.llm.models()[2], "vendor/some-model");
        // …and the new root inherits the pin, for the same reason.
        let created = handoff(&f.ctx, &s.source.id, &goal("d"), no_scout())
            .await
            .unwrap();
        assert_eq!(created.model.as_deref(), Some("vendor/some-model"));
    }

    // ---- refusals -----------------------------------------------------------

    #[tokio::test]
    async fn an_unknown_session_is_a_404_and_an_empty_thread_is_a_400() {
        let f = scripted_ctx();
        let empty = session_with(
            &f,
            SessionOver {
                title: "brand new".into(),
                ..Default::default()
            },
        );

        let missing = handoff(&f.ctx, "no-such-session", &goal("anything"), no_scout())
            .await
            .unwrap_err();
        assert_eq!(missing.status(), 404);

        let blank = handoff(&f.ctx, &empty.id, &goal("anything"), no_scout())
            .await
            .unwrap_err();
        assert_eq!(blank.status(), 400);
        assert!(blank.to_string().contains("empty thread"), "{blank}");

        // Neither bought an LLM call.
        assert!(f.llm.prompts().is_empty());
    }

    // ---- events -------------------------------------------------------------

    #[tokio::test]
    async fn the_root_is_created_then_updated_with_the_draft() {
        let f = scripted_ctx();
        f.llm.set_reply("the draft text");
        let s = scenario(&f);
        f.clear_events();

        let created = handoff(&f.ctx, &s.source.id, &goal("go"), no_scout())
            .await
            .unwrap();

        assert_eq!(f.event_types(), vec!["session.created", "session.updated"]);
        let events = f.events.lock().unwrap();
        assert!(events
            .iter()
            .all(|e| e.session_id.as_deref() == Some(created.id.as_str())));
        // The update carries the draft, so a live tree view has it without a
        // refetch.
        assert_eq!(events[1].data["draft"], "the draft text");
    }

    // ---- the title ----------------------------------------------------------

    #[tokio::test]
    async fn a_handoff_from_an_untitled_conversation_is_named_after_its_goal() {
        let f = scripted_ctx();
        let source = session_with(
            &f,
            SessionOver {
                title: String::new(),
                workspace: Some("/tmp/checkout".into()),
                ..Default::default()
            },
        );
        text(&f, &source.id, Role::User, "the coupons do not stack");

        let created = handoff(
            &f.ctx,
            &source.id,
            &goal("finish the coupon stacking fix"),
            no_scout(),
        )
        .await
        .unwrap();
        assert_eq!(created.title, "handoff · finish the coupon stacking fix");

        // A long goal is cut at a word boundary rather than mid-word.
        let long = handoff(
            &f.ctx,
            &source.id,
            &goal(
                "rewrite the whole pricing engine so that coupons, taxes and shipping compose \
                 in one pass instead of three",
            ),
            no_scout(),
        )
        .await
        .unwrap();
        assert!(
            long.title
                .starts_with("handoff · rewrite the whole pricing engine"),
            "{}",
            long.title
        );
        assert!(
            long.title.chars().count() <= "handoff · ".chars().count() + 49,
            "{}",
            long.title
        );
        assert!(long.title.ends_with('…'), "{}", long.title);
    }

    // ---- the drafting prompt ------------------------------------------------

    /// The instruction that stops the draft addressing the user. Observed
    /// before it existed: a thin transcript plus a goal it had no context for
    /// produced "Once you provide that, I can write a focused opening prompt
    /// for the new conversation." — which lands verbatim in the composer as if
    /// it were the distilled prompt, addressed to nobody.
    #[tokio::test]
    async fn the_drafting_prompt_forbids_replying_to_the_user_or_asking_for_input() {
        let f = scripted_ctx();
        let source = session_with(
            &f,
            SessionOver {
                title: "t".into(),
                workspace: Some("/tmp/checkout".into()),
                ..Default::default()
            },
        );
        text(&f, &source.id, Role::User, "anything");
        handoff(&f.ctx, &source.id, &goal("g"), no_scout())
            .await
            .unwrap();

        let system = f.llm.systems().last().cloned().unwrap_or_default();
        assert!(system.contains("never ask for more information"));
        assert!(system.contains("text the user will SEND"));
        // And it says what to do INSTEAD, or the model is left to invent a
        // fallback.
        assert!(system.contains("a short prompt is a correct answer"));
    }

    // ---- the scout ----------------------------------------------------------

    #[tokio::test]
    async fn scout_notes_reach_the_drafter_ahead_of_the_goal() {
        let f = scripted_ctx();
        let s = scenario(&f);
        with_db(&f.ctx.db, |d| d.set_session_workspace(&s.source.id, "/w")).unwrap();
        let saw: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sink = saw.clone();
        let scout: Scout = Arc::new(move |_thread, workspace: String| {
            let sink = sink.clone();
            Box::pin(async move {
                *sink.lock().unwrap() = workspace;
                Some("NOTES: the rename was reverted".to_string())
            })
        });

        handoff(
            &f.ctx,
            &s.source.id,
            &goal("finish the relaunch path"),
            CompactDeps {
                explore: Some(scout),
            },
        )
        .await
        .unwrap();

        assert_eq!(saw.lock().unwrap().clone(), "/w");
        let prompt = f.llm.prompts()[0].clone();
        assert!(prompt.contains("NOTES: the rename was reverted"));
        // Order matters: transcript, then what is true now, then what the new
        // thread is for.
        assert!(
            prompt.find("NOTES:") < prompt.find("Goal for the new conversation"),
            "the goal must be the last thing the drafter reads"
        );
    }

    #[tokio::test]
    async fn a_scout_with_nothing_to_say_still_hands_off() {
        let f = scripted_ctx();
        f.llm.set_reply("DRAFT");
        let s = scenario(&f);
        with_db(&f.ctx.db, |d| d.set_session_workspace(&s.source.id, "/w")).unwrap();

        let created = handoff(&f.ctx, &s.source.id, &goal("finish it"), no_scout())
            .await
            .unwrap();

        assert_eq!(created.draft.as_deref(), Some("DRAFT"));
        assert!(!f.llm.prompts()[0].contains("Scout notes"));
    }

    #[tokio::test]
    async fn a_session_with_no_workspace_is_never_scouted() {
        let f = scripted_ctx();
        f.llm.set_reply("DRAFT");
        // NOT `scenario`, which pins a checkout: the case is a session that
        // never had one, and then there is nothing on disk for a scout to read.
        let source = session_with(
            &f,
            SessionOver {
                title: "no checkout".into(),
                ..Default::default()
            },
        );
        text(&f, &source.id, Role::User, "we only talked");
        let exploding: Scout =
            Arc::new(|_span, _ws| Box::pin(async { panic!("there is no checkout to read") }));

        let created = handoff(
            &f.ctx,
            &source.id,
            &goal("finish it"),
            CompactDeps {
                explore: Some(exploding),
            },
        )
        .await
        .unwrap();

        assert_eq!(created.draft.as_deref(), Some("DRAFT"));
    }
}
