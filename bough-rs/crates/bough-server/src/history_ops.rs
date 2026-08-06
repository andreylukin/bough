//! History-operation routes (route surface of `src/history/*` handlers,
//! wave-1 stub): fork, compact, sections, extract, move-into, handoff,
//! unsend.
//!
//! v1-STUB per server.md §8: 400 "not yet" — the history subsystem lands in
//! wave 2/3. One TS ordering rule is kept live because it is cheap and it is
//! the reason these routes are nested under `/sessions/:id` at all: **a
//! mistyped session id 404s before anything else happens** (in TS, before an
//! LLM call; here, before the not-yet 400), so a typo is diagnosed as a typo
//! and not as a missing feature.

use bough_core::errors::BoughError;
use bough_core::types::AppCtx;

use crate::http::{handler, Handler};

fn require_session(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    match ctx.db.lock().unwrap().get_session(id)? {
        Some(_) => Ok(()),
        None => Err(BoughError::not_found(format!("session {id} not found"))),
    }
}

/// One 400-not-yet handler per operation; the op name keeps the seven
/// error messages distinguishable in a client log.
fn not_yet_op(op: &'static str) -> Handler {
    handler(move |_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        Err::<axum::response::Response, _>(BoughError::bad_request(format!(
            "{op} is not yet ported in this build",
        )))
    })
}

/// `POST /sessions/:id/fork`
pub fn fork_session() -> Handler {
    not_yet_op("fork")
}

/// `POST /sessions/:id/compact`
pub fn compact_session() -> Handler {
    not_yet_op("compact")
}

/// `POST /sessions/:id/sections`
pub fn sections() -> Handler {
    not_yet_op("sections")
}

/// `POST /sessions/:id/extract`
pub fn extract() -> Handler {
    not_yet_op("extract")
}

/// `POST /sessions/:id/move-into`
pub fn move_into() -> Handler {
    not_yet_op("move-into")
}

/// `POST /sessions/:id/handoff`
pub fn handoff() -> Handler {
    not_yet_op("handoff")
}

/// `POST /sessions/:id/unsend`
pub fn unsend_message() -> Handler {
    not_yet_op("unsend")
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::schema::parts::{Session, SessionKind};
    use serde_json::json as j;

    const OPS: [&str; 7] =
        ["fork", "compact", "sections", "extract", "move-into", "handoff", "unsend"];

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
    async fn a_mistyped_session_id_is_a_404_before_the_not_yet_400() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        for op in OPS {
            let res = call
                .call(testutil::req("POST", &format!("/sessions/nope/{op}"), Some(j!({}))))
                .await;
            assert_eq!(res.status(), 404, "{op}");
        }
    }

    #[tokio::test]
    async fn every_operation_on_a_real_session_is_a_400_naming_itself() {
        let fx = testutil::fixture();
        let id = seed_session(&fx);
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        for op in OPS {
            let res = call
                .call(testutil::req("POST", &format!("/sessions/{id}/{op}"), Some(j!({}))))
                .await;
            assert_eq!(res.status(), 400, "{op}");
            let body = testutil::body_json(res).await;
            assert_eq!(body["error"], format!("{op} is not yet ported in this build"), "{op}");
        }
    }
}
