//! The `ask()` REST surface (port of `src/server/questions.ts`): read the
//! live holds, settle one.
//!
//! `GET /questions` is a RECONNECT path, not a feed — it answers from memory
//! because that is where holds live; a restart leaves the list empty and
//! there is nothing stale to heal. `POST /sessions/:id/questions/:qid` is
//! scoped by session id AND question id so a client cannot answer another
//! session's hold by guessing a uuid.
//!
//! v1: the `AskRegistry` on `HostState` is the wave-2 stub whose `pending()`
//! is always empty (the `ask()` host fn lands with row 2.5), so the honest
//! answers are the TS restart-state ones: an empty list, and a 404 whose
//! message explains that holds are memory-only. The settle verbs (answer /
//! decline / the 409 settled-race) un-stub with the registry.

use bough_core::errors::BoughError;
use bough_core::schema::parts::AskQuestion;

use crate::http::{handler, json, Handler};

/// `GET /questions[?sessionId=]` — every question awaiting an answer, oldest
/// first. A bare array, like `GET /sessions`: the list IS the resource.
pub fn list_questions() -> Handler {
    handler(|req, ctx, _params| async move {
        let query = req.uri().query().unwrap_or("");
        let session_id = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("sessionId="))
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        let pending: Vec<AskQuestion> = ctx
            .host
            .asks
            .pending()
            .into_iter()
            .filter(|q| session_id.as_deref().is_none_or(|s| q.session_id == s))
            .collect();
        Ok(json(&pending, 200))
    })
}

/// `POST /sessions/:id/questions/:qid` — settle one hold.
///
/// v1: nothing is ever held (see the header), so every qid is the 404 whose
/// message explains memory-only holds — the same answer TS gives for a hold
/// that settled, was interrupted, or predates a restart.
pub fn answer_question() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let qid = params.get("qid").cloned().unwrap_or_default();
        let held = ctx.host.asks.pending().into_iter().any(|q| q.id == qid && q.session_id == id);
        if !held {
            return Err(BoughError::not_found(format!(
                "no question awaiting an answer for {qid} in session {id} — holds are \
                 memory-only, so one that was already settled, interrupted, or raised before \
                 a restart is gone. GET /questions lists the live ones.",
            )));
        }
        // Unreachable in v1: `pending()` is empty until the ask() host fn
        // lands (wave 2, row 2.5) — the registry has no settle verbs yet.
        Err(BoughError::not_found(format!(
            "no question awaiting an answer for {qid} in session {id} — holds are \
             memory-only, so one that was already settled, interrupted, or raised before \
             a restart is gone. GET /questions lists the live ones.",
        )))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use serde_json::json as j;

    #[tokio::test]
    async fn the_questions_listing_is_a_bare_empty_array_after_a_restart() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/questions")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!([]));
        // The session filter narrows the same (empty) memory-only set.
        let res = call.call(testutil::get("/questions?sessionId=abc")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!([]));
    }

    #[tokio::test]
    async fn answering_an_unknown_hold_is_a_404_explaining_memory_only_holds() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                "/sessions/s1/questions/q_missing",
                Some(j!({"answer": "yes"})),
            ))
            .await;
        assert_eq!(res.status(), 404);
        let body = testutil::body_json(res).await;
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("no question awaiting an answer for q_missing in session s1"), "{msg}");
        assert!(msg.contains("memory-only"), "{msg}");
        assert!(msg.contains("GET /questions"), "{msg}");
    }
}
