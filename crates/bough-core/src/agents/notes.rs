//! Harness-injected system notes, and the wake rule that decides when one
//! costs a turn (port of `src/agents/notes.ts`).
//!
//! THE INVARIANT THIS HOLDS: **a note reaches the session exactly once, and
//! never as a second concurrent turn.** Every post lands in one of two states,
//! no third: spawner idle → the note starts a fresh turn; a turn in flight →
//! the note is persisted + announced immediately and rides the queued drain
//! (`has_unanswered_input` finds it — the note is **persisted before it is
//! decided upon**, so the queue derivation reads it from the DB, restart-safe).
//!
//! Two deliberate non-wakes: (1) **a stop stays stopped** — if the session's
//! own last finished turn ended `interrupted`, record without waking (a user
//! stop cascades into detached children; their completion notes must not
//! restart the stopped work); (2) **boot recovery** — orphaned-subagent notes
//! are recorded, never woken.
//!
//! The starter is read off the ctx (`AppCtx::turn_starter`), the same seam
//! boot wires for posted user messages — this module never constructs a
//! second, subtly different turn. The note format below is VERBATIM product
//! surface: the TUI parses it and the model keys off it.

use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use uuid::Uuid;

use crate::errors::BoughError;
use crate::schema::events::{EventInput, EventType};
use crate::schema::parts::{Message, Part, Role, Session, SessionKind, TurnStatus};
use crate::turn::queue::TurnRegistry;
use crate::turn::runner::ReportError;
use crate::turn::state::OrphanedTurn;
use crate::types::{AppCtx, Clock, TurnCtx};

use super::subagent::{build_result, SubagentResult, SubagentStatus};
use super::with_db;

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// The future a wake spawns; its rejection is reported, never thrown.
pub type StartFuture = BoxFuture<'static, Result<(), BoughError>>;

/// How a session's turn is started.
///
/// The sync-error-vs-async-rejection distinction of the TS `wakeFor` is
/// preserved in the signature: a synchronous `Err` means "a turn claimed the
/// session between the check and this call" (→ the note rides the queue); an
/// `Ok` future that later rejects is only reported. The production default
/// adapts `AppCtx::turn_starter`, which never fails synchronously.
pub type NoteStarter =
    Arc<dyn Fn(&AppCtx, &Session, &Message) -> Result<StartFuture, BoughError> + Send + Sync>;

/// What the post did about the note, beyond persisting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeOutcome {
    /// The session was idle; a fresh turn was asked for.
    Started,
    /// A turn is in flight; the note drains into the next one.
    Queued,
    /// Written and announced, waking nothing (interrupt / boot recovery / no
    /// starter wired — an unwired seam degrades to "read next turn", never a
    /// lost note).
    Recorded,
    /// No such session. Nothing was written.
    Dropped,
}

/// What a post reports back. Tests assert on it; production ignores it.
#[derive(Clone, Debug)]
pub struct NoteDelivery {
    /// The persisted note, or `None` when the session was gone.
    pub message: Option<Message>,
    pub wake: WakeOutcome,
}

/// `Auto` applies the wake rule; `Never` records the note and wakes nothing
/// (boot recovery).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WakeMode {
    #[default]
    Auto,
    Never,
}

#[derive(Clone, Default)]
pub struct NoteDeps {
    /// Absent = the ctx's registry, which is what the turn runner defaults to.
    pub registry: Option<Arc<TurnRegistry>>,
    /// Injected clock. Absent = `ctx.now`.
    pub now: Option<Clock>,
    /// Absent = `ctx.turn_starter()`. Absent there too = recorded, not woken.
    pub start: Option<NoteStarter>,
    pub wake: WakeMode,
    /// Extra parts riding with the note (`image()` attaches pictures).
    pub extra: Vec<Part>,
    /// Where a failed wake is reported. Tests pass a collector.
    pub report_error: Option<ReportError>,
}

// ---------------------------------------------------------------------------
// The post
// ---------------------------------------------------------------------------

