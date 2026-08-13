//! Compaction (port of `src/history/compact.ts`) — replacing a span of a
//! conversation with a summary, WITHOUT rewriting anything.
//!
//! THE INVARIANT THIS HOLDS: **compaction never mutates the session it
//! compacts.** It branches a SIBLING of the target (`parent_id =
//! target.parent_id`) and seeds it with copies of the pre-span messages, one
//! summary message, then copies of the post-span messages. The original
//! session's rows are untouched — same ids, same parts, same timestamps — so
//! the full and the compacted thread stay side by side in the tree and remain
//! comparable. The test asserts that literally: the source session and its
//! messages are JSON-identical after a compaction runs.
//!
//! WHY A SIBLING RATHER THAN A CHILD. `thread_for(s)` is *ancestors
//! root→parent, then s's own*. A branch parented at the TARGET'S parent
//! therefore inherits every shared ancestor for free, and its own seeded
//! messages reconstruct the rest of the thread with the span swapped for the
//! summary. Parenting at the target instead would inherit the very messages
//! compaction is removing, and no amount of seeding could take them back out.
//! That is also why a selection may only name the session's OWN messages: a
//! pick reaching into ancestor history is a 400 naming the ancestor, because
//! the operation that removes an ancestor's turns is a compaction OF THE
//! ANCESTOR.
//!
//! SELECTION NEED NOT BE CONTIGUOUS. Each maximal run of adjacent selected
//! messages collapses to ONE summary in place; unselected messages are copied
//! verbatim around the summaries, preserving thread order. A user who compacts
//! three separate debugging detours and keeps the design discussion between
//! them gets exactly that — rather than one summary of everything from the
//! first pick to the last, which would silently swallow what they deliberately
//! did not select.
//!
//! A pick may carry `parts` indexes to narrow what the SUMMARIZER SEES (a
//! turn's prose without its tool output). The message is still wholly
//! replaced: compaction shrinks, so unpicked parts drop rather than surviving
//! verbatim beside the summary.
//!
//! ORDER OF OPERATIONS: summarize FIRST, branch second. Every LLM call for the
//! whole selection completes before a single row is written, so a summarizer
//! that fails leaves no half-seeded branch behind for the user to find and
//! clean up. (Rust delta: the summaries are awaited in sequence rather than
//! through `Promise.all` — same ordering guarantee, one fewer concurrent
//! provider connection.)

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::errors::{BoughError, ErrorKind};
use crate::llm::{client_for, complete_text, ClientOpts, CompleteTextOpts};
use crate::schema::events::EventType;
use crate::schema::parts::{AskStatus, Message, Part, Role, Session, SessionKind};
use crate::schema::requests::{CompactBody, PartPick};
use crate::turn::runner::DEFAULT_MODEL;
use crate::types::{AppCtx, Db, LlmClient};

