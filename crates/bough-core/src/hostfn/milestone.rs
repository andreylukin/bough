//! `milestone(text)` — one line in the session's LOG.
//!
//! THE DISTINCTION THIS EXISTS FOR: the transcript records everything a
//! session tried; the log records what it ACCOMPLISHED, written by the program
//! at the moment an overarching action landed — a PR opened, tests green, a
//! finding reached, a decision taken, a blocker hit. Nothing else in bough can
//! tell those moments apart from the reads and retries around them, which is
//! why the model writes the line itself (the prompt's `milestone` section
//! says when) instead of a summarizer guessing from tool output.
//!
//! THE INVARIANTS. **Append-only**: a line is never edited or removed, so the
//! log is a record, not a scratchpad (that is `state`). **Bounded**: empty
//! lines are rejected — an empty milestone is a call that should not have
//! happened, and storing it would teach the model that it was fine — and
//! anything over [`MAX_TEXT_CHARS`] is clipped, because a milestone is a
//! headline, not the report. **Cheap and silent on success**: it returns
//! `"ok"`, nothing the program would act on, so it never becomes a control
//! flow primitive.
//!
//! Announced on the bus as `session.milestone` so the rolling summary
//! (`worker/summary`) can count new lines without polling and a sidebar can
//! show them as they land.

use std::sync::Arc;

use crate::errors::{BoughError, ErrorKind};
use crate::schema::events::{EventInput, EventType, SessionMilestoneData};
use crate::types::{Clock, HostFn, TurnCtx};

/// A milestone is a headline. Longer text is clipped, never rejected — the
/// call already happened at the moment that mattered.
pub const MAX_TEXT_CHARS: usize = 300;

/// Injected seams: the clock, for tests that assert on `ts`.
#[derive(Clone, Default)]
pub struct MilestoneDeps {
    pub now: Option<Clock>,
}

/// Normalize the program's text: trim, reject empty, clip to the ceiling.
pub fn normalize(text: &str) -> Result<String, BoughError> {
    let t = text.trim();
    if t.is_empty() {
        return Err(BoughError::http(
            400,
            ErrorKind::BadRequest,
            "milestone(text) needs one line saying what landed — an empty milestone records nothing",
        ));
    }
    let mut out: String = t.chars().take(MAX_TEXT_CHARS).collect();
    if t.chars().count() > MAX_TEXT_CHARS {
        out.push('…');
    }
    // One line: a milestone with a paragraph in it is a report in the wrong slot.
    Ok(out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Write one line for `session_id` and announce it. Pure over its inputs apart
/// from the two side effects it exists for.
pub fn record(ctx: &TurnCtx, deps: &MilestoneDeps, text: &str) -> Result<String, BoughError> {
    let line = normalize(text)?;
    let now: Clock = deps.now.clone().unwrap_or_else(|| ctx.app.now.clone());
    let ts = now();
    {
        let db = ctx.app.db.lock().unwrap();
        db.add_milestone(&ctx.session_id, ts, &line)?;
    }
    ctx.app.bus.publish(EventInput {
        r#type: EventType::SessionMilestone,
        session_id: Some(ctx.session_id.clone()),
        data: serde_json::to_value(SessionMilestoneData {
            session_id: ctx.session_id.clone(),
            ts,
            text: line,
        })
        .unwrap_or_default(),
    });
    Ok("ok".to_string())
}

/// The bridged form: `milestone(text)` → `"ok"`.
pub fn create_milestone_host_fn(ctx: &TurnCtx, deps: MilestoneDeps) -> HostFn {
    let ctx = ctx.clone();
    Arc::new(move |args: Vec<String>| {
        let ctx = ctx.clone();
        let deps = deps.clone();
        let text = args.first().cloned().unwrap_or_default();
        Box::pin(async move { record(&ctx, &deps, &text) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testkit::{seed_session, shared_db, turn_ctx_for, SeedOpts};
    use crate::schema::events::BoughEvent;
    use std::sync::Mutex;

    fn fixture() -> (TurnCtx, Arc<Mutex<Vec<BoughEvent>>>) {
        let db = shared_db();
        let session = seed_session(&db, SeedOpts::default());
        let ctx = turn_ctx_for(&db, &session.id, "turn-1", 0);
        let seen: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = seen.clone();
        ctx.app.bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.clone());
        }));
        (ctx, seen)
    }

    #[tokio::test]
    async fn a_milestone_is_stored_in_order_and_announced() {
        let (ctx, seen) = fixture();
        let deps = MilestoneDeps {
            now: Some(Arc::new(|| 5_000)),
        };
        let f = create_milestone_host_fn(&ctx, deps.clone());
        assert_eq!(f(vec!["  Opened PR #34 ".into()]).await.unwrap(), "ok");
        assert_eq!(f(vec!["Tests green".into()]).await.unwrap(), "ok");
        let log = ctx.app.db.lock().unwrap().milestones(&ctx.session_id).unwrap();
        assert_eq!(
            log.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["Opened PR #34", "Tests green"]
        );
        assert!(log.iter().all(|m| m.ts == 5_000));
        let events = seen.lock().unwrap();
        let mine: Vec<&BoughEvent> = events
            .iter()
            .filter(|e| e.r#type == EventType::SessionMilestone)
            .collect();
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].data["text"], "Opened PR #34");
        assert_eq!(mine[0].session_id.as_deref(), Some(ctx.session_id.as_str()));
    }

    #[tokio::test]
    async fn empty_text_is_rejected_and_nothing_is_written() {
        let (ctx, seen) = fixture();
        let f = create_milestone_host_fn(&ctx, MilestoneDeps::default());
        let err = f(vec!["   \n".into()]).await.unwrap_err();
        assert!(err.to_string().contains("empty milestone"), "{err}");
        assert!(ctx.app.db.lock().unwrap().milestones(&ctx.session_id).unwrap().is_empty());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn long_text_is_clipped_and_flattened_to_one_line() {
        let long = "x".repeat(MAX_TEXT_CHARS + 50);
        let out = normalize(&long).unwrap();
        assert_eq!(out.chars().count(), MAX_TEXT_CHARS + 1);
        assert!(out.ends_with('…'));
        assert_eq!(normalize("a\n  b\t c").unwrap(), "a b c");
    }
}
