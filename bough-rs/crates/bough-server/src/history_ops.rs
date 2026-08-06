//! History-operation routes (route surface of `src/history/*` handlers).
//!
//! Wave 2 (row 2.13): **fork** and **unsend** are live. The remaining five
//! (compact, sections, extract, move-into, handoff) stay v1-stubs per
//! server.md §8: 400 "not yet" — they land with wave 3 (row 3.13). For the
//! stubs, one TS ordering rule is kept live because it is cheap and it is the
//! reason these routes are nested under `/sessions/:id` at all: **a mistyped
//! session id 404s before anything else happens**, so a typo is diagnosed as a
//! typo and not as a missing feature.

use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::history::ops::fork::{fork, ForkDeps};
use bough_core::history::ops::unsend::unsend;
use bough_core::schema::requests::{ForkBody, UnsendBody};
use bough_core::types::AppCtx;

use crate::http::{handler, json as json_response, parse_body, Handler};

fn require_session(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    match ctx.db.lock().unwrap().get_session(id)? {
        Some(_) => Ok(()),
        None => Err(BoughError::not_found(format!("session {id} not found"))),
    }
}

/// One 400-not-yet handler per operation; the op name keeps the five
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

/// `POST /sessions/:id/fork` — `{session, thread, turnStarted}`, 201.
///
/// The thread rides along for the same reason `GET /sessions/:id` carries it:
/// the client is about to switch to this branch, and a create that answered
/// with a bare session id would force an immediate second fetch to render
/// anything at all.
///
/// 201 with the turn possibly still running is deliberate — the BRANCH is what
/// was created and it is complete; the turn reports over `/events` like every
/// other turn, and `turnStarted` says whether to expect one.
pub fn fork_session() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let body: ForkBody = parse_body(req, None).await?;
        let result = fork(&ctx, &id, &body, ForkDeps::default())?;
        // pi's branch-summary-on-switch is best-effort by construction: a
        // summariser that fails must not fail the branch, which already exists
        // and is already correct. The summarizer (`summarize_span`, compact's)
        // is not yet ported in this build, so the note is skipped — the same
        // degradation as a failed summary, logged the same way.
        if body.summarize_abandoned.unwrap_or(false) {
            tracing::error!(
                "branch summary failed [{}]: the compact summarizer is not yet ported in this build",
                result.session.id
            );
        }
        let thread = ctx.db.lock().unwrap().thread_for(&result.session.id)?;
        Ok(json_response(
            &json!({
                "session": result.session,
                "thread": thread,
                "turnStarted": result.turn_started,
            }),
            201,
        ))
    })
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