use super::explore::{explore_span, ExploreCtx};
use super::seed::{
    event, inherit_pins, open_branch, resolve_picks, to_value, with_db, BranchCtx, BranchSpec,
    ResolvedPick,
};

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// The scout seam (`history/ops/explore.rs`): a bash-capable subagent that
/// reads the current state of the directories a span touched, so the summary
/// can describe the checkout rather than the conversation's memory of it. A
/// seam so a test drives compaction with no shell and no second provider key.
///
/// Returning `None` — which it does for every failure, by design — is the
/// pre-scout behaviour: summarize from the transcript alone.
pub type Scout =
    Arc<dyn Fn(Vec<Message>, String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// Injection for [`compact`] (and, with the same seam, `handoff`).
#[derive(Clone, Default)]
pub struct CompactDeps {
    /// Absent = the real scout (`explore_span`).
    pub explore: Option<Scout>,
}

pub const SYSTEM: &str =
    "You are compacting a span of a coding-agent conversation. Produce a concise summary \
that preserves the decisions made, files/code changed, the resulting state, and any \
open questions — enough that the conversation can continue as if the original \
messages were still present. Output only the summary text.";

/// Appended to the system prompt only when a scout actually returned notes.
///
/// Separate from `SYSTEM`, and both halves matter. A summary that quietly
/// averaged the transcript against the tree would be the worst of both — so the
/// notes are named as the authority on present state, and the transcript stays
/// the authority on what was decided and why, which no amount of reading the
/// checkout can recover.
pub const SCOUT_SYSTEM: &str =
    " You are also given SCOUT NOTES: what a subagent found in the files this span \
touched, read from the checkout as it stands now. Where the notes and the \
conversation disagree about the state of the code, the notes are right and the \
summary must say what is actually there — the conversation records intentions, some \
of which were undone later. The conversation remains the only source for decisions, \
reasons and open questions.";

const MAX_TOKENS: i64 = 1024;
/// Keeps the prompt bounded when a span contains a 200KB tool result.
const PART_CLIP: usize = 2000;

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Clip at `n` CHARS (not bytes — a byte slice inside a multi-byte codepoint
/// would panic), appending `…` when anything was dropped.
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// A tool payload as one string: a JSON string renders as its text, anything
/// else as its JSON.
fn stringify(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Supervisor => "supervisor",
        Role::System => "system",
    }
}

/// One part as a line of transcript.
///
/// Exhaustive over the part union on purpose (no wildcard arm): a new part kind
/// is a compile error here rather than a span that silently summarizes without
/// it.
fn render_part(role: Role, p: &Part) -> String {
    let role = role_str(role);
    match p {
        Part::Text { text } | Part::Reasoning { text, .. } => format!("{role}: {text}"),
        Part::ToolCall { name, input, .. } => {
            format!(
                "{role}: [tool {name}] {}",
                clip(&stringify(input), PART_CLIP)
            )
        }
        Part::ToolResult {
            output,
            is_error,
            interrupted,
            ..
        } => format!(
            "tool_result{}{}: {}",
            if *is_error { " (error)" } else { "" },
            if interrupted.unwrap_or(false) {
                " (interrupted)"
            } else {
                ""
            },
            clip(&stringify(output), PART_CLIP)
        ),
        Part::Image { name, .. } => format!("{role}: [image {name}]"),
        // A settled ask() Q/A: what was asked and how the human resolved it.
        // The answer is often the decision the rest of the span rests on.
        Part::Ask {
            question,
            status,
            answer,
            ..
        } => {
            let resolution = match status {
                AskStatus::Answered => {
                    format!("user answered: {}", answer.clone().unwrap_or_default())
                }
                AskStatus::Declined => "declined".to_string(),
                AskStatus::Interrupted => "interrupted".to_string(),
            };
            format!("ask: {question} → {resolution}")
        }
        // Kept in the summary even though replay drops it: a compacted span is
        // the only place left that remembers a fan-out was launched here once
        // the `[workflow done]` note has itself been compacted away.
        Part::Workflow {
            name, description, ..
        } => {
            format!("{role}: [workflow {name}] {}", clip(description, PART_CLIP))
        }
    }
}

/// Messages rendered as a plain transcript for an LLM prompt.
///
/// Public because handoff renders the same thing for a different prompt, and
/// the scout mines paths out of it — a second renderer would drift the moment a
/// part kind is added.
pub fn render_span(messages: &[Message]) -> String {
    messages
        .iter()
        .flat_map(|m| {
            if m.parts.is_empty() {
                // A message with no parts still contributes a line, so the
                // roles stay legible.
                vec![format!("{}:", role_str(m.role))]
            } else {
                m.parts
                    .iter()
                    .map(|p| render_part(m.role, p))
                    .collect::<Vec<_>>()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// One maximal run of adjacent selected messages — what becomes a single
/// summary.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
    pub span: Vec<Message>,
}

/// Group picked thread indexes into maximal runs of ADJACENT messages. Pure,
/// and the whole of the non-contiguous rule: two picks separated by an
/// unselected message are two runs, and therefore two summaries with that
/// message copied between them.
pub fn runs_of(picked: &[ResolvedPick]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for p in picked {
        match runs.last_mut() {
            Some(last) if p.idx == last.end + 1 => {
                last.end = p.idx;
                last.span.push(p.view.clone());
            }
            _ => runs.push(Run {
                start: p.idx,
                end: p.idx,
                span: vec![p.view.clone()],
            }),
        }
    }
    runs
}

// ---------------------------------------------------------------------------
// Summarizing
// ---------------------------------------------------------------------------

fn compact_err(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Compact, message)
}

/// The model that summarizes.
///
/// Resolved exactly as the turn runner resolves it — session pin, then the
/// global default, then `DEFAULT_MODEL` — because a model id IS a provider
/// routing decision: a session pinned to an OpenAI or OpenRouter model belongs
/// to a user who may hold only that provider's key, and summarizing it on the
/// Anthropic default would fail the compaction with an auth error on a
/// conversation that runs fine.
pub(crate) fn model_for(ctx: &AppCtx, session: &Session) -> String {
    session
        .model
        .clone()
        .or_else(|| ctx.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// The injected client, else the provider-routed one for the resolved model.
pub(crate) fn llm_for(ctx: &AppCtx, model: &str) -> Arc<dyn LlmClient> {
    ctx.llm
        .clone()
        .unwrap_or_else(|| client_for(model, ClientOpts::default()))
}

/// Summarize a span of messages. Public because a BRANCH SWITCH needs the same
/// thing compaction does: fork's `summarizeAbandoned` carries "the essence of
/// abandoned work without all the token-heavy details" onto the new path, and
/// that is this function with a different span.
pub async fn summarize_span(
    ctx: &AppCtx,
    model: &str,
    span: &[Message],
    instructions: Option<&str>,
) -> Result<String, BoughError> {
    summarize(ctx, model, span, instructions, None).await
}

async fn summarize(
    ctx: &AppCtx,
    model: &str,
    span: &[Message],
    instructions: Option<&str>,
    notes: Option<&str>,
) -> Result<String, BoughError> {
    let llm = llm_for(ctx, model);
    let notes = notes.filter(|n| !n.is_empty());
    let mut parts = vec![render_span(span)];
    if let Some(notes) = notes {
        parts.push(format!(
            "Scout notes — the files this span touched, as they are now:\n{notes}"
        ));
    }
    if let Some(instructions) = instructions.filter(|i| !i.is_empty()) {
        parts.push(format!("Additional instructions: {instructions}"));
    }
    let system = match notes {
        Some(_) => format!("{SYSTEM}{SCOUT_SYSTEM}"),
        None => SYSTEM.to_string(),
    };
    let text = complete_text(
        &llm,
        CompleteTextOpts {
            model: model.to_string(),
            system,
            max_tokens: MAX_TOKENS,
            prompt: parts.join("\n\n"),
        },
    )
    .await?;
    let text = text.trim().to_string();
    // An empty summary is not a summary. Seeding it would put an empty message
    // where a span of work used to be — a branch that silently lost the span
    // rather than compacting it. Raised before anything is written, so the
    // branch never exists.
    if text.is_empty() {
        return Err(compact_err(
            502,
            format!(
                "the summarizer ({model}) returned no text for a span of {} message(s) — \
                 nothing was written; retry, or narrow the selection",
                span.len()
            ),
        ));
    }
    Ok(text)
}

// ---------------------------------------------------------------------------
// The operation
// ---------------------------------------------------------------------------

/// Reject a pick that is not one of the session's own messages, with the error
/// that says what to do about it.
///
/// Naming the ancestor is the difference between an error the user can act on
/// and one they cannot: the operation they want exists, it just runs on a
/// different session.
fn assert_own_messages(
    db: &dyn Db,
    session: &Session,
    picks: &[PartPick],
    own: &[Message],
) -> Result<(), BoughError> {
    let mut ancestors: Option<Vec<String>> = None;
    for p in picks {
        if own.iter().any(|m| m.id == p.message_id) {
            continue;
        }
        if ancestors.is_none() {
            // `ancestor_chain` is root→self INCLUSIVE; the session itself is
            // not one of its own ancestors (TS: `.slice(0, -1)`).
            let chain = db.ancestor_chain(&session.id)?;
            ancestors = Some(
                chain
                    .iter()
                    .filter(|s| s.id != session.id)
                    .map(|s| s.id.clone())
                    .collect(),
            );
        }
        let foreign = db.get_message(&p.message_id)?;
        if let Some(f) = &foreign {
            if ancestors
                .as_ref()
                .is_some_and(|a| a.contains(&f.session_id))
            {
                return Err(compact_err(
                    400,
                    format!(
                        "message {} belongs to ancestor session {}, not to {} — a compaction \
                         branches a sibling of the session it compacts, so it can only replace \
                         that session's own turns. Compact {} instead.",
                        p.message_id, f.session_id, session.id, f.session_id
                    ),
                ));
            }
        }
        return Err(compact_err(
            400,
            format!(
                "message {} is not a message of session {}{}",
                p.message_id,
                session.id,
                match &foreign {
                    Some(f) => format!(" (it belongs to {})", f.session_id),
                    None => String::new(),
                }
            ),
        ));
    }
    Ok(())
}

/// The deterministic branch title.
///
/// Deliberately NOT built on `base_title(session.title)` the way fork's is: the
/// strip list does not include this prefix, so composing "compacted · <base>"
/// would accumulate — compact a compaction and the picker shows "compacted ·
/// compacted · X". A standalone label cannot accumulate, and the cheap tier
/// replaces it with something about the content as soon as the first summary
/// exists.
fn compaction_title(picks: usize) -> String {
    format!(
        "compacted · {picks} turn{}",
        if picks == 1 { "" } else { "s" }
    )
}

/// Compact the selected messages of `session_id` onto a new compaction branch
/// and return the new session.
///
/// Each maximal contiguous run of selected messages is replaced in place by one
/// summary; everything unselected is copied verbatim. The source session is
/// never touched. 404 for an unknown session, 400 for a selection this
/// operation cannot express, 502 for a summarizer that produced nothing.
pub async fn compact(
    ctx: &AppCtx,
    session_id: &str,
    args: &CompactBody,
    deps: CompactDeps,
) -> Result<Session, BoughError> {
    let session = with_db(&ctx.db, |d| d.get_session(session_id))?
        .ok_or_else(|| compact_err(404, format!("session {session_id} not found")))?;

    // The schema already rejects an empty selection at the router edge, but
    // this function is also called directly, and an empty selection would
    // otherwise reach the seeding loop with no last pick to name.
    if args.picks.is_empty() {
        return Err(compact_err(
            400,
            "compaction needs at least one picked message",
        ));
    }

    let own = with_db(&ctx.db, |d| d.messages_for(session_id))?;
    if own.is_empty() {
        return Err(compact_err(
            400,
            format!("session {session_id} has no messages of its own to compact"),
        ));
    }
    with_db(&ctx.db, |d| {
        assert_own_messages(d, &session, &args.picks, &own)
    })?;

    // Resolved against the session's OWN messages, so a thread index here is an
    // index into exactly the sequence the branch re-seeds. `resolve_picks`
    // merges duplicate picks, validates part ranges, and restores thread order.
    let picked = resolve_picks(&own, &args.picks, |m| compact_err(400, m))?;
    let runs = runs_of(&picked);

    let model = model_for(ctx, &session);
    // The runtime is read HERE rather than at the seeder, because the scout
    // needs the workspace and the scout runs before the first summary — see the
    // ordering note in the header: everything that can fail happens before
    // anything is written.
    let runtime = with_db(&ctx.db, |d| d.get_session_runtime(session_id))?;

    // One scout PER RUN: each run is a separate summary about a separate
    // stretch of work, and pointing one scout at the union of their files would
    // scope every summary by every other run's subject. Failures are already
    // `None` inside the scout, so this cannot fail. No workspace, no scout:
    // there is no checkout to read, and the transcript is then all there ever
    // was to summarize from.
    let notes: Vec<Option<String>> = match runtime.workspace.clone().filter(|w| !w.is_empty()) {
        Some(workspace) => {
            let scout = deps
                .explore
                .clone()
                .unwrap_or_else(|| default_scout(session_id.to_string()));
            futures::future::join_all(
                runs.iter()
                    .map(|r| scout(r.span.clone(), workspace.clone())),
            )
            .await
        }
        None => runs.iter().map(|_| None).collect(),
    };

    // Every summary before the first write (see the header). One failed summary
    // means this compaction cannot be expressed, and there is nothing partial
    // worth keeping.
    let mut summaries: Vec<String> = Vec::with_capacity(runs.len());
    for (i, run) in runs.iter().enumerate() {
        summaries.push(
            summarize(
                ctx,
                &model,
                &run.span,
                args.instructions.as_deref(),
                notes[i].as_deref(),
            )
            .await?,
        );
    }

    let branch_ctx = BranchCtx::from(ctx);
    let seeder = open_branch(
        branch_ctx.clone(),
        BranchSpec {
            // The TARGET'S parent — a sibling, not a child. This is the whole
            // mechanism.
            parent_id: session.parent_id.clone(),
            title: compaction_title(picked.len()),
            kind: Some(SessionKind::Compaction),
            // A compaction continues the same work in the same checkout, so it
            // inherits both the workspace and the `base` sha its change set is
            // measured from — otherwise the branch shows no changes for work
            // that is plainly in the tree.
            workspace: runtime.workspace.clone(),
            base: runtime.base.clone(),
            origin_dir: session.origin_dir.clone(),
            origin_id: Some(session.id.clone()), // lineage: the compacted session…
            origin_message_id: Some(own[picked[picked.len() - 1].idx].id.clone()), // …last picked
        },
    )?;

    // Seed in thread order: copies of the unselected messages, each run swapped
    // for its one summary. The shared ancestors come from
    // thread-through-parents.
    let mut run = 0usize;
    let mut i = 0usize;
    while i < own.len() {
        if run < runs.len() && i == runs[run].start {
            // `supervisor`, not `system`: the summary stands in for a stretch
            // of the conversation and replays as an assistant message, which is
            // what makes the compacted thread read — and replay — as a
            // continuation rather than as a harness note about one.
            seeder.add(
                Role::Supervisor,
                vec![Part::Text {
                    text: summaries[run].clone(),
                }],
            )?;
            i = runs[run].end + 1; // skip the rest of the run
            run += 1;
        } else {
            seeder.copy(&own[i])?;
            i += 1;
        }
    }

    let branch = inherit_pins(&branch_ctx, &session, seeder.session.clone())?;
    retitle(ctx, &branch, summaries[0].clone(), picked.len());
    Ok(branch)
}

/// The real scout, bound to the session it runs for.
fn default_scout(session_id: String) -> Scout {
    Arc::new(move |span: Vec<Message>, workspace: String| {
        let session_id = session_id.clone();
        Box::pin(async move {
            explore_span(
                &ExploreCtx {
                    session_id,
                    workspace,
                    llm: None,
                    model: None,
                    registry: None,
                },
                &span,
            )
            .await
        })
    })
}

/// Name the branch from its first summary. Fire-and-forget by design: the
/// response carries the branch immediately, and the rename lands as a
/// `session.updated` when (and only if) the cheap tier answers.
///
/// Two guards, both about not overwriting a fact the user established: the
/// rename is skipped if the branch's title is no longer the placeholder — the
/// user renamed it first, or a previous rename already landed — and every
/// failure is swallowed, because a cosmetic title must never turn a completed
/// compaction into an error.
fn retitle(ctx: &AppCtx, branch: &Session, summary: String, picks: usize) {
    let Some(cheap) = ctx.cheap.clone() else {
        return;
    };
    let placeholder = branch.title.clone();
    let branch_id = branch.id.clone();
    let db = ctx.db.clone();
    let bus = ctx.bus.clone();
    tokio::spawn(async move {
        // No glossary: the input is a summary this system already wrote, in
        // the project's own words, not a stranger's first message.
        let Some(title) = cheap.title(&summary, &[]).await else {
            return;
        };
        if title.is_empty() {
            return;
        }
        if !matches!(with_db(&db, |d| d.get_session(&branch_id)), Ok(Some(ref s)) if s.title == placeholder)
        {
            return;
        }
        let renamed = format!("{title} · compacted {picks}");
        if with_db(&db, |d| d.set_session_title(&branch_id, &renamed)).is_err() {
            return;
        }
        if let Ok(Some(updated)) = with_db(&db, |d| d.get_session(&branch_id)) {
            bus.publish(event(
                EventType::SessionUpdated,
                &branch_id,
                to_value(&updated),
            ));
        }
    });
}

// ---------------------------------------------------------------------------
// Tests (port of `src/history/compact.test.ts`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::ops::testkit::{
        conversation, message, picks, scripted_ctx, session_with, texts_of, Fixture, SessionOver,
    };
    use serde_json::json;
    use std::sync::Mutex;

    fn snapshot(f: &Fixture, session_id: &str) -> String {
        let session = with_db(&f.ctx.db, |d| d.get_session(session_id)).unwrap();
        let messages = with_db(&f.ctx.db, |d| d.messages_for(session_id)).unwrap();
        serde_json::to_string(&json!({ "session": session, "messages": messages })).unwrap()
    }

    fn session_count(f: &Fixture) -> usize {
        with_db(&f.ctx.db, |d| d.list_sessions()).unwrap().len()
    }

    fn own_texts(f: &Fixture, session_id: &str) -> Vec<String> {
        texts_of(&with_db(&f.ctx.db, |d| d.messages_for(session_id)).unwrap())
    }

    fn body(picks: Vec<PartPick>) -> CompactBody {
        CompactBody {
            picks,
            instructions: None,
        }
    }

    // ---- the AC -------------------------------------------------------------

    #[tokio::test]
    async fn a_non_contiguous_selection_collapses_each_run_to_one_summary() {
        let f = scripted_ctx();
        // 0..6, selecting {1,2} and {5}: two runs, so two summaries, with 3 and
        // 4 copied verbatim between them and 0 and 6 copied around them.
        let (source, messages) = conversation(
            &f,
            &["m0", "m1", "m2", "m3", "m4", "m5", "m6"],
            SessionOver::default(),
        );

        let branch = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1, 2, 5])),
            CompactDeps::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            own_texts(&f, &branch.id),
            vec!["m0", "SUMMARY-0", "m3", "m4", "SUMMARY-1", "m6"]
        );
        // Exactly two summarizer calls — one per run, not one per picked message.
        assert_eq!(f.llm.prompts().len(), 2);
        // …and each saw only its own run.
        assert!(f.llm.prompts()[0].contains("m1"));
        assert!(f.llm.prompts()[0].contains("m2"));
        assert!(!f.llm.prompts()[0].contains("m5"));
        assert!(f.llm.prompts()[1].contains("m5"));
        assert!(!f.llm.prompts()[1].contains("m1"));

        // The copies are copies, not moves: new ids, same text, same roles.
        let seeded = with_db(&f.ctx.db, |d| d.messages_for(&branch.id)).unwrap();
        assert!(!seeded.iter().any(|m| messages.iter().any(|s| s.id == m.id)));
        assert_eq!(
            seeded.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![
                Role::User,
                Role::Supervisor,
                Role::Supervisor,
                Role::User,
                Role::Supervisor,
                Role::User
            ]
        );
    }

    #[tokio::test]
    async fn the_compacted_session_is_byte_unchanged() {
        let f = scripted_ctx();
        let (source, messages) =
            conversation(&f, &["a", "b", "c", "d", "e"], SessionOver::default());
        let before = snapshot(&f, &source.id);

        compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1, 3])),
            CompactDeps::default(),
        )
        .await
        .unwrap();

        assert_eq!(snapshot(&f, &source.id), before);
    }

    // ---- selection semantics ------------------------------------------------

    #[tokio::test]
    async fn a_contiguous_selection_is_one_summary_in_place() {
        let f = scripted_ctx();
        let (source, messages) = conversation(&f, &["a", "b", "c", "d"], SessionOver::default());

        let branch = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1, 2])),
            CompactDeps::default(),
        )
        .await
        .unwrap();

        assert_eq!(own_texts(&f, &branch.id), vec!["a", "SUMMARY-0", "d"]);
        assert_eq!(f.llm.prompts().len(), 1);
    }

    #[tokio::test]
    async fn picks_are_ordered_and_de_duplicated_whatever_order_the_client_sent() {
        let f = scripted_ctx();
        let (source, messages) =
            conversation(&f, &["a", "b", "c", "d", "e"], SessionOver::default());

        // Sent backwards, with one duplicate — a user shift-clicking upward.
        let branch = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[3, 1, 3])),
            CompactDeps::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            own_texts(&f, &branch.id),
            vec!["a", "SUMMARY-0", "c", "SUMMARY-1", "e"]
        );
        assert!(f.llm.prompts()[0].contains('b'));
        assert!(f.llm.prompts()[1].contains('d'));
    }

    #[test]
    fn runs_of_groups_only_adjacent_indexes() {
        let view = |i: usize| Message {
            id: format!("m{i}"),
            session_id: "s".into(),
            role: Role::User,
            parts: vec![],
            pending: false,
            created_at: 0,
        };
        let runs = runs_of(
            &[0usize, 1, 2, 5, 7, 8]
                .iter()
                .map(|&idx| ResolvedPick {
                    idx,
                    view: view(idx),
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            runs.iter().map(|r| (r.start, r.end)).collect::<Vec<_>>(),
            vec![(0, 2), (5, 5), (7, 8)]
        );
    }

    #[tokio::test]
    async fn a_part_pick_narrows_what_the_summarizer_sees_and_the_message_is_still_replaced() {
        let f = scripted_ctx();
        let source = session_with(
            &f,
            SessionOver {
                title: "parts".into(),
                ..Default::default()
            },
        );
        message(
            &f,
            &source.id,
            Role::User,
            vec![Part::Text {
                text: "keep-me".into(),
            }],
        );
        let target = message(
            &f,
            &source.id,
            Role::Supervisor,
            vec![
                Part::Text {
                    text: "prose-part".into(),
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "noisy-tool-input" }),
                },
            ],
        );

        let branch = compact(
            &f.ctx,
            &source.id,
            &body(vec![PartPick {
                message_id: target.id.clone(),
                parts: Some(vec![0]),
            }]),
            CompactDeps::default(),
        )
        .await
        .unwrap();

        assert!(f.llm.prompts()[0].contains("prose-part"));
        assert!(!f.llm.prompts()[0].contains("noisy-tool-input"));
        // The message is wholly replaced — the unpicked tool call does not
        // survive beside it.
        assert_eq!(own_texts(&f, &branch.id), vec!["keep-me", "SUMMARY-0"]);
    }

    #[tokio::test]
    async fn instructions_steer_the_summary_prompt() {
        let f = scripted_ctx();
        let (source, messages) = conversation(&f, &["a", "b"], SessionOver::default());

        compact(
            &f.ctx,
            &source.id,
            &CompactBody {
                picks: picks(&messages, &[0]),
                instructions: Some("keep the file paths".into()),
            },
            CompactDeps::default(),
        )
        .await
        .unwrap();

        assert!(f.llm.prompts()[0].contains("Additional instructions: keep the file paths"));
    }

    // ---- the branch ---------------------------------------------------------

    #[tokio::test]
    async fn the_branch_is_a_sibling_so_ancestors_come_through_the_parent_chain() {
        let f = scripted_ctx();
        let root = session_with(
            &f,
            SessionOver {
                title: "root".into(),
                ..Default::default()
            },
        );
        message(
            &f,
            &root.id,
            Role::User,
            vec![Part::Text {
                text: "ancestor-1".into(),
            }],
        );
        message(
            &f,
            &root.id,
            Role::Supervisor,
            vec![Part::Text {
                text: "ancestor-2".into(),
            }],
        );
        let child = session_with(
            &f,
            SessionOver {
                title: "child".into(),
                parent_id: Some(root.id.clone()),
                kind: SessionKind::Fork,
                ..Default::default()
            },
        );
        let own: Vec<Message> = ["own-a", "own-b", "own-c"]
            .iter()
            .map(|t| {
                message(
                    &f,
                    &child.id,
                    Role::User,
                    vec![Part::Text { text: (*t).into() }],
                )
            })
            .collect();

        let branch = compact(
            &f.ctx,
            &child.id,
            &body(picks(&own, &[1])),
            CompactDeps::default(),
        )
        .await
        .unwrap();

        assert_eq!(branch.parent_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(branch.kind, SessionKind::Compaction);
        assert_eq!(branch.origin_id.as_deref(), Some(child.id.as_str()));
        assert_eq!(
            branch.origin_message_id.as_deref(),
            Some(own[1].id.as_str())
        );
        // The ancestor's messages were never copied…
        assert_eq!(
            own_texts(&f, &branch.id),
            vec!["own-a", "SUMMARY-0", "own-c"]
        );
        // …and still appear in the thread, before the branch's own.
        assert_eq!(
            texts_of(&with_db(&f.ctx.db, |d| d.thread_for(&branch.id)).unwrap()),
            vec!["ancestor-1", "ancestor-2", "own-a", "SUMMARY-0", "own-c"]
        );
    }

    #[tokio::test]
    async fn the_branch_inherits_the_checkout_the_base_sha_and_the_pins() {
        let f = scripted_ctx();
        let source = session_with(
            &f,
            SessionOver {
                title: "pinned".into(),
                workspace: Some("/tmp/checkout".into()),
                base: Some("abc123".into()),
                origin_dir: Some("/tmp/checkout".into()),
                model: Some("openai:gpt-5".into()),
                effort: Some("high".into()),
                ..Default::default()
            },
        );
        let own = vec![message(
            &f,
            &source.id,
            Role::User,
            vec![Part::Text { text: "x".into() }],
        )];

        // A workspace means a scout would run; the seam is injected so the test
        // stays offline and shell-free.
        let branch = compact(
            &f.ctx,
            &source.id,
            &body(picks(&own, &[0])),
            CompactDeps {
                explore: Some(no_scout()),
            },
        )
        .await
        .unwrap();

        let runtime = with_db(&f.ctx.db, |d| d.get_session_runtime(&branch.id)).unwrap();
        assert_eq!(runtime.workspace.as_deref(), Some("/tmp/checkout"));
        assert_eq!(runtime.base.as_deref(), Some("abc123"));
        assert_eq!(branch.origin_dir.as_deref(), Some("/tmp/checkout"));
        assert_eq!(branch.model.as_deref(), Some("openai:gpt-5"));
        assert_eq!(branch.effort.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn the_summarizer_runs_on_the_sessions_pinned_model() {
        let f = scripted_ctx();
        let source = session_with(
            &f,
            SessionOver {
                title: "pinned".into(),
                model: Some("openai:gpt-5".into()),
                ..Default::default()
            },
        );
        let own = vec![message(
            &f,
            &source.id,
            Role::User,
            vec![Part::Text { text: "x".into() }],
        )];

        compact(
            &f.ctx,
            &source.id,
            &body(picks(&own, &[0])),
            CompactDeps::default(),
        )
        .await
        .unwrap();

        assert_eq!(f.llm.models(), vec!["openai:gpt-5".to_string()]);
    }

    #[tokio::test]
    async fn the_branch_is_announced_before_the_messages_seeded_into_it() {
        let f = scripted_ctx();
        let (source, messages) = conversation(&f, &["a", "b", "c"], SessionOver::default());

        let branch = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1])),
            CompactDeps::default(),
        )
        .await
        .unwrap();

        let mine: Vec<String> = f
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(branch.id.as_str()))
            .map(|e| e.r#type.as_str().to_string())
            .collect();
        assert_eq!(
            mine,
            vec![
                "session.created",
                "message.started",
                "message.started",
                "message.started"
            ]
        );
    }

    // ---- refusals -----------------------------------------------------------

    #[tokio::test]
    async fn a_selection_reaching_into_ancestor_history_is_a_400_naming_the_ancestor() {
        let f = scripted_ctx();
        let root = session_with(
            &f,
            SessionOver {
                title: "root".into(),
                ..Default::default()
            },
        );
        let ancestor_message = message(
            &f,
            &root.id,
            Role::User,
            vec![Part::Text {
                text: "ancestor".into(),
            }],
        );
        let child = session_with(
            &f,
            SessionOver {
                title: "child".into(),
                parent_id: Some(root.id.clone()),
                kind: SessionKind::Fork,
                ..Default::default()
            },
        );
        message(
            &f,
            &child.id,
            Role::User,
            vec![Part::Text { text: "own".into() }],
        );
        let before = session_count(&f);

        let err = compact(
            &f.ctx,
            &child.id,
            &body(vec![PartPick {
                message_id: ancestor_message.id,
                parts: None,
            }]),
            CompactDeps::default(),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status(), 400);
        assert!(
            err.to_string().contains(&root.id),
            "names the ancestor to compact instead"
        );
        assert_eq!(session_count(&f), before, "nothing was branched");
        assert_eq!(f.llm.prompts().len(), 0, "and nothing was paid for");
    }

    #[tokio::test]
    async fn unknown_message_bad_part_empty_session_and_unknown_session_all_refuse() {
        let f = scripted_ctx();
        let (source, messages) = conversation(&f, &["a", "b"], SessionOver::default());

        let unknown = compact(
            &f.ctx,
            &source.id,
            &body(vec![PartPick {
                message_id: "nope".into(),
                parts: None,
            }]),
            CompactDeps::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.status(), 400);

        let range = compact(
            &f.ctx,
            &source.id,
            &body(vec![PartPick {
                message_id: messages[0].id.clone(),
                parts: Some(vec![7]),
            }]),
            CompactDeps::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(range.status(), 400);
        assert!(range.to_string().contains("part index"));

        let empty = session_with(
            &f,
            SessionOver {
                title: "no messages".into(),
                ..Default::default()
            },
        );
        let none = compact(
            &f.ctx,
            &empty.id,
            &body(vec![PartPick {
                message_id: messages[0].id.clone(),
                parts: None,
            }]),
            CompactDeps::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(none.status(), 400);

        let missing = compact(
            &f.ctx,
            "no-such-session",
            &body(vec![PartPick {
                message_id: messages[0].id.clone(),
                parts: None,
            }]),
            CompactDeps::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(missing.status(), 404);
    }

    #[tokio::test]
    async fn a_failed_summarizer_leaves_no_half_seeded_branch() {
        // The FIRST summary succeeds and the second fails — the case where a
        // naive implementation has already written half a transcript.
        let f = scripted_ctx();
        f.llm.set_failure_after(1, "provider exploded");
        let (source, messages) =
            conversation(&f, &["a", "b", "c", "d", "e"], SessionOver::default());
        let before = snapshot(&f, &source.id);
        let sessions_before = session_count(&f);

        let err = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1, 3])),
            CompactDeps::default(),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("provider exploded"));
        assert_eq!(session_count(&f), sessions_before, "no branch was created");
        assert_eq!(snapshot(&f, &source.id), before);
    }

    #[tokio::test]
    async fn an_empty_summary_is_a_502_not_a_message_that_lost_the_span() {
        let f = scripted_ctx();
        f.llm.set_reply("   ");
        let (source, messages) = conversation(&f, &["a", "b"], SessionOver::default());

        let err = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[0])),
            CompactDeps::default(),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status(), 502);
        assert_eq!(session_count(&f), 1, "nothing was branched");
    }

    // ---- the title ----------------------------------------------------------

    #[tokio::test]
    async fn the_deterministic_title_counts_picked_messages_and_never_compounds() {
        let f = scripted_ctx();
        let (source, messages) = conversation(&f, &["a", "b", "c", "d"], SessionOver::default());

        let one = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1])),
            CompactDeps::default(),
        )
        .await
        .unwrap();
        assert_eq!(one.title, "compacted · 1 turn");

        let two = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1, 2])),
            CompactDeps::default(),
        )
        .await
        .unwrap();
        assert_eq!(two.title, "compacted · 2 turns");

        // Compacting a compaction does not stack prefixes.
        let own = with_db(&f.ctx.db, |d| d.messages_for(&two.id)).unwrap();
        let again = compact(
            &f.ctx,
            &two.id,
            &body(vec![PartPick {
                message_id: own[0].id.clone(),
                parts: None,
            }]),
            CompactDeps::default(),
        )
        .await
        .unwrap();
        assert_eq!(again.title, "compacted · 1 turn");
    }

    #[tokio::test]
    async fn the_cheap_tier_renames_the_branch_and_a_silent_one_keeps_the_placeholder() {
        struct Titler {
            seen: Mutex<Vec<String>>,
            answer: Option<String>,
        }
        #[async_trait::async_trait]
        impl crate::types::CheapTier for Titler {
            async fn title(&self, first_message: &str, _glossary: &[String]) -> Option<String> {
                self.seen.lock().unwrap().push(first_message.to_string());
                self.answer.clone()
            }
            async fn ghost_text(&self, _prefix: &str) -> Option<String> {
                None
            }
            async fn activity(&self, _recent: &str) -> Option<String> {
                None
            }
        }

        let mut f = scripted_ctx();
        let titler = Arc::new(Titler {
            seen: Mutex::new(vec![]),
            answer: Some("token refresh race".into()),
        });
        f.ctx.cheap = Some(titler.clone());
        let (source, messages) = conversation(&f, &["a", "b", "c"], SessionOver::default());

        let branch = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1])),
            CompactDeps::default(),
        )
        .await
        .unwrap();
        // The response never waits for a rename.
        assert_eq!(branch.title, "compacted · 1 turn");

        assert!(
            wait_for_title(&f, &branch.id, "token refresh race · compacted 1").await,
            "the cheap tier's rename must land"
        );
        assert_eq!(
            titler.seen.lock().unwrap().clone(),
            vec!["SUMMARY-0".to_string()]
        );
        assert!(f
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.r#type.as_str() == "session.updated"
                && e.session_id.as_deref() == Some(branch.id.as_str())));

        // A cheap tier that answers nothing is silent: the compaction stands,
        // the title stays.
        f.ctx.cheap = Some(Arc::new(Titler {
            seen: Mutex::new(vec![]),
            answer: None,
        }));
        let second = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[2])),
            CompactDeps::default(),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(
            with_db(&f.ctx.db, |d| d.get_session(&second.id))
                .unwrap()
                .unwrap()
                .title,
            "compacted · 1 turn"
        );
    }

    /// Let the fire-and-forget rename settle without a fixed sleep.
    async fn wait_for_title(f: &Fixture, id: &str, want: &str) -> bool {
        for _ in 0..200 {
            if with_db(&f.ctx.db, |d| d.get_session(id))
                .unwrap()
                .is_some_and(|s| s.title == want)
            {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        false
    }

    // ---- the scout ----------------------------------------------------------
    //
    // `explore.rs` owns what the scout is pointed at and how its loop runs;
    // what belongs here is only the contract between them: notes reach the
    // summarizer, and a compaction happens either way.

    fn no_scout() -> Scout {
        Arc::new(|_span, _workspace| Box::pin(async { None }))
    }

    #[tokio::test]
    async fn scout_notes_reach_the_summarizer_per_run_with_the_prompt_that_ranks_them() {
        let f = scripted_ctx();
        let (source, messages) = conversation(
            &f,
            &["m0", "m1", "m2", "m3", "m4", "m5"],
            SessionOver::default(),
        );
        // A scout only runs where there is a checkout to read.
        with_db(&f.ctx.db, |d| d.set_session_workspace(&source.id, "/w")).unwrap();

        let spans: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(vec![]));
        let sink = spans.clone();
        let scout: Scout = Arc::new(move |span: Vec<Message>, _ws| {
            let sink = sink.clone();
            Box::pin(async move {
                let mut seen = sink.lock().unwrap();
                seen.push(
                    span.iter()
                        .map(|m| match m.parts.first() {
                            Some(Part::Text { text }) => text.clone(),
                            _ => String::new(),
                        })
                        .collect(),
                );
                Some(format!("NOTES-{}", seen.len() - 1))
            })
        });

        compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1, 4])),
            CompactDeps {
                explore: Some(scout),
            },
        )
        .await
        .unwrap();

        // One scout per RUN, each seeing only its own run.
        assert_eq!(
            spans.lock().unwrap().clone(),
            vec![vec!["m1".to_string()], vec!["m4".to_string()]]
        );
        assert!(f.llm.prompts()[0].contains("NOTES-0"));
        assert!(!f.llm.prompts()[0].contains("NOTES-1"));
        assert!(f.llm.prompts()[1].contains("NOTES-1"));
        // And the summarizer is told what to do when notes and transcript disagree.
        assert!(f.llm.systems()[0].contains("the notes are right"));
    }

    #[tokio::test]
    async fn no_notes_leaves_the_summarizer_exactly_as_it_was_before_the_scout_existed() {
        let f = scripted_ctx();
        let (source, messages) = conversation(&f, &["a", "b", "c"], SessionOver::default());
        with_db(&f.ctx.db, |d| d.set_session_workspace(&source.id, "/w")).unwrap();

        let branch = compact(
            &f.ctx,
            &source.id,
            &body(picks(&messages, &[1])),
            CompactDeps {
                explore: Some(no_scout()),
            },
        )
        .await
        .unwrap();

        assert_eq!(own_texts(&f, &branch.id), vec!["a", "SUMMARY-0", "c"]);
        assert!(!f.llm.systems()[0].contains("the notes are right"));
        assert!(!f.llm.prompts()[0].contains("Scout notes"));
    }

    // ---- rendering ----------------------------------------------------------

    #[test]
    fn render_span_renders_every_part_kind_and_clips_oversized_tool_output() {
        let m = Message {
            id: "m".into(),
            session_id: "s".into(),
            role: Role::Supervisor,
            pending: false,
            created_at: 0,
            parts: vec![
                Part::Text {
                    text: "said".into(),
                },
                Part::Reasoning {
                    text: "thought".into(),
                    meta: None,
                    model: None,
                },
                Part::ToolCall {
                    id: "c".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "x" }),
                },
                Part::ToolResult {
                    call_id: "c".into(),
                    output: json!("y".repeat(5000)),
                    is_error: false,
                    interrupted: None,
                },
                Part::Image {
                    path: "/a.png".into(),
                    media_type: "image/png".into(),
                    name: "a.png".into(),
                    size: 1,
                },
                Part::Ask {
                    id: "q".into(),
                    question: "which?".into(),
                    options: None,
                    status: AskStatus::Answered,
                    answer: Some("the second one".into()),
                },
            ],
        };

        let rendered = render_span(std::slice::from_ref(&m));
        let lines: Vec<&str> = rendered.split('\n').collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "supervisor: said");
        assert!(lines[2].contains("[tool run_steps]"));
        assert!(
            lines[3].chars().count() < 2100,
            "a 5000-char tool result is clipped"
        );
        assert!(lines[3].ends_with('…'));
        assert!(lines[4].contains("[image a.png]"));
        assert!(lines[5].contains("ask: which? → user answered: the second one"));

        // A message with no parts still contributes a line.
        assert_eq!(
            render_span(&[Message { parts: vec![], ..m }]),
            "supervisor:"
        );
    }
}
