//! `step(type, content)` — one typed line on a mind's TRAJECTORY.
//!
//! THE DISTINCTION THIS EXISTS FOR: the transcript records everything a
//! wakeup tried; the trajectory records the narrative — what the mind
//! thought, noticed, resolved, learned, or chose to let pass. The model
//! writes the step itself at the moment it happens (the prompt's mind
//! section says when), because nothing downstream can tell a thought from
//! the tool traffic around it.
//!
//! THE INVARIANTS. **Append-only**: a step is never edited or removed.
//! **Typed at the boundary**: an unknown type is a 400 naming the valid set —
//! error text is a product surface — and `message` is refused here because it
//! is the mirror's word for what a PERSON said; a mind that could write
//! `message` steps could forge its own inbox. **Bounded**: empty content is
//! rejected, long content is clipped at [`crate::mind::STEP_MAX_CHARS`].
//! **Cheap and silent on success**: returns `"ok"`, never a value a program
//! would branch on.
//!
//! Announced as `mind.step` so a viewer can lane the stream live and the
//! wake driver's idle detection never has to poll.

use std::sync::Arc;

use crate::errors::{BoughError, ErrorKind};
use crate::schema::events::{EventInput, EventType};
use crate::schema::parts::MindStepType;
use crate::types::{Clock, HostFn, TurnCtx};

/// Injected seams: the clock, for tests that assert on `ts`.
#[derive(Clone, Default)]
pub struct StepDeps {
    pub now: Option<Clock>,
}

/// The types `step()` accepts — the registry minus the mirror's `message`.
const WRITABLE: [MindStepType; 5] = [
    MindStepType::Thought,
    MindStepType::Observation,
    MindStepType::Idle,
    MindStepType::Goal,
    MindStepType::Learning,
];

fn parse_type(raw: &str) -> Result<MindStepType, BoughError> {
    let t = MindStepType::parse(raw.trim());
    match t {
        Some(t) if WRITABLE.contains(&t) => Ok(t),
        _ => Err(BoughError::http(
            400,
            ErrorKind::BadRequest,
            format!(
                "step(type, content): type must be one of thought | observation | idle | \
goal | learning (got '{raw}'). 'message' belongs to the mirror — record what you \
did as an observation instead."
            ),
        )),
    }
}

/// Normalize the content: trim, reject empty, clip to the ceiling.
pub fn normalize(r#type: MindStepType, content: &str) -> Result<String, BoughError> {
    let t = content.trim();
    if t.is_empty() {
        return Err(BoughError::http(
            400,
            ErrorKind::BadRequest,
            format!(
                "step('{}', content) needs the content — an empty step records nothing",
                r#type.as_str()
            ),
        ));
    }
    let max = crate::mind::STEP_MAX_CHARS;
    if t.chars().count() <= max {
        return Ok(t.to_string());
    }
    let mut out: String = t.chars().take(max).collect();
    out.push('…');
    Ok(out)
}

/// Append one step for this turn's session and announce it.
pub fn record(ctx: &TurnCtx, deps: &StepDeps, raw_type: &str, content: &str) -> Result<String, BoughError> {
    let r#type = parse_type(raw_type)?;
    let content = normalize(r#type, content)?;
    let now: Clock = deps.now.clone().unwrap_or_else(|| ctx.app.now.clone());
    let step = {
        let db = ctx.app.db.lock().unwrap_or_else(|p| p.into_inner());
        db.add_mind_step(
            &ctx.session_id,
            Some(&ctx.turn_id),
            now(),
            r#type,
            "self",
            &content,
        )?
    };
    ctx.app.bus.publish(EventInput {
        r#type: EventType::MindStep,
        session_id: Some(ctx.session_id.clone()),
        data: serde_json::to_value(&step).unwrap_or_default(),
    });
    Ok("ok".to_string())
}

/// The bridged form: `step(type, content)` → `"ok"`.
pub fn create_step_host_fn(ctx: &TurnCtx, deps: StepDeps) -> HostFn {
    let ctx = ctx.clone();
    Arc::new(move |args: Vec<String>| {
        let ctx = ctx.clone();
        let deps = deps.clone();
        let raw_type = args.first().cloned().unwrap_or_default();
        let content = args.get(1).cloned().unwrap_or_default();
        Box::pin(async move { record(&ctx, &deps, &raw_type, &content) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testkit::{seed_session, shared_db, turn_ctx_for, SeedOpts};
    use crate::agents::with_db;
    use crate::schema::events::BoughEvent;
    use crate::schema::parts::SessionKind;
    use std::sync::Mutex;

    fn fixture() -> (TurnCtx, Arc<Mutex<Vec<BoughEvent>>>) {
        let db = shared_db();
        let session = seed_session(
            &db,
            SeedOpts {
                kind: Some(SessionKind::Mind),
                ..Default::default()
            },
        );
        let ctx = turn_ctx_for(&db, &session.id, "turn-1", 0);
        let seen: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = seen.clone();
        ctx.app.bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.clone());
        }));
        (ctx, seen)
    }

    #[tokio::test]
    async fn a_step_lands_typed_stamped_with_the_turn_and_announced() {
        let (ctx, seen) = fixture();
        let deps = StepDeps {
            now: Some(Arc::new(|| 7_000)),
        };
        let f = create_step_host_fn(&ctx, deps);
        assert_eq!(
            f(vec!["thought".into(), " the RLM idea again ".into()])
                .await
                .unwrap(),
            "ok"
        );
        let steps = with_db(&ctx.app.db, |d| d.mind_steps_for_turn("turn-1")).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].r#type, MindStepType::Thought);
        assert_eq!(steps[0].source, "self");
        assert_eq!(steps[0].content, "the RLM idea again");
        assert_eq!(steps[0].ts, 7_000);
        let events = seen.lock().unwrap();
        let mine: Vec<&BoughEvent> = events
            .iter()
            .filter(|e| e.r#type == EventType::MindStep)
            .collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].data["type"], "thought");
    }

    #[tokio::test]
    async fn unknown_types_message_and_empty_content_are_refused() {
        let (ctx, _) = fixture();
        let f = create_step_host_fn(&ctx, StepDeps::default());
        let err = f(vec!["daydream".into(), "x".into()]).await.unwrap_err();
        assert!(err.to_string().contains("thought | observation"), "{err}");
        let err = f(vec!["message".into(), "hi".into()]).await.unwrap_err();
        assert!(err.to_string().contains("mirror"), "{err}");
        let err = f(vec!["idle".into(), "  ".into()]).await.unwrap_err();
        assert!(err.to_string().contains("empty step"), "{err}");
        assert!(with_db(&ctx.app.db, |d| d.mind_steps_for_turn("turn-1"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn long_content_is_clipped_not_rejected() {
        let long = "y".repeat(crate::mind::STEP_MAX_CHARS + 10);
        let out = normalize(MindStepType::Thought, &long).unwrap();
        assert_eq!(out.chars().count(), crate::mind::STEP_MAX_CHARS + 1);
        assert!(out.ends_with('…'));
    }
}