/// Persist a system note into `session_id`, announce it, and apply the wake
/// rule.
///
/// **This never fails.** Every caller is a completion callback — a child's
/// result pipeline, a shell's exit handler, a comment POST — with no
/// round-trip to report a failure on. A session that has gone missing is
/// reported as `Dropped` and the note is not written (the row would fail the
/// foreign key anyway).
pub fn post_system_note(
    ctx: &AppCtx,
    session_id: &str,
    text: &str,
    deps: &NoteDeps,
) -> NoteDelivery {
    let session = match with_db(&ctx.db, |d| d.get_session(session_id)) {
        Ok(Some(s)) => s,
        _ => {
            return NoteDelivery {
                message: None,
                wake: WakeOutcome::Dropped,
            }
        }
    };

    let now: Clock = deps.now.clone().unwrap_or_else(|| ctx.now.clone());
    let mut parts = vec![Part::Text {
        text: text.to_string(),
    }];
    parts.extend(deps.extra.iter().cloned());
    let message = match with_db(&ctx.db, |d| {
        d.create_message(Message {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            role: Role::System,
            parts,
            // Complete when it lands: a note left pending is a session the UI
            // shows as busy forever.
            pending: false,
            created_at: now(),
        })
    }) {
        Ok(m) => m,
        Err(err) => {
            tracing::error!("failed to write a system note into {session_id}: {err}");
            return NoteDelivery {
                message: None,
                wake: WakeOutcome::Dropped,
            };
        }
    };
    index_quietly(ctx, &message);
    ctx.bus.publish(EventInput {
        r#type: EventType::MessageStarted,
        session_id: Some(session_id.to_string()),
        data: serde_json::to_value(&message).unwrap_or_default(),
    });

    let wake = wake_for(ctx, &session, &message, deps);
    NoteDelivery {
        message: Some(message),
        wake,
    }
}

/// The wake rule, and the only place it is decided.
///
/// Order matters. The busy check comes first because it is the one that must
/// never be wrong, and the registry — not the database — is the authority on
/// it (a turn claims the session synchronously in `begin_turn`, before its
/// row exists).
fn wake_for(ctx: &AppCtx, session: &Session, message: &Message, deps: &NoteDeps) -> WakeOutcome {
    if deps.wake == WakeMode::Never {
        return WakeOutcome::Recorded;
    }

    let registry = deps
        .registry
        .clone()
        .unwrap_or_else(|| ctx.turn_registry.clone());
    if registry.is_running(&session.id) {
        // The derived check would find this note on its own. The explicit
        // nudge is belt and braces for the case the derivation cannot see: a
        // turn that has not yet written its supervisor placeholder.
        registry.enqueue(&session.id);
        return WakeOutcome::Queued;
    }

    if ended_on_an_interrupt(ctx, &session.id) {
        return WakeOutcome::Recorded;
    }

    let start: Option<NoteStarter> = deps.start.clone().or_else(|| {
        ctx.turn_starter().map(|starter| -> NoteStarter {
            Arc::new(move |c: &AppCtx, s: &Session, m: &Message| {
                starter.start_turn(c, s, m);
                Ok(futures::future::ready(Ok(())).boxed())
            })
        })
    });
    let Some(start) = start else {
        return WakeOutcome::Recorded;
    };

    let report: ReportError = deps.report_error.clone().unwrap_or_else(|| {
        Arc::new(|err: &BoughError, id: &str| {
            tracing::error!("failed to wake session {id} with a note: {err}");
        })
    });
    match start(ctx, session, message) {
        Ok(started) => {
            let sid = session.id.clone();
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        if let Err(err) = started.await {
                            report(&err, &sid);
                        }
                    });
                }
                Err(_) => report(
                    &BoughError::bad_request("no async runtime to run the woken turn on"),
                    &sid,
                ),
            }
            WakeOutcome::Started
        }
        Err(err) => {
            // A turn claimed the session between the check above and this
            // call. The note is already persisted, so the running turn's
            // drain will find it — mark the nudge and say so rather than
            // losing the report to a race.
            registry.enqueue(&session.id);
            report(&err, &session.id);
            WakeOutcome::Queued
        }
    }
}

/// Did this session's last turn end because the user stopped it?
///
/// KNOWN WINDOW (accepted): a note landing while the interrupted turn is
/// still winding down takes the `Queued` path and drains once — closing it is
/// `turn/queue`'s call, not this module's.
fn ended_on_an_interrupt(ctx: &AppCtx, session_id: &str) -> bool {
    with_db(&ctx.db, |d| d.turns_for_session(session_id))
        .ok()
        .and_then(|turns| turns.last().map(|t| t.status))
        == Some(TurnStatus::Interrupted)
}

// ---------------------------------------------------------------------------
// A detached subagent's report
// ---------------------------------------------------------------------------

/// The marker the UI and the model both key off. Stable text, not decoration.
pub const SUBAGENT_NOTE_PREFIX: &str = "[subagent finished]";