/// `POST /sessions/:id/unsend` — the take-back. 200 with
/// `{sessionId, text, removed, interrupted}`.
pub fn unsend_message() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let body: UnsendBody = parse_body(req, None).await?;
        let result = unsend(&ctx, &id, &body.at_message_id)?;
        Ok(json_response(&result, 200))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions, Dispatcher};
    use crate::http::testutil::{self, Fixture};
    use bough_core::schema::parts::{Message, Part, Role, Session, SessionKind};
    use serde_json::json as j;

    const NOT_YET_OPS: [&str; 5] = ["compact", "sections", "extract", "move-into", "handoff"];

    fn seed_session(fx: &Fixture, title: &str, parent_id: Option<&str>) -> Session {
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: uuid::Uuid::new_v4().to_string(),
                title: title.into(),
                kind: if parent_id.is_some() { SessionKind::Fork } else { SessionKind::Root },
                created_at: (fx.ctx.now)(),
                parent_id: parent_id.map(|s| s.to_string()),
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
            .unwrap()
    }

    fn seed_message(
        fx: &Fixture,
        session_id: &str,
        role: Role,
        text: &str,
        created_at: i64,
    ) -> Message {
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role,
                parts: vec![Part::Text { text: text.to_string() }],
                pending: false,
                created_at,
            })
            .unwrap()
    }

    fn call(fx: &Fixture) -> Dispatcher {
        create_handler(fx.ctx.clone(), CreateHandlerOptions::default())
    }

    fn texts_of(messages: &[serde_json::Value]) -> Vec<String> {
        messages
            .iter()
            .map(|m| {
                m["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p["text"].as_str().unwrap_or("<part>"))
                    .collect::<String>()
            })
            .collect()
    }

    fn own_texts(fx: &Fixture, session_id: &str) -> Vec<String> {
        fx.ctx
            .db
            .lock()
            .unwrap()
            .messages_for(session_id)
            .unwrap()
            .iter()
            .map(|m| {
                m.parts
                    .iter()
                    .map(|p| match p {
                        Part::Text { text } => text.clone(),
                        other => format!("<{other:?}>"),
                    })
                    .collect::<String>()
            })
            .collect()
    }

    // ---- the wave-3 stubs ---------------------------------------------------

    #[tokio::test]
    async fn a_mistyped_session_id_is_a_404_before_the_not_yet_400() {
        let fx = testutil::fixture();
        let call = call(&fx);
        for op in NOT_YET_OPS {
            let res = call
                .call(testutil::req("POST", &format!("/sessions/nope/{op}"), Some(j!({}))))
                .await;
            assert_eq!(res.status(), 404, "{op}");
        }
    }

    #[tokio::test]
    async fn every_unported_operation_on_a_real_session_is_a_400_naming_itself() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "t", None);
        let call = call(&fx);
        for op in NOT_YET_OPS {
            let res = call
                .call(testutil::req("POST", &format!("/sessions/{}/{op}", s.id), Some(j!({}))))
                .await;
            assert_eq!(res.status(), 400, "{op}");
            let body = testutil::body_json(res).await;
            assert_eq!(body["error"], format!("{op} is not yet ported in this build"), "{op}");
        }
    }

    // ---- fork (port of the route half of `src/history/fork.test.ts`) -------

    struct ForkScenario {
        target: Session,
        own: Vec<Message>,
    }

    fn fork_scenario(fx: &Fixture) -> ForkScenario {
        let parent = seed_session(fx, "parent", None);
        seed_message(fx, &parent.id, Role::User, "ancestor question", 1_000);
        seed_message(fx, &parent.id, Role::Supervisor, "ancestor answer", 1_001);
        let target = seed_session(fx, "rename the router", Some(&parent.id));
        seed_message(fx, &target.id, Role::User, "first ask", 1_002);
        seed_message(fx, &target.id, Role::Supervisor, "first answer", 1_003);
        seed_message(fx, &target.id, Role::User, "second ask", 1_004);
        seed_message(fx, &target.id, Role::Supervisor, "second answer", 1_005);
        let own = fx.ctx.db.lock().unwrap().messages_for(&target.id).unwrap();
        ForkScenario { target, own }
    }

    #[tokio::test]
    async fn post_fork_answers_201_with_the_branch_and_its_thread() {
        let fx = testutil::fixture();
        let s = fork_scenario(&fx);
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/fork", s.target.id),
                Some(j!({"atMessageId": s.own[2].id, "exclusive": true})),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let body = testutil::body_json(res).await;
        assert_eq!(body["session"]["kind"], "fork");
        assert_eq!(body["turnStarted"], false);
        // The thread rides along so the client can render the branch it is
        // switching to without an immediate second fetch.
        assert_eq!(
            texts_of(body["thread"].as_array().unwrap()),
            vec!["ancestor question", "ancestor answer", "first ask", "first answer"]
        );
    }

    #[tokio::test]
    async fn the_route_maps_a_bad_fork_point_to_400_and_an_unknown_session_to_404() {
        let fx = testutil::fixture();
        let s = fork_scenario(&fx);
        let call = call(&fx);
        let post = |id: String, body: serde_json::Value| {
            let call = &call;
            async move {
                call.call(testutil::req("POST", &format!("/sessions/{id}/fork"), Some(body)))
                    .await
            }
        };

        let bad = post(
            s.target.id.clone(),
            j!({"atMessageId": s.own[1].id, "editedText": "nope"}),
        )
        .await;
        assert_eq!(bad.status(), 400);
        let body = testutil::body_json(bad).await;
        assert!(
            body["error"].as_str().unwrap().contains("can only replace a user message"),
            "{body}"
        );

        let missing =
            post("no-such-session".into(), j!({"atMessageId": s.own[0].id})).await;
        assert_eq!(missing.status(), 404);

        // A body the schema rejects is the router's 400, not the domain's.
        let malformed = post(s.target.id.clone(), j!({"atPart": 1})).await;
        assert_eq!(malformed.status(), 400);
        let body = testutil::body_json(malformed).await;
        assert!(body["error"].as_str().unwrap().contains("invalid body"), "{body}");
    }

    #[tokio::test]
    async fn the_route_starts_the_turn_through_the_ctx_seam_boot_wires() {
        // The seam boot fills — read off the ctx structurally; the fixture's
        // recording starter stands in for the composed turn starter.
        let fx = testutil::fixture();
        let s = fork_scenario(&fx);

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/fork", s.target.id),
                Some(j!({"atMessageId": s.own[2].id, "editedText": "try again"})),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let body = testutil::body_json(res).await;
        assert_eq!(body["turnStarted"], true);

        let started = fx.started.lock().unwrap();
        assert_eq!(started.len(), 1);
        let (session, message) = &started[0];
        assert_eq!(session.id, body["session"]["id"].as_str().unwrap());
        assert_eq!(message.parts, vec![Part::Text { text: "try again".into() }]);
    }

    // ---- unsend (port of `src/history/unsend.test.ts`) ----------------------

    async fn post_unsend(
        call: &Dispatcher,
        id: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        call.call(testutil::req("POST", &format!("/sessions/{id}/unsend"), Some(body))).await
    }

    #[tokio::test]
    async fn the_last_user_message_and_the_answer_it_provoked_are_gone_the_rest_untouched() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "chat", None);
        seed_message(&fx, &s.id, Role::User, "first ask", 1_000);
        seed_message(&fx, &s.id, Role::Supervisor, "first answer", 1_001);
        let retracted = seed_message(&fx, &s.id, Role::User, "the typo", 1_002);
        let partial = seed_message(&fx, &s.id, Role::Supervisor, "half an answer to a typo", 1_003);

        let res = post_unsend(&call(&fx), &s.id, j!({"atMessageId": retracted.id})).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;

        // The text comes back for the composer — that is the whole point of
        // the gesture.
        assert_eq!(body["text"], "the typo");
        assert_eq!(body["removed"], j!([retracted.id, partial.id]));
        // Nothing was running, and saying so is not an error.
        assert_eq!(body["interrupted"], false);

        // The conversation is now what it was before the message was ever sent.
        assert_eq!(own_texts(&fx, &s.id), vec!["first ask", "first answer"]);
        let db = fx.ctx.db.lock().unwrap();
        assert_eq!(db.get_message(&retracted.id).unwrap(), None);
        assert_eq!(db.get_message(&partial.id).unwrap(), None);
    }

    #[tokio::test]
    async fn a_retracted_message_stops_answering_keyword_search() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "chat", None);
        let m = seed_message(&fx, &s.id, Role::User, "kumquat migration plan", 1_000);
        {
            let db = fx.ctx.db.lock().unwrap();
            db.index_message(&m).unwrap();
            assert_eq!(db.search_messages("kumquat", None, None).unwrap().len(), 1);
        }

        post_unsend(&call(&fx), &s.id, j!({"atMessageId": m.id})).await;
        // A deleted message that still matched would surface in `/search` with
        // nothing to open — the FTS row has to go with the message.
        assert!(fx
            .ctx
            .db
            .lock()
            .unwrap()
            .search_messages("kumquat", None, None)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn the_session_it_was_sent_in_is_the_one_it_is_removed_from_siblings_untouched() {
        let fx = testutil::fixture();
        let parent = seed_session(&fx, "parent", None);
        seed_message(&fx, &parent.id, Role::User, "ancestor question", 1_000);
        let own = seed_session(&fx, "branch", Some(&parent.id));
        let mine = seed_message(&fx, &own.id, Role::User, "my ask", 1_001);

        post_unsend(&call(&fx), &own.id, j!({"atMessageId": mine.id})).await;

        assert!(own_texts(&fx, &own.id).is_empty());
        // The inherited prefix is another session's rows and was never in scope.
        assert_eq!(own_texts(&fx, &parent.id), vec!["ancestor question"]);
    }

    #[tokio::test]
    async fn an_earlier_user_message_is_refused_that_is_what_fork_is_for() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "chat", None);
        let earlier = seed_message(&fx, &s.id, Role::User, "first ask", 1_000);
        seed_message(&fx, &s.id, Role::Supervisor, "first answer", 1_001);
        seed_message(&fx, &s.id, Role::User, "second ask", 1_002);

        let res = post_unsend(&call(&fx), &s.id, j!({"atMessageId": earlier.id})).await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        // The refusal names the operation that works, because a user who
        // reached this has a real intention and a bare 400 leaves them with a
        // key that did nothing.
        assert!(body["error"].as_str().unwrap().contains("fork"), "{body}");
        // And it refused by not deleting, which is the assertion that actually
        // matters.
        assert_eq!(own_texts(&fx, &s.id), vec!["first ask", "first answer", "second ask"]);
    }

    #[tokio::test]
    async fn a_supervisor_message_is_refused_the_models_turns_are_not_the_users_to_retract() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "chat", None);
        seed_message(&fx, &s.id, Role::User, "ask", 1_000);
        let answer = seed_message(&fx, &s.id, Role::Supervisor, "answer", 1_001);

        let res = post_unsend(&call(&fx), &s.id, j!({"atMessageId": answer.id})).await;
        assert_eq!(res.status(), 400);
        assert_eq!(fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_ancestors_message_is_refused_a_session_cannot_delete_rows_it_does_not_own() {
        let fx = testutil::fixture();
        let parent = seed_session(&fx, "parent", None);
        let theirs = seed_message(&fx, &parent.id, Role::User, "ancestor question", 1_000);
        let own = seed_session(&fx, "branch", Some(&parent.id));
        seed_message(&fx, &own.id, Role::User, "my ask", 1_001);

        let res = post_unsend(&call(&fx), &own.id, j!({"atMessageId": theirs.id})).await;
        assert_eq!(res.status(), 400);
        assert_eq!(own_texts(&fx, &parent.id), vec!["ancestor question"]);
    }

    #[tokio::test]
    async fn an_unknown_session_is_a_404_and_an_unknown_message_a_400() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "chat", None);
        let m = seed_message(&fx, &s.id, Role::User, "ask", 1_000);
        let call = call(&fx);

        assert_eq!(post_unsend(&call, "nope", j!({"atMessageId": m.id})).await.status(), 404);
        assert_eq!(post_unsend(&call, &s.id, j!({"atMessageId": "nope"})).await.status(), 400);
        assert_eq!(fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unsend_interrupts_the_running_turn_and_reports_that_it_did() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "chat", None);
        let m = seed_message(&fx, &s.id, Role::User, "the typo", 1_000);
        // A claimed turn — stop first, delete second.
        let claim = fx.ctx.turn_registry.begin(&s.id).unwrap();

        let res = post_unsend(&call(&fx), &s.id, j!({"atMessageId": m.id})).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["interrupted"], true);
        assert!(claim.cancel.is_cancelled(), "the turn's token must be cancelled");
        assert!(fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap().is_empty());
    }
}
