//! `POST /sessions/:id/ghost` — composer ghost text (route port of
//! `src/worker/ghost.ts`'s `ghostTextH`).
//!
//! ALWAYS 200 for a session that exists, `{ghost: null}` standing in for
//! every failure there is — a cheap-model outcome must never reach the
//! composer as an error banner. v1 ships `cheap: None` (ARCHITECTURE §4.3),
//! and null-on-absence is the documented contract, so this stub IS the
//! v1 behavior, not an approximation of it. POST rather than GET because the
//! half-typed prefix is user text that has no business in a URL or a log.
//! 404 is the only failure: an unknown session id is a real client bug.

use serde::Deserialize;
use serde_json::json;

use bough_core::errors::BoughError;

use crate::http::{handler, json as json_res, parse_body, Handler};

/// `{prefix?}` — accepted and (in v1) unused; the cheap tier is absent.
#[derive(Deserialize)]
struct GhostBody {
    #[serde(default)]
    #[allow(dead_code)]
    prefix: Option<String>,
}

/// `POST /sessions/:id/ghost` — `{ghost: string|null}`.
pub fn ghost_text() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        if ctx.db.lock().unwrap().get_session(&id)?.is_none() {
            return Err(BoughError::not_found(format!("session {id} not found")));
        }
        let _body: GhostBody = parse_body(req, Some(json!({}))).await?;
        Ok(json_res(&json!({ "ghost": null }), 200))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::schema::parts::{Session, SessionKind};
    use serde_json::json as j;

    fn seed_session(fx: &testutil::Fixture) -> String {
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
            })
            .unwrap();
        id
    }

    #[tokio::test]
    async fn ghost_is_always_200_with_null_for_an_existing_session() {
        let fx = testutil::fixture();
        let id = seed_session(&fx);
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        // With a prefix…
        let res = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/ghost"),
                Some(j!({"prefix": "fix the "})),
            ))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"ghost": null}));
        // …and with no body at all (the parse falls back to `{}`).
        let res = call
            .call(testutil::req("POST", &format!("/sessions/{id}/ghost"), None))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"ghost": null}));
    }

    #[tokio::test]
    async fn an_unknown_session_is_a_404_not_a_null_ghost() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req("POST", "/sessions/nope/ghost", Some(j!({"prefix": "x"}))))
            .await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"error": "session nope not found"})
        );
    }
}
