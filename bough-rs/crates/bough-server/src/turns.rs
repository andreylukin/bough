//! `POST /sessions/:id/interrupt` — the user stopping a turn that is running
//! (port of `src/server/turns.ts`).
//!
//! THE INVARIANT: **it is an ANSWER, not an error, that nothing was running.**
//! A stop pressed a beat after the turn ended, a double-tap, a retry after a
//! dropped connection: all of them are 200 with `interrupted: false`. The
//! alternative — a 409 for "no turn here" — makes every client write a
//! race-condition branch for a button whose whole job is to be safe to press.
//!
//! SECOND — **it does not wait.** The abort travels to a worker that has
//! children to kill and a partial tool result to persist, and that unwinding
//! is what publishes `turn.finished`. This handler signals and returns; the
//! client learns the turn actually stopped from the event stream.
//!
//! The registry lives on the ctx (`AppCtx.turn_registry`), so a test drives
//! the real route with its own `TurnRegistry` and no turn machinery at all.

use serde::Serialize;

use bough_core::errors::BoughError;
use bough_core::types::AppCtx;

use crate::http::{handler, json, Handler};

/// What a client gets back. `interrupted` is the only field worth branching on.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InterruptResult {
    pub session_id: String,
    /// True when a turn (or a cascade hook) was there to signal.
    pub interrupted: bool,
    /// Human-readable, so a CLI can print it verbatim.
    pub message: String,
}

fn require_session(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    match ctx.db.lock().unwrap().get_session(id)? {
        Some(_) => Ok(()),
        None => Err(BoughError::not_found(format!("no session {id}"))),
    }
}

/// `POST /sessions/:id/interrupt`.
pub fn interrupt_session() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        let interrupted = ctx.turn_registry.interrupt(&id);
        let result = InterruptResult {
            session_id: id,
            interrupted,
            message: if interrupted {
                "interrupting — the program's children are killed and the partial result is kept"
                    .to_string()
            } else {
                "nothing was running in this session".to_string()
            },
        };
        Ok(json(&result, 200))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions, Dispatcher};
    use crate::http::testutil::{self, Fixture};
    use bough_core::schema::parts::{Session, SessionKind};
    use serde_json::json as j;

    fn call(fx: &Fixture) -> Dispatcher {
        create_handler(fx.ctx.clone(), CreateHandlerOptions::default())
    }

    fn seed_session(fx: &Fixture) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: id.clone(),
                title: "t".into(),
                kind: SessionKind::Root,
                created_at: (fx.ctx.now)(),
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some("/tmp".into()),
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
            .unwrap();
        id
    }

    fn interrupt(id: &str) -> axum::extract::Request {
        testutil::req("POST", &format!("/sessions/{id}/interrupt"), None)
    }

    #[tokio::test]
    async fn interrupting_a_running_turn_aborts_it_and_reports_that_it_did() {
        let fx = testutil::fixture();
        let id = seed_session(&fx);

        let claim = fx.ctx.turn_registry.begin(&id).unwrap();
        let cascaded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = cascaded.clone();
        fx.ctx.turn_registry.on_interrupt(
            &id,
            std::sync::Arc::new(move || flag.store(true, std::sync::atomic::Ordering::SeqCst)),
        );

        let res = call(&fx).call(interrupt(&id)).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["interrupted"], true);
        assert_eq!(body["sessionId"], id.as_str());
        assert!(claim.cancel.is_cancelled(), "the turn's token must be cancelled");
        assert!(
            cascaded.load(std::sync::atomic::Ordering::SeqCst),
            "the cascade hooks must fire — that is what kills the children"
        );
    }

    #[tokio::test]
    async fn interrupting_an_idle_session_is_an_answer_not_an_error() {
        let fx = testutil::fixture();
        let id = seed_session(&fx);

        let res = call(&fx).call(interrupt(&id)).await;
        // A stop pressed a beat after the turn ended must not make the client
        // branch on a status code; `interrupted: false` is the whole answer.
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["interrupted"], false);
        assert!(
            body["message"].as_str().unwrap().contains("nothing was running"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_second_interrupt_is_safe_the_verb_is_idempotent() {
        let fx = testutil::fixture();
        let id = seed_session(&fx);
        let _claim = fx.ctx.turn_registry.begin(&id).unwrap();

        let first = testutil::body_json(call(&fx).call(interrupt(&id)).await).await;
        let second = testutil::body_json(call(&fx).call(interrupt(&id)).await).await;

        assert_eq!(first["interrupted"], true);
        // Still registered until the turn unwinds — a double-tap must not read
        // as a failure either way.
        assert!(second["interrupted"].is_boolean());
    }

    #[tokio::test]
    async fn interrupting_an_unknown_session_is_a_404_not_a_silent_success() {
        let fx = testutil::fixture();
        let res = call(&fx).call(interrupt("no-such-session")).await;
        assert_eq!(res.status(), 404);
        let body = testutil::body_json(res).await;
        assert_eq!(body, j!({"error": "no session no-such-session"}));
    }
}