/// How the child ended, in words the parent can act on. Four distinct
/// outcomes, four distinct first lines; each failure says what survived,
/// because a subagent works in the SAME checkout.
fn status_text(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Done => "finished",
        SubagentStatus::Error => {
            "FAILED — its turn errored, and the report below carries the error. Nothing \
             retried it. Whatever it had already written is in the checkout"
        }
        SubagentStatus::Interrupted => {
            "STOPPED — it was interrupted (a user stop, or it hit its wall-clock \
             limit). Whatever it had already written is in the checkout"
        }
        SubagentStatus::Orphaned => {
            "ORPHANED — the server restarted before it finished. Whatever it had \
             already written is in the checkout"
        }
    }
}

/// The note a detached child's report becomes. VERBATIM — the TUI parses it.
///
/// The last line is not filler: the single most common wrong move after a
/// delegated report is looking for the merge step, and there isn't one.
/// "not reported" rather than "none" for an empty file list — "none" would be
/// a claim the harness cannot back.
pub fn format_subagent_note(result: &SubagentResult) -> String {
    let files = if result.changed_files.is_empty() {
        "not reported".to_string()
    } else {
        result.changed_files.join(", ")
    };
    [
        format!(
            "{SUBAGENT_NOTE_PREFIX} \"{}\" ({}) — {}.",
            result.title,
            result.session_id,
            status_text(result.status)
        ),
        format!("Changed files: {files}."),
        if result.report.is_empty() {
            "No report.".to_string()
        } else {
            format!("Report:\n{}", result.report)
        },
        "It worked in THIS session's checkout, so its edits are already here — read them \
         before building on top; there is nothing to merge."
            .to_string(),
    ]
    .join("\n")
}

/// Deliver an unclaimed detached result to its spawner.
///
/// The ctx is the SPAWNING turn's, so `ctx.session_id` is the spawner.
/// Claimed (`join()`ed) results never reach here — `hostfn/delegate` checks
/// first, because a joined report already went back in-band.
pub fn deliver_subagent_note(
    ctx: &TurnCtx,
    result: &SubagentResult,
    deps: &NoteDeps,
) -> NoteDelivery {
    post_system_note(
        &ctx.app,
        &ctx.session_id,
        &format_subagent_note(result),
        deps,
    )
}

/// The `deliver` seam `hostfn/delegate` takes, deps bound.
pub fn create_note_deliverer(
    deps: NoteDeps,
) -> Arc<dyn Fn(&TurnCtx, &SubagentResult) + Send + Sync> {
    Arc::new(move |ctx, result| {
        deliver_subagent_note(ctx, result, &deps);
    })
}

// ---------------------------------------------------------------------------
// Background job exits
// ---------------------------------------------------------------------------

/// The poster the job registry calls when a background shell exits. The
/// registry formats its own text and posts through here so a job exit and a
/// subagent report obey exactly one wake rule.
pub fn create_job_notifier(ctx: AppCtx, deps: NoteDeps) -> Arc<dyn Fn(&str, &str) + Send + Sync> {
    Arc::new(move |session_id, text| {
        post_system_note(&ctx, session_id, text, &deps);
    })
}

// ---------------------------------------------------------------------------
// Boot recovery: the child a restart stranded
// ---------------------------------------------------------------------------

/// Tell a spawner that one of its subagents was orphaned by a restart.
///
/// Recorded, never woken — a restarting server must not spend tokens on
/// sessions nobody has returned to. Returns `None` for an orphan that owes
/// nobody a note (not a subagent, or no origin edge).
pub async fn note_orphaned_subagent(
    ctx: &AppCtx,
    orphan: &OrphanedTurn,
    deps: &NoteDeps,
) -> Result<Option<NoteDelivery>, BoughError> {
    let child = with_db(&ctx.db, |d| d.get_session(&orphan.session_id))?;
    let Some(child) = child else { return Ok(None) };
    if child.kind != SessionKind::Subagent {
        return Ok(None);
    }
    let Some(origin_id) = child.origin_id.clone() else {
        return Ok(None);
    };
    let result = build_result(
        &ctx.db,
        &orphan.session_id,
        &orphan.message_id,
        None,
        Default::default(),
    )
    .await;
    let mut never = deps.clone();
    never.wake = WakeMode::Never;
    Ok(Some(post_system_note(
        ctx,
        &origin_id,
        &format_subagent_note(&result),
        &never,
    )))
}

