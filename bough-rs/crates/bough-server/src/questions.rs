//! The `ask()` REST surface (port of `src/server/questions.ts`): read the
//! live holds, settle one.
//!
//! `GET /questions` is a RECONNECT path, not a feed — it answers from memory
//! because that is where holds live; a restart leaves the list empty and
//! there is nothing stale to heal. `POST /sessions/:id/questions/:qid` is
//! scoped by session id AND question id so a client cannot answer another
//! session's hold by guessing a uuid.
//!
//! Both handlers are thin translations over the `hostfn/ask` registry: no
//! hold logic lives here, and no HTTP lives there. A question that settled
//! between the read and the write is a 409 rather than a silent success.

use bough_core::errors::BoughError;
use bough_core::schema::parts::AskQuestion;
use bough_core::schema::requests::AnswerQuestionBody;

use crate::http::{handler, json, parse_body, Handler};

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
        let pending: Vec<AskQuestion> = ctx.host.asks.list(session_id.as_deref());
        Ok(json(&pending, 200))
    })
}

/// `POST /sessions/:id/questions/:qid` — `{answer}` resolves the program's
/// `ask()`; `{decline: true}` rejects it with a catchable "user declined".
///
/// The empty-answer check is not pedantry. An empty string would resolve
/// `ask()` with nothing, and the program would branch on "" as though the user
/// had chosen it — a dismissal is what "I am not answering this" means, and it
/// has its own flag.
pub fn answer_question() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let qid = params.get("qid").cloned().unwrap_or_default();
        let question = ctx.host.asks.get(&qid);
        if !question.is_some_and(|q| q.session_id == id) {
            return Err(BoughError::not_found(format!(
                "no question awaiting an answer for {qid} in session {id} — holds are \
                 memory-only, so one that was already settled, interrupted, or raised before \
                 a restart is gone. GET /questions lists the live ones.",
            )));
        }

        let body: AnswerQuestionBody =
            parse_body(req, Some(serde_json::json!({}))).await?;

        if body.decline == Some(true) {
            if !ctx.host.asks.decline(&qid) {
                return Err(settled_meanwhile(&qid));
            }
            return Ok(json(&serde_json::json!({ "ok": true, "id": qid, "status": "declined" }), 200));
        }

        let answer = body.answer.as_deref().unwrap_or("");
        if answer.trim().is_empty() {
            return Err(BoughError::bad_request(
                "body must be {answer: \"…\"} with non-empty text, or {decline: true} to dismiss \
                 the question"
                    .to_string(),
            ));
        }
        if !ctx.host.asks.answer(&qid, answer) {
            return Err(settled_meanwhile(&qid));
        }
        Ok(json(&serde_json::json!({ "ok": true, "id": qid, "status": "answered" }), 200))
    })
}

/// The read-then-write race: someone else settled it in between.
fn settled_meanwhile(qid: &str) -> BoughError {
    BoughError::conflict(format!(
        "question {qid} settled before this answer arrived — it was answered, declined, \
         or its turn ended. Nothing was applied.",
    ))
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
