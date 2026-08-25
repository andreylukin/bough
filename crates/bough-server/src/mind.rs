//! Mind routes (specs/mind.md §8) — the thin HTTP adapters over
//! `bough_core::mind`'s state accessors.
//!
//! The invariant these hold: **only a `kind: mind` session has a mind
//! surface.** Every route 400s on any other kind rather than writing state
//! keys no driver would ever read — a mind toggle that "worked" on a root
//! would be a switch wired to nothing.

use bough_core::errors::BoughError;
use bough_core::mind::{self, keys};
use bough_core::schema::parts::SessionKind;
use bough_core::types::AppCtx;
use serde::{Deserialize, Serialize};

use crate::http::{handler, json, parse_body, Handler};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MindStatus {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona: Option<String>,
    idle_streak: i64,
    fail_streak: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_wake_at: Option<i64>,
    /// A wakeup is in flight or queued.
    pending: bool,
    step_count: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MindPatchBody {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    persona: Option<String>,
}

fn require_mind(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    let session = ctx.db.lock().unwrap().get_session(id)?;
    match session {
        None => Err(BoughError::not_found(format!("session {id} not found"))),
        Some(s) if s.kind != SessionKind::Mind => Err(BoughError::bad_request(format!(
            "session {id} is not a mind — create one with kind \"mind\""
        ))),
        Some(_) => Ok(()),
    }
}

fn read_i64(ctx: &AppCtx, id: &str, key: &str) -> Option<i64> {
    ctx.db
        .lock()
        .unwrap()
        .get_state(id, key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
}

fn status_of(ctx: &AppCtx, id: &str) -> Result<MindStatus, BoughError> {
    let (persona, step_count) = {
        let db = ctx.db.lock().unwrap();
        let persona = db.get_state(id, keys::PERSONA)?.filter(|p| !p.trim().is_empty());
        let step_count = db.mind_steps_after(id, 0, i64::MAX)?.len() as i64;
        (persona, step_count)
    };
    Ok(MindStatus {
        enabled: mind::is_enabled(&ctx.db, id),
        persona,
        idle_streak: read_i64(ctx, id, keys::IDLE_STREAK).unwrap_or(0),
        fail_streak: read_i64(ctx, id, keys::FAIL_STREAK).unwrap_or(0),
        next_wake_at: read_i64(ctx, id, keys::NEXT_WAKE_AT),
        pending: read_i64(ctx, id, keys::PENDING_SINCE).is_some(),
        step_count,
    })
}

/// `GET /sessions/:id/mind` — the loop's live state, derived from the same
/// rows the driver reads.
pub fn get_mind() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_mind(&ctx, &id)?;
        Ok(json(&status_of(&ctx, &id)?, 200))
    })
}

/// `POST /sessions/:id/mind` `{enabled?, persona?}` — enable stamps a due
/// wake and zeroes the streaks (`mind::set_enabled`); disable stops the next
/// tick from waking it. Returns the resulting status.
pub fn patch_mind() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_mind(&ctx, &id)?;
        let body: MindPatchBody = parse_body(req, Some(serde_json::json!({}))).await?;
        let now = (ctx.now)();
        if let Some(persona) = &body.persona {
            ctx.db
                .lock()
                .unwrap()
                .set_state(&id, keys::PERSONA, persona, now)?;
        }
        if let Some(enabled) = body.enabled {
            mind::set_enabled(&ctx.db, &id, enabled, now);
        }
        Ok(json(&status_of(&ctx, &id)?, 200))
    })
}

/// `GET /sessions/:id/mind/steps?n=` — the recent stream, oldest first,
/// default 50, capped at 500.
pub fn get_mind_steps() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_mind(&ctx, &id)?;
        let n: i64 = req
            .uri()
            .query()
            .and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix("n=").and_then(|v| v.parse().ok()))
            })
            .unwrap_or(50);
        let steps = ctx
            .db
            .lock()
            .unwrap()
            .mind_steps_tail(&id, n.clamp(1, 500))?;
        Ok(json(&steps, 200))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions, Dispatcher};
    use crate::http::testutil::{self, Fixture};
    use bough_core::schema::parts::{MindStepType, Session, SessionKind};
    use serde_json::json as j;

    fn call(fx: &Fixture) -> Dispatcher {
        create_handler(fx.ctx.clone(), CreateHandlerOptions::default())
    }

    fn seed(fx: &Fixture, kind: SessionKind) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: id.clone(),
                title: "t".into(),
                kind,
                created_at: (fx.ctx.now)(),
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some(std::env::temp_dir().to_string_lossy().into_owned()),
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
            .unwrap();
        id
    }

    #[tokio::test]
    async fn the_mind_surface_rejects_non_mind_sessions() {
        let fx = testutil::fixture();
        let root = seed(&fx, SessionKind::Root);
        let res = call(&fx)
            .call(testutil::get(&format!("/sessions/{root}/mind")))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        assert!(body["error"].as_str().unwrap().contains("not a mind"));

        let res = call(&fx)
            .call(testutil::get("/sessions/nope/mind"))
            .await;
        assert_eq!(res.status(), 404);
    }

    #[tokio::test]
    async fn enabling_stamps_a_due_wake_and_the_status_reads_back() {
        let fx = testutil::fixture();
        let id = seed(&fx, SessionKind::Mind);
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/mind"),
                Some(j!({"enabled": true, "persona": "terse and curious"})),
            ))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["enabled"], true);
        assert_eq!(body["persona"], "terse and curious");
        assert!(body["nextWakeAt"].is_i64());
        assert_eq!(body["idleStreak"], 0);
        assert!(bough_core::mind::is_enabled(&fx.ctx.db, &id));

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/mind"),
                Some(j!({"enabled": false})),
            ))
            .await;
        assert_eq!(testutil::body_json(res).await["enabled"], false);
        assert!(!bough_core::mind::is_enabled(&fx.ctx.db, &id));
    }

    #[tokio::test]
    async fn the_steps_route_returns_the_tail_oldest_first() {
        let fx = testutil::fixture();
        let id = seed(&fx, SessionKind::Mind);
        for i in 0..3 {
            fx.ctx
                .db
                .lock()
                .unwrap()
                .add_mind_step(&id, None, i, MindStepType::Thought, "self", &format!("t{i}"))
                .unwrap();
        }
        let res = call(&fx)
            .call(testutil::get(&format!("/sessions/{id}/mind/steps?n=2")))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["content"], "t1");
        assert_eq!(body[1]["content"], "t2");
    }
}