/// The whole recovered batch. One failure must not abandon the rest — a
/// spawner that cannot be notified is a bad outcome, and every *other*
/// spawner losing its notice as well is a worse one.
pub async fn note_orphaned_subagents(
    ctx: &AppCtx,
    orphans: &[OrphanedTurn],
    deps: &NoteDeps,
) -> Vec<NoteDelivery> {
    let mut posted = vec![];
    for orphan in orphans {
        match note_orphaned_subagent(ctx, orphan, deps).await {
            Ok(Some(delivery)) => posted.push(delivery),
            Ok(None) => {}
            Err(err) => match &deps.report_error {
                Some(report) => report(&err, &orphan.session_id),
                None => tracing::error!(
                    "failed to note the orphaned subagent {}: {err}",
                    orphan.session_id
                ),
            },
        }
    }
    posted
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A note that fails to index is a degraded search, never a lost note — and
/// never a thrown completion callback.
fn index_quietly(ctx: &AppCtx, message: &Message) {
    if let Err(err) = with_db(&ctx.db, |d| d.index_message(message)) {
        tracing::error!("failed to index system note {}: {err}", message.id);
    }
}

// ---------------------------------------------------------------------------
// Tests — port of `src/agents/notes.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testkit::{
        gated_llm, recording_llm, seed_idle_session, seed_spawner, spawner_turn_ctx, until,
        watch_turns, AgentsFixture,
    };
    use crate::schema::parts::Turn;
    use crate::turn::queue::has_unanswered_input;
    use crate::turn::runner::{begin_turn, create_turn_starter, TurnDeps};
    use crate::turn::state::{recover_orphaned_turns, RecoverOptions};
    use crate::turn::testkit::stub_deps;
    use std::sync::Mutex;
    use uuid::Uuid;

    /// A result as `build_result` would have assembled it.
    fn result_with(f: impl FnOnce(&mut SubagentResult)) -> SubagentResult {
        let mut r = SubagentResult {
            session_id: "child-1".to_string(),
            title: "seatbelt audit".to_string(),
            ok: true,
            status: SubagentStatus::Done,
            report: "Checked every handler; two were missing error paths.".to_string(),
            changed_files: vec![],
        };
        f(&mut r);
        r
    }

    fn deps_of(f: &AgentsFixture) -> NoteDeps {
        NoteDeps {
            registry: Some(f.registry.clone()),
            ..Default::default()
        }
    }

    /// The fixture's turn deps, wired the way boot wires them (starter on ctx).
    fn wire_starter(f: &AgentsFixture) -> TurnDeps {
        let deps = TurnDeps {
            registry: Some(f.registry.clone()),
            ..stub_deps()
        };
        *f.ctx.starter.write().unwrap() = Some(create_turn_starter(deps.clone()));
        deps
    }

    fn system_notes(f: &AgentsFixture, session_id: &str) -> Vec<Message> {
        with_db(&f.db, |d| d.messages_for(session_id))
            .unwrap()
            .into_iter()
            .filter(|m| m.role == Role::System)
            .collect()
    }

    fn first_text(m: &Message) -> String {
        match &m.parts[0] {
            Part::Text { text } => text.clone(),
            other => panic!("expected text part, got {other:?}"),
        }
    }

    // ---- the note itself ----------------------------------------------------

    #[test]
    fn the_four_subagent_outcomes_read_differently_in_the_note() {
        let done = format_subagent_note(&result_with(|_| {}));
        assert!(
            done.starts_with(r#"[subagent finished] "seatbelt audit" (child-1) — finished."#),
            "{done}"
        );
        assert!(done.contains("Report:\nChecked every handler"));
        // The most common wrong move after a delegated report is looking for
        // the merge.
        assert!(done.contains("already here"));
        assert!(done.contains("nothing to merge"));

        let errored = format_subagent_note(&result_with(|r| {
            r.ok = false;
            r.status = SubagentStatus::Error;
        }));
        let stopped = format_subagent_note(&result_with(|r| {
            r.ok = false;
            r.status = SubagentStatus::Interrupted;
        }));
        let orphaned = format_subagent_note(&result_with(|r| {
            r.ok = false;
            r.status = SubagentStatus::Orphaned;
        }));

        assert!(errored.contains("FAILED — its turn errored"));
        assert!(stopped.contains("STOPPED — it was interrupted"));
        assert!(orphaned.contains("ORPHANED — the server restarted"));

        // Distinguishable is the requirement: four outcomes, four different
        // first lines.
        let heads: std::collections::HashSet<&str> = [&done, &errored, &stopped, &orphaned]
            .iter()
            .map(|n| n.split('\n').next().unwrap())
            .collect();
        assert_eq!(heads.len(), 4);

        // Each failure says what survived.
        for note in [&errored, &stopped, &orphaned] {
            assert!(
                note.contains("already written is in the checkout"),
                "{note}"
            );
        }

        assert!(format_subagent_note(&result_with(|_| {})).contains("Changed files: not reported."));
        assert!(format_subagent_note(&result_with(|r| {
            r.changed_files = vec!["a.ts".to_string(), "b.ts".to_string()];
        }))
        .contains("Changed files: a.ts, b.ts."));
    }

    // ---- wake path 1: the idle spawner --------------------------------------

    #[tokio::test]
    async fn a_note_into_an_idle_session_starts_a_fresh_turn_and_reaches_the_model() {
        let f = AgentsFixture::new();
        let watch = watch_turns(&f.ctx.bus);
        let session = seed_idle_session(&f);
        let llm = recording_llm("acknowledged");
        let f = f.with_llm(llm.clone());
        wire_starter(&f);

        let delivery = post_system_note(
            &f.ctx,
            &session.id,
            &format_subagent_note(&result_with(|r| {
                r.report = "the audit found two gaps".into()
            })),
            &deps_of(&f),
        );
        assert_eq!(
            delivery.wake,
            WakeOutcome::Started,
            "an idle spawner is woken"
        );

        until(|| watch.turns_for(&session.id) >= 1, "the woken turn").await;
        until(
            || !f.registry.is_running(&session.id),
            "the woken turn to finish",
        )
        .await;
        assert_eq!(watch.turns_for(&session.id), 1, "exactly one fresh turn");

        // The note is in the spawner's own thread, as a system message…
        let notes = system_notes(&f, &session.id);
        assert_eq!(notes.len(), 1, "the report landed as a system-role message");
        assert!(first_text(&notes[0]).starts_with("[subagent finished]"));
        assert!(first_text(&notes[0]).contains("the audit found two gaps"));

        // …and it reached the model, which is the only reason to wake at all.
        let calls = llm.calls();
        let last = serde_json::to_string(&calls.last().unwrap().messages).unwrap();
        assert!(last.contains("subagent finished"), "{last}");

        assert!(
            watch.violations().is_empty(),
            "no session ever ran two turns at once"
        );
    }

    // ---- wake path 2: the busy spawner --------------------------------------

    #[tokio::test]
    async fn a_note_that_lands_mid_turn_rides_the_queued_drain_instead_of_racing_it() {
        let f = AgentsFixture::new();
        let watch = watch_turns(&f.ctx.bus);
        let session = seed_idle_session(&f);
        let (llm, release, mut started) = gated_llm("first turn's answer");
        let f = f.with_llm(llm);
        let deps = TurnDeps {
            registry: Some(f.registry.clone()),
            ..stub_deps()
        };
        *f.ctx.starter.write().unwrap() = Some(create_turn_starter(deps.clone()));

        let first = begin_turn(&f.ctx, &session.id, deps).unwrap();
        started.changed().await.unwrap();
        assert!(
            f.registry.is_running(&session.id),
            "a turn is provably in flight"
        );

        let delivery = post_system_note(
            &f.ctx,
            &session.id,
            &format_subagent_note(&result_with(|r| r.session_id = "child-9".into())),
            &deps_of(&f),
        );
        assert_eq!(
            delivery.wake,
            WakeOutcome::Queued,
            "a busy session never gets a second turn"
        );
        assert_eq!(
            watch.turns_for(&session.id),
            1,
            "and none was started behind its back"
        );
        // Persisted and announced immediately all the same.
        let note = delivery.message.unwrap();
        assert_eq!(
            with_db(&f.db, |d| d.get_message(&note.id))
                .unwrap()
                .unwrap()
                .role,
            Role::System
        );

        release();
        let _ = first.done.await;

        // The drain: the running turn ends and the note it queued behind
        // becomes the next turn — one, not one per note and not none.
        until(
            || watch.turns_for(&session.id) == 2,
            "the queued drain to start a turn",
        )
        .await;
        until(
            || !f.registry.is_running(&session.id),
            "the drained turn to finish",
        )
        .await;
        assert_eq!(watch.turns_for(&session.id), 2);

        let roles: Vec<Role> = with_db(&f.db, |d| d.messages_for(&session.id))
            .unwrap()
            .iter()
            .map(|m| m.role)
            .collect();
        assert_eq!(
            roles,
            vec![Role::User, Role::Supervisor, Role::System, Role::Supervisor],
            "ordered, nothing lost"
        );
        assert!(watch.violations().is_empty());
    }

    #[tokio::test]
    async fn a_burst_of_notes_on_a_busy_session_drains_into_exactly_one_turn() {
        let f = AgentsFixture::new();
        let watch = watch_turns(&f.ctx.bus);
        let session = seed_idle_session(&f);
        let (llm, release, mut started) = gated_llm("first turn's answer");
        let f = f.with_llm(llm);
        let deps = TurnDeps {
            registry: Some(f.registry.clone()),
            ..stub_deps()
        };
        *f.ctx.starter.write().unwrap() = Some(create_turn_starter(deps.clone()));

        let first = begin_turn(&f.ctx, &session.id, deps).unwrap();
        started.changed().await.unwrap();

        // Four children finishing at once is the ordinary shape of a fan-out.
        let wakes: Vec<WakeOutcome> = (1..=4)
            .map(|n| {
                post_system_note(
                    &f.ctx,
                    &session.id,
                    &format!("{SUBAGENT_NOTE_PREFIX} child {n}"),
                    &deps_of(&f),
                )
                .wake
            })
            .collect();
        assert_eq!(wakes, vec![WakeOutcome::Queued; 4]);

        release();
        let _ = first.done.await;
        until(|| watch.turns_for(&session.id) == 2, "one drained turn").await;
        until(|| !f.registry.is_running(&session.id), "it to finish").await;

        assert_eq!(
            watch.turns_for(&session.id),
            2,
            "four notes, one turn — not four"
        );
        assert_eq!(system_notes(&f, &session.id).len(), 4);
        assert!(watch.violations().is_empty());
    }

    #[tokio::test]
    async fn a_burst_of_notes_on_an_idle_session_also_produces_exactly_one_turn() {
        let f = AgentsFixture::new();
        let watch = watch_turns(&f.ctx.bus);
        let session = seed_idle_session(&f);
        let f = f.with_llm(recording_llm("acknowledged"));
        wire_starter(&f);

        // The first note finds the session idle and starts a turn
        // SYNCHRONOUSLY — the registry is claimed inside `begin_turn` before
        // it returns — so the second and third already see a busy session.
        let wakes: Vec<WakeOutcome> = (1..=3)
            .map(|n| {
                post_system_note(
                    &f.ctx,
                    &session.id,
                    &format!("{SUBAGENT_NOTE_PREFIX} child {n}"),
                    &deps_of(&f),
                )
                .wake
            })
            .collect();
        assert_eq!(
            wakes,
            vec![
                WakeOutcome::Started,
                WakeOutcome::Queued,
                WakeOutcome::Queued
            ]
        );

        until(
            || watch.turns_for(&session.id) == 2,
            "the drain for the two queued notes",
        )
        .await;
        until(
            || !f.registry.is_running(&session.id),
            "the drained turn to finish",
        )
        .await;

        assert_eq!(watch.turns_for(&session.id), 2);
        assert!(
            watch.violations().is_empty(),
            "no session ever ran two turns at once"
        );
    }

    // ---- the two deliberate non-wakes ---------------------------------------

    #[tokio::test]
    async fn a_stop_stays_stopped_a_note_into_an_interrupted_session_wakes_nothing() {
        let f = AgentsFixture::new();
        let watch = watch_turns(&f.ctx.bus);
        let session = seed_idle_session(&f);
        // The session's last turn ended because the user stopped it — which is
        // also what cascade-stopped the detached child whose note arrives now.
        let message = with_db(&f.db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session.id.clone(),
                role: Role::Supervisor,
                parts: vec![Part::Text {
                    text: "⏹ Stopped.".to_string(),
                }],
                pending: false,
                created_at: 1_002,
            })
        })
        .unwrap();
        with_db(&f.db, |d| {
            d.create_turn(Turn {
                id: Uuid::new_v4().to_string(),
                session_id: session.id.clone(),
                message_id: message.id.clone(),
                status: TurnStatus::Interrupted,
                step: "ended".to_string(),
                created_at: 1_002,
                updated_at: 1_003,
                error: None,
                usage: None,
            })
        })
        .unwrap();
        let f = f.with_llm(recording_llm("acknowledged"));
        wire_starter(&f);

        let delivery = post_system_note(
            &f.ctx,
            &session.id,
            &format_subagent_note(&result_with(|r| {
                r.ok = false;
                r.status = SubagentStatus::Interrupted;
            })),
            &deps_of(&f),
        );

        assert_eq!(
            delivery.wake,
            WakeOutcome::Recorded,
            "the stop is still in force"
        );
        assert!(
            delivery.message.is_some(),
            "but the report is still written into the thread"
        );
        assert_eq!(
            watch.turns_for(&session.id),
            0,
            "nothing restarted the stopped work"
        );
        assert!(watch.violations().is_empty());
    }

    #[tokio::test]
    async fn a_note_for_a_session_that_is_gone_is_dropped_never_thrown() {
        let f = AgentsFixture::new();
        let f = f.with_llm(recording_llm("x"));
        wire_starter(&f);
        let delivery = post_system_note(
            &f.ctx,
            "no-such-session",
            &format!("{SUBAGENT_NOTE_PREFIX} ghost"),
            &deps_of(&f),
        );
        assert!(delivery.message.is_none());
        assert_eq!(delivery.wake, WakeOutcome::Dropped);
    }

    // ---- the sync-throw → queued arm ----------------------------------------

    #[tokio::test]
    async fn a_starter_that_fails_synchronously_queues_the_note_instead_of_losing_it() {
        let f = AgentsFixture::new();
        let session = seed_idle_session(&f);
        let f = f.with_llm(recording_llm("x"));

        let reported: Arc<Mutex<Vec<BoughError>>> = Arc::new(Mutex::new(vec![]));
        let sink = reported.clone();
        let mut deps = deps_of(&f);
        // A turn claimed the session between the check and the call.
        deps.start = Some(Arc::new(|_c, s, _m| {
            Err(BoughError::http(
                500,
                crate::errors::ErrorKind::Turn,
                format!("a turn is already running for session {}", s.id),
            ))
        }));
        deps.report_error = Some(Arc::new(move |err, _sid| {
            sink.lock().unwrap().push(err.clone())
        }));

        let delivery = post_system_note(&f.ctx, &session.id, "late note", &deps);
        assert_eq!(
            delivery.wake,
            WakeOutcome::Queued,
            "persisted, nudged, never lost"
        );
        assert_eq!(
            reported.lock().unwrap().len(),
            1,
            "and the race is reported"
        );
        assert!(f.registry.drain(&session.id), "the drain nudge is armed");
        // The derived queue would find it anyway: the note is in the DB.
        assert!(with_db(&f.db, |d| has_unanswered_input(d, &session.id)).unwrap());
    }

    #[tokio::test]
    async fn a_starter_whose_future_rejects_is_reported_only() {
        let f = AgentsFixture::new();
        let session = seed_idle_session(&f);
        let f = f.with_llm(recording_llm("x"));

        let reported: Arc<Mutex<Vec<BoughError>>> = Arc::new(Mutex::new(vec![]));
        let sink = reported.clone();
        let mut deps = deps_of(&f);
        deps.start = Some(Arc::new(|_c, _s, _m| {
            Ok(async { Err(BoughError::bad_request("async wake failure")) }.boxed())
        }));
        deps.report_error = Some(Arc::new(move |err, _sid| {
            sink.lock().unwrap().push(err.clone())
        }));

        let delivery = post_system_note(&f.ctx, &session.id, "a note", &deps);
        assert_eq!(delivery.wake, WakeOutcome::Started);
        until(
            || !reported.lock().unwrap().is_empty(),
            "the async rejection to be reported",
        )
        .await;
        assert!(
            !f.registry.drain(&session.id),
            "an async rejection queues nothing"
        );
    }

    // ---- the failure matrix: orphaned by a restart --------------------------

    #[tokio::test]
    async fn a_child_orphaned_by_a_restart_reaches_its_spawner_without_waking_it() {
        let f = AgentsFixture::new();
        let watch = watch_turns(&f.ctx.bus);
        let spawner = seed_idle_session(&f);
        let spawner_message = with_db(&f.db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: spawner.id.clone(),
                role: Role::Supervisor,
                parts: vec![Part::Text {
                    text: "spawning the audit".to_string(),
                }],
                pending: false,
                created_at: 1_002,
            })
        })
        .unwrap();
        // What the previous process left behind: a subagent branch whose turn
        // row still says `running`, and a detached register that died with it.
        let child = with_db(&f.db, |d| {
            d.create_session(Session {
                id: Uuid::new_v4().to_string(),
                title: "seatbelt audit".to_string(),
                kind: SessionKind::Subagent,
                created_at: 1_003,
                parent_id: None,
                origin_id: Some(spawner.id.clone()),
                origin_message_id: Some(spawner_message.id.clone()),
                workspace: Some("/tmp/checkout".to_string()),
                origin_dir: Some("/tmp/checkout".to_string()),
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
        .unwrap();
        let child_message = with_db(&f.db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: child.id.clone(),
                role: Role::Supervisor,
                parts: vec![],
                pending: true,
                created_at: 1_004,
            })
        })
        .unwrap();
        with_db(&f.db, |d| {
            d.create_turn(Turn {
                id: Uuid::new_v4().to_string(),
                session_id: child.id.clone(),
                message_id: child_message.id.clone(),
                status: TurnStatus::Running,
                step: "round:2".to_string(),
                created_at: 1_004,
                updated_at: 1_005,
                error: None,
                usage: None,
            })
        })
        .unwrap();
        let f = f.with_llm(recording_llm("x"));
        wire_starter(&f);

        let orphans = {
            let guard = f.db.lock().unwrap();
            recover_orphaned_turns(&*guard, f.ctx.bus.as_ref(), RecoverOptions::default()).unwrap()
        };
        let posted = note_orphaned_subagents(&f.ctx, &orphans, &deps_of(&f)).await;

        assert_eq!(
            posted.len(),
            1,
            "the stranded child owes its spawner exactly one note"
        );
        let note = &posted[0];
        let message = note.message.as_ref().unwrap();
        assert_eq!(
            message.session_id, spawner.id,
            "posted to the SPAWNER, not the child"
        );
        let text = first_text(message);
        assert!(text.contains("ORPHANED — the server restarted"), "{text}");
        assert!(
            text.contains(&child.id),
            "names the branch, so the user can open it"
        );

        // Recorded, not woken: recovery surfaces a restart, never resumes it.
        assert_eq!(note.wake, WakeOutcome::Recorded);
        assert_eq!(watch.turns_for(&spawner.id), 0);
        assert!(!f.registry.is_running(&spawner.id));
        assert!(watch.violations().is_empty());
    }

    #[tokio::test]
    async fn an_orphan_that_owes_nobody_yields_no_note() {
        let f = AgentsFixture::new();
        let root = seed_idle_session(&f);
        // A root's own orphaned turn: not a subagent, no note owed.
        let orphan = OrphanedTurn {
            turn_id: "t1".to_string(),
            session_id: root.id.clone(),
            message_id: "m1".to_string(),
            step: "round:1".to_string(),
            closed_message: false,
        };
        let posted = note_orphaned_subagents(&f.ctx, &[orphan], &deps_of(&f)).await;
        assert!(posted.is_empty());
    }

    // ---- the production seams -----------------------------------------------

    #[tokio::test]
    async fn create_note_deliverer_is_the_deliver_seam_delegate_takes() {
        let f = AgentsFixture::new();
        let watch = watch_turns(&f.ctx.bus);
        let seeded = seed_spawner(&f);
        let f = f.with_llm(recording_llm("acknowledged"));
        wire_starter(&f);
        let deliver = create_note_deliverer(deps_of(&f));

        // The shape delegate passes it: a turn ctx and the child's result.
        let ctx = spawner_turn_ctx(&f, &seeded, recording_llm("unused"));
        deliver(&ctx, &result_with(|r| r.title = "the audit".into()));

        let notes = system_notes(&f, &seeded.session.id);
        assert_eq!(notes.len(), 1, "the seam posts into the spawner's session");
        assert!(first_text(&notes[0]).contains(r#"[subagent finished] "the audit""#));

        until(
            || !f.registry.is_running(&seeded.session.id),
            "the woken turn to finish",
        )
        .await;
        assert_eq!(watch.turns_for(&seeded.session.id), 1);
        assert!(watch.violations().is_empty());
    }

    #[tokio::test]
    async fn a_background_jobs_exit_posts_through_the_same_wake_rule() {
        let f = AgentsFixture::new();
        let watch = watch_turns(&f.ctx.bus);
        let session = seed_idle_session(&f);
        let f = f.with_llm(recording_llm("acknowledged"));
        wire_starter(&f);

        let notify = create_job_notifier(f.ctx.clone(), deps_of(&f));
        notify(
            &session.id,
            "[background] bg_1 \"failing job\" finished (exit 3), 0 lines",
        );

        let notes = system_notes(&f, &session.id);
        assert_eq!(notes.len(), 1);
        assert!(first_text(&notes[0]).starts_with("[background] bg_1 \"failing job\""));

        // It woke the idle session exactly once — the same rule the subagent
        // note obeys, because both go through `post_system_note`.
        until(|| watch.turns_for(&session.id) == 1, "the woken turn").await;
        until(
            || !f.registry.is_running(&session.id),
            "the woken turn to finish",
        )
        .await;
        assert_eq!(watch.turns_for(&session.id), 1);
        assert!(watch.violations().is_empty());
    }
}
