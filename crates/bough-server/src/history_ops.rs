//! History-operation routes (route surface of `src/history/*` handlers).
//!
//! All seven are live: fork and unsend landed in wave 2 (row 2.13), compact /
//! sections / extract / move-into / handoff in wave 3 (row 3.13). One TS
//! ordering rule holds across all of them and is the reason these routes are
//! nested under `/sessions/:id` at all: **a mistyped session id 404s before
//! anything else happens**, so a typo is diagnosed as a typo — and, for the
//! LLM-backed verbs, before a paid round-trip is bought.
//!
//! The status codes are the contract: **201 for the four that CREATE a session**
//! (fork, compact, extract, handoff), **200 for the two that do not** (move-into
//! appends to an existing session; sections stores nothing at all).

use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::history::ops::compact::{compact, summarize_span, CompactDeps};
use bough_core::history::ops::extract::extract as extract_op;
use bough_core::history::ops::fork::{fork, ForkDeps};
use bough_core::history::ops::handoff::handoff as handoff_op;
use bough_core::history::ops::move_into::move_into as move_into_op;
use bough_core::history::ops::sections::sectionize;
use bough_core::history::ops::unsend::unsend;
use bough_core::schema::events::{EventInput, EventType};
use bough_core::schema::parts::{Message, Part, Role, Session};
use bough_core::schema::requests::{
    CompactBody, ExtractBody, ForkBody, HandoffBody, MoveBody, SectionsBody, UnsendBody,
};
use bough_core::turn::runner::DEFAULT_MODEL;
use bough_core::types::AppCtx;

use crate::http::{handler, json as json_response, parse_body, Handler};

fn require_session(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    match ctx.db.lock().unwrap().get_session(id)? {
        Some(_) => Ok(()),
        None => Err(BoughError::not_found(format!("session {id} not found"))),
    }
}

/// The thread a create answers with. It is `thread_for`, not the seeded
/// messages: the inherited ancestors are half of what the user will be looking
/// at, and they were never seeded.
fn thread_of(ctx: &AppCtx, session_id: &str) -> Result<serde_json::Value, BoughError> {
    let thread = ctx.db.lock().unwrap().thread_for(session_id)?;
    Ok(serde_json::to_value(thread).unwrap_or_else(|_| json!([])))
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
        // pi's branch-summary-on-switch. The abandoned path is everything from
        // the fork point to the end of the SOURCE — precisely what you stop
        // being able to see the moment you branch — so the essence of it is
        // carried onto the new path as a system note rather than lost.
        // Best-effort by construction: a summariser that fails must not fail
        // the branch, which already exists and is already correct.
        if body.summarize_abandoned.unwrap_or(false) {
            summarize_abandoned_path(&ctx, &result.session, &id, &body.at_message_id).await;
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

/// Seed the branch with a summary of the path it left behind. Every failure is
/// logged and swallowed — the fork already happened and is already correct.
async fn summarize_abandoned_path(
    ctx: &AppCtx,
    branch: &Session,
    source_id: &str,
    at_message_id: &str,
) {
    let own = match ctx.db.lock().unwrap().messages_for(source_id) {
        Ok(own) => own,
        Err(err) => {
            tracing::error!("branch summary failed [{}]: {err}", branch.id);
            return;
        }
    };
    let Some(at) = own.iter().position(|m| m.id == at_message_id) else {
        return;
    };
    let abandoned = &own[at..];
    if abandoned.is_empty() {
        return;
    }
    let model = branch
        .model
        .clone()
        .or_else(|| ctx.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let text = match summarize_span(ctx, &model, abandoned, None).await {
        Ok(text) => text,
        Err(err) => {
            tracing::error!("branch summary failed [{}]: {err}", branch.id);
            return;
        }
    };
    // Seeded exactly the way the seeder seeds: complete on arrival, never
    // `pending` (nothing exists to close it), announced so an open client
    // renders it with no new reducer.
    let note = ctx.db.lock().unwrap().create_message(Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: branch.id.clone(),
        role: Role::System,
        parts: vec![Part::Text {
            text: format!("Summary of the path this branch left behind:\n\n{text}"),
        }],
        pending: false,
        created_at: (ctx.now)(),
    });
    match note {
        Ok(note) => {
            ctx.bus.publish(EventInput {
                r#type: EventType::MessageStarted,
                session_id: Some(branch.id.clone()),
                data: serde_json::to_value(&note).unwrap_or_default(),
            });
        }
        Err(err) => tracing::error!("branch summary failed [{}]: {err}", branch.id),
    }
}

/// `POST /sessions/:id/compact` — 201 with the new compaction branch and its
/// thread.
///
/// 201 because a compaction CREATES a session, the same as `POST /sessions`.
/// The thread rides along for the same reason `GET /sessions/:id` carries it:
/// the client is about to switch to this branch, and a create that answered
/// with a bare session would force an immediate second fetch to render anything
/// at all.
pub fn compact_session() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let body: CompactBody = parse_body(req, None).await?;
        body.validate()?;
        let session = compact(&ctx, &id, &body, CompactDeps::default()).await?;
        let thread = thread_of(&ctx, &session.id)?;
        Ok(json_response(
            &json!({ "session": session, "thread": thread }),
            201,
        ))
    })
}

/// `POST /sessions/:id/sections` — `{sections}` for the turns in the body.
///
/// The session id is VALIDATED and then unused — the labeling itself never
/// touches the database. It is checked because the URL claims a session: a
/// client sending a stale or mistyped id gets a 404 instead of a paid LLM
/// round-trip whose ranges point at a thread nobody is looking at.
pub fn sections() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        let body: SectionsBody = parse_body(req, None).await?;
        body.validate()?;
        let sections = sectionize(ctx.llm.clone(), &body.turns).await?;
        Ok(json_response(&json!({ "sections": sections }), 200))
    })
}

/// `POST /sessions/:id/extract` — 201 with the new root and its thread.
pub fn extract() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let body: ExtractBody = parse_body(req, None).await?;
        body.validate()?;
        let result = extract_op(&ctx, &id, &body)?;
        let thread = thread_of(&ctx, &result.session.id)?;
        Ok(json_response(
            &json!({ "session": result.session, "thread": thread }),
            201,
        ))
    })
}

/// `POST /sessions/:id/move-into` — **200** with the target, its thread and a
/// count.
///
/// 200, not 201: unlike every other history operation, this one creates no
/// session. The `:id` in the path is the TARGET; the source travels in the body,
/// because it is the argument and not the thing being acted on. `appended` is in
/// the response because duplicate picks of one message merge, so the count the
/// client would otherwise assume can differ from the count that was written.
pub fn move_into() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let body: MoveBody = parse_body(req, None).await?;
        body.validate()?;
        let result = move_into_op(&ctx, &id, &body)?;
        let thread = thread_of(&ctx, &result.session.id)?;
        Ok(json_response(
            &json!({
                "session": result.session,
                "thread": thread,
                "appended": result.messages.len(),
            }),
            200,
        ))
    })
}

/// `POST /sessions/:id/handoff` — 201 with the new root, draft attached.
///
/// No `thread` in the response, unlike fork/compact/extract: a handoff seeds no
/// messages and the new root inherits none, so the thread is empty by
/// construction and sending it would only suggest there is something to look at.
pub fn handoff() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let body: HandoffBody = parse_body(req, None).await?;
        body.validate()?;
        let session = handoff_op(&ctx, &id, &body, CompactDeps::default()).await?;
        Ok(json_response(&json!({ "session": session }), 201))
    })
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

    fn seed_session(fx: &Fixture, title: &str, parent_id: Option<&str>) -> Session {
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: uuid::Uuid::new_v4().to_string(),
                title: title.into(),
                kind: if parent_id.is_some() {
                    SessionKind::Fork
                } else {
                    SessionKind::Root
                },
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
                description: None,
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
                parts: vec![Part::Text {
                    text: text.to_string(),
                }],
                pending: false,
                created_at,
            })
            .unwrap()
    }

    fn call(fx: &Fixture) -> Dispatcher {
        create_handler(fx.ctx.clone(), CreateHandlerOptions::default())
    }

    // ---- a recording LLM, for the three routes that summarize ---------------

    /// A one-shot completion client that answers a fixed reply and records what
    /// it was asked. The server fixture ships `llm: None`, and the LLM-backed
    /// routes must never reach a real provider from a test.
    #[derive(Default)]
    struct RecordingLlm {
        reply: String,
        prompts: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl bough_core::types::LlmClient for RecordingLlm {
        async fn run(
            &self,
            params: bough_core::types::LlmParams,
            _on_text: bough_core::types::OnText,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<bough_core::types::LlmResult, bough_core::errors::LlmError> {
            let prompt = params
                .messages
                .first()
                .and_then(|m| m.content.first())
                .map(|b| match b {
                    bough_core::types::LlmContentBlock::Text { text } => text.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            self.prompts.lock().unwrap().push(prompt);
            Ok(bough_core::types::LlmResult {
                content: vec![bough_core::types::LlmBlock::Text {
                    text: self.reply.clone(),
                }],
                stop_reason: "end_turn".into(),
                usage: None,
            })
        }
    }

    /// A fixture whose ctx carries the recording client. Derefs to the plain
    /// [`Fixture`] so every helper above keeps working on it.
    struct Fx {
        inner: Fixture,
        llm: std::sync::Arc<RecordingLlm>,
    }

    impl std::ops::Deref for Fx {
        type Target = Fixture;
        fn deref(&self) -> &Fixture {
            &self.inner
        }
    }

    impl Fx {
        fn llm_calls(&self) -> usize {
            self.llm.prompts.lock().unwrap().len()
        }
        fn llm_prompts(&self) -> Vec<String> {
            self.llm.prompts.lock().unwrap().clone()
        }
    }

    fn with_llm(mut fx: Fixture, reply: &str) -> Fx {
        let llm = std::sync::Arc::new(RecordingLlm {
            reply: reply.to_string(),
            prompts: std::sync::Mutex::new(vec![]),
        });
        fx.ctx.llm = Some(llm.clone());
        Fx { inner: fx, llm }
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

    // ---- a mistyped session id, on every operation ---------------------------

    #[tokio::test]
    async fn a_mistyped_session_id_is_a_404_on_every_history_operation() {
        let fx = with_llm(testutil::fixture(), "SUMMARY");
        let call = call(&fx);
        // A well-formed body for each op, so the 404 is the SESSION's and not
        // the schema's.
        let bodies = [
            ("compact", j!({ "picks": [{ "messageId": "m" }] })),
            ("sections", j!({ "turns": [{ "gist": "a" }] })),
            ("extract", j!({ "picks": [{ "messageId": "m" }] })),
            (
                "move-into",
                j!({ "sourceId": "s", "picks": [{ "messageId": "m" }] }),
            ),
            ("handoff", j!({ "goal": "go" })),
            ("fork", j!({ "atMessageId": "m" })),
            ("unsend", j!({ "atMessageId": "m" })),
        ];
        for (op, body) in bodies {
            let res = call
                .call(testutil::req(
                    "POST",
                    &format!("/sessions/nope/{op}"),
                    Some(body),
                ))
                .await;
            assert_eq!(res.status(), 404, "{op}");
        }
        // …and a stale id never buys an LLM call.
        assert_eq!(fx.llm_calls(), 0);
    }

    // ---- compact (route half of `src/history/compact.test.ts`) ---------------

    #[tokio::test]
    async fn post_compact_is_reachable_and_answers_201_with_the_branch_and_its_thread() {
        let fx = with_llm(testutil::fixture(), "SUMMARY-0");
        let s = seed_session(&fx, "the work", None);
        let a = seed_message(&fx, &s.id, Role::User, "a", 1_000);
        let b = seed_message(&fx, &s.id, Role::Supervisor, "b", 1_001);
        seed_message(&fx, &s.id, Role::User, "c", 1_002);
        let _ = (a, &b);

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/compact", s.id),
                Some(j!({ "picks": [{ "messageId": b.id }] })),
            ))
            .await;

        assert_eq!(res.status(), 201);
        let body = testutil::body_json(res).await;
        assert_eq!(body["session"]["kind"], "compaction");
        assert_eq!(
            texts_of(body["thread"].as_array().unwrap()),
            vec!["a", "SUMMARY-0", "c"]
        );
    }

    #[tokio::test]
    async fn the_compact_route_maps_domain_refusals_to_their_statuses() {
        let fx = with_llm(testutil::fixture(), "SUMMARY-0");
        let call = call(&fx);

        let missing = call
            .call(testutil::req(
                "POST",
                "/sessions/nope/compact",
                Some(j!({ "picks": [{ "messageId": "m" }] })),
            ))
            .await;
        assert_eq!(missing.status(), 404);

        // An empty selection is the schema's 400, decided at the router edge.
        let s = seed_session(&fx, "the work", None);
        seed_message(&fx, &s.id, Role::User, "a", 1_000);
        let bad = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/compact", s.id),
                Some(j!({ "picks": [] })),
            ))
            .await;
        assert_eq!(bad.status(), 400);
    }

    // ---- extract (route half of `src/history/extract.test.ts`) ---------------

    #[tokio::test]
    async fn post_extract_answers_201_with_the_new_root_and_its_thread() {
        let fx = testutil::fixture();
        let parent = seed_session(&fx, "the original work", None);
        seed_message(&fx, &parent.id, Role::User, "ancestor question", 1_000);
        let inherited = seed_message(&fx, &parent.id, Role::Supervisor, "ancestor answer", 1_001);
        let child = seed_session(&fx, "fork · the original work", Some(&parent.id));
        let own = seed_message(&fx, &child.id, Role::User, "own ask", 1_002);

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/extract", child.id),
                Some(j!({ "picks": [{ "messageId": inherited.id }, { "messageId": own.id }] })),
            ))
            .await;

        assert_eq!(res.status(), 201);
        let body = testutil::body_json(res).await;
        assert_eq!(body["session"]["kind"], "root");
        assert_eq!(body["session"]["parentId"], j!(null));
        // The thread rides along so the client can render the root it is
        // switching to without an immediate second fetch.
        assert_eq!(
            texts_of(body["thread"].as_array().unwrap()),
            vec!["ancestor answer", "own ask"]
        );
    }

    #[tokio::test]
    async fn the_extract_route_maps_an_unknown_session_to_404_and_a_bad_body_to_400() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "the work", None);
        seed_message(&fx, &s.id, Role::User, "a", 1_000);
        let call = call(&fx);

        let missing = call
            .call(testutil::req(
                "POST",
                "/sessions/no-such-session/extract",
                Some(j!({ "picks": [{ "messageId": "m" }] })),
            ))
            .await;
        assert_eq!(missing.status(), 404);

        // An empty selection is the schema's 400, not the domain's.
        let empty = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/extract", s.id),
                Some(j!({ "picks": [] })),
            ))
            .await;
        assert_eq!(empty.status(), 400);

        let stray = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/extract", s.id),
                Some(j!({ "picks": [{ "messageId": "nope" }] })),
            ))
            .await;
        assert_eq!(stray.status(), 400);
        let body = testutil::body_json(stray).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("no message nope exists"),
            "{body}"
        );
    }

    // ---- move-into (route half of `src/history/move.test.ts`) ----------------

    #[tokio::test]
    async fn post_move_into_answers_200_with_the_target_its_thread_and_a_count() {
        let fx = testutil::fixture();
        let source = seed_session(&fx, "the investigation", None);
        let one = seed_message(&fx, &source.id, Role::User, "why twice?", 1_000);
        let two = seed_message(&fx, &source.id, Role::Supervisor, "catch-up", 1_001);
        let target = seed_session(&fx, "the fix", None);
        seed_message(&fx, &target.id, Role::User, "let's write it up", 1_002);

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/move-into", target.id),
                Some(j!({
                    "sourceId": source.id,
                    // Duplicate picks of one message merge, so `appended` is 2,
                    // not 3 — which is exactly why the count is in the response.
                    "picks": [
                        { "messageId": one.id },
                        { "messageId": one.id, "parts": [0] },
                        { "messageId": two.id },
                    ],
                })),
            ))
            .await;

        // 200, not 201: this history operation creates no session.
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["session"]["id"], target.id);
        assert_eq!(body["appended"], 2);
        assert_eq!(
            texts_of(body["thread"].as_array().unwrap()),
            vec!["let's write it up", "why twice?", "catch-up"]
        );
    }

    #[tokio::test]
    async fn the_move_into_route_maps_unknown_404_self_move_400_and_a_bad_body_400() {
        let fx = testutil::fixture();
        let source = seed_session(&fx, "src", None);
        let m = seed_message(&fx, &source.id, Role::User, "a", 1_000);
        let target = seed_session(&fx, "dst", None);
        let call = call(&fx);

        let missing = call
            .call(testutil::req(
                "POST",
                "/sessions/no-such-session/move-into",
                Some(j!({ "sourceId": source.id, "picks": [{ "messageId": m.id }] })),
            ))
            .await;
        assert_eq!(missing.status(), 404);

        let self_move = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/move-into", target.id),
                Some(j!({ "sourceId": target.id, "picks": [{ "messageId": m.id }] })),
            ))
            .await;
        assert_eq!(self_move.status(), 400);
        let body = testutil::body_json(self_move).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("source and target are both"),
            "{body}"
        );

        // No `sourceId` at all is the schema's 400, not the domain's.
        let malformed = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/move-into", target.id),
                Some(j!({ "picks": [{ "messageId": m.id }] })),
            ))
            .await;
        assert_eq!(malformed.status(), 400);
        let body = testutil::body_json(malformed).await;
        assert!(
            body["error"].as_str().unwrap().contains("invalid body"),
            "{body}"
        );
    }

    // ---- handoff (route half of `src/history/handoff.test.ts`) ---------------

    #[tokio::test]
    async fn post_handoff_answers_201_with_the_drafted_root_and_the_first_post_clears_the_draft() {
        let fx = with_llm(testutil::fixture(), "Pick up the relaunch path.");
        let source = seed_session(&fx, "the migration", None);
        seed_message(&fx, &source.id, Role::User, "migrate the journal", 1_000);
        let call = call(&fx);

        let res = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/handoff", source.id),
                Some(j!({ "goal": "finish the relaunch path" })),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let body = testutil::body_json(res).await;
        assert_eq!(body["session"]["draft"], "Pick up the relaunch path.");
        assert_eq!(body["session"]["kind"], "root");
        // Deliberately NO thread: a handoff seeds no messages and the new root
        // inherits none.
        assert!(body.get("thread").is_none(), "{body}");
        let created = body["session"]["id"].as_str().unwrap().to_string();

        // The user edits it and sends. Whatever they actually sent supersedes
        // the draft — the half of this contract that lives in `sessions.rs`.
        let posted = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{created}/messages"),
                Some(j!({ "text": "Pick up the relaunch path, but start with the tests." })),
            ))
            .await;
        assert_eq!(posted.status(), 202);
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .get_session(&created)
                .unwrap()
                .unwrap()
                .draft,
            None
        );
    }

    #[tokio::test]
    async fn the_handoff_route_maps_an_unknown_session_to_404_and_an_empty_goal_to_400() {
        let fx = with_llm(testutil::fixture(), "DRAFT");
        let source = seed_session(&fx, "t", None);
        seed_message(&fx, &source.id, Role::User, "a", 1_000);
        let call = call(&fx);

        let missing = call
            .call(testutil::req(
                "POST",
                "/sessions/no-such-session/handoff",
                Some(j!({"goal": "x"})),
            ))
            .await;
        assert_eq!(missing.status(), 404);

        let blank = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/handoff", source.id),
                Some(j!({ "goal": "" })),
            ))
            .await;
        assert_eq!(blank.status(), 400);
    }

    // ---- sections (route half of `src/history/sections.test.ts`) -------------

    #[tokio::test]
    async fn post_sections_returns_ranges_and_stores_nothing() {
        let fx = with_llm(
            testutil::fixture(),
            r#"[{"start":0,"end":1,"label":"auth token refresh"}]"#,
        );
        let s = seed_session(&fx, "the work", None);
        seed_message(&fx, &s.id, Role::User, "hello", 1_700_000_000_000);
        let before = serde_json::to_string(&j!({
            "sessions": fx.ctx.db.lock().unwrap().list_sessions().unwrap(),
            "messages": fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap(),
        }))
        .unwrap();

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/sections", s.id),
                Some(j!({ "turns": [{ "gist": "fix the refresh" }, { "gist": "still failing" }] })),
            ))
            .await;

        assert_eq!(res.status(), 200);
        assert_eq!(
            testutil::body_json(res).await,
            j!({ "sections": [{ "start": 0, "end": 1, "label": "auth token refresh" }] })
        );
        // A labeling pass is read-only — no session, no message, nothing stored.
        let after = serde_json::to_string(&j!({
            "sessions": fx.ctx.db.lock().unwrap().list_sessions().unwrap(),
            "messages": fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap(),
        }))
        .unwrap();
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn the_sections_route_refuses_an_unknown_session_and_an_empty_turn_list() {
        let fx = with_llm(testutil::fixture(), "[]");
        let call = call(&fx);

        let missing = call
            .call(testutil::req(
                "POST",
                "/sessions/nope/sections",
                Some(j!({ "turns": [{ "gist": "a" }] })),
            ))
            .await;
        assert_eq!(missing.status(), 404);
        assert_eq!(fx.llm_calls(), 0, "a stale id must not buy an LLM call");

        let s = seed_session(&fx, "the work", None);
        let empty = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/sections", s.id),
                Some(j!({ "turns": [] })),
            ))
            .await;
        assert_eq!(empty.status(), 400);
    }

    // ---- fork: the abandoned-path summary ------------------------------------

    #[tokio::test]
    async fn summarize_abandoned_seeds_the_branch_with_what_it_left_behind() {
        let fx = with_llm(testutil::fixture(), "they tried three dead ends");
        let s = fork_scenario(&fx);

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/fork", s.target.id),
                Some(j!({
                    "atMessageId": s.own[2].id,
                    "exclusive": true,
                    "summarizeAbandoned": true,
                })),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let branch = testutil::body_json(res).await["session"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let texts = own_texts(&fx, &branch);
        assert_eq!(
            texts.last().map(String::as_str),
            Some("Summary of the path this branch left behind:\n\nthey tried three dead ends")
        );
        // The abandoned span is everything from the fork point to the source's
        // END, and only that.
        let prompt = fx.llm_prompts()[0].clone();
        assert!(prompt.contains("second ask"), "{prompt}");
        assert!(prompt.contains("second answer"), "{prompt}");
        assert!(!prompt.contains("first ask"), "{prompt}");
    }

    #[tokio::test]
    async fn a_failed_abandoned_summary_never_fails_the_fork() {
        struct FailingLlm;
        #[async_trait::async_trait]
        impl bough_core::types::LlmClient for FailingLlm {
            async fn run(
                &self,
                _params: bough_core::types::LlmParams,
                _on_text: bough_core::types::OnText,
                _cancel: tokio_util::sync::CancellationToken,
            ) -> Result<bough_core::types::LlmResult, bough_core::errors::LlmError> {
                Err(bough_core::errors::LlmError::new("provider exploded"))
            }
        }

        let mut fx = testutil::fixture();
        // The summariser fails; the branch must still be a 201. (Injected
        // rather than left to provider routing — a test never reaches the
        // network, whatever keys the developer's env happens to hold.)
        fx.ctx.llm = Some(std::sync::Arc::new(FailingLlm));
        let s = fork_scenario(&fx);

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/fork", s.target.id),
                Some(j!({
                    "atMessageId": s.own[2].id,
                    "exclusive": true,
                    "summarizeAbandoned": true,
                })),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let branch = testutil::body_json(res).await["session"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        // Seeded copies only — no note, and no error.
        assert_eq!(own_texts(&fx, &branch), vec!["first ask", "first answer"]);
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
            vec![
                "ancestor question",
                "ancestor answer",
                "first ask",
                "first answer"
            ]
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
                call.call(testutil::req(
                    "POST",
                    &format!("/sessions/{id}/fork"),
                    Some(body),
                ))
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
            body["error"]
                .as_str()
                .unwrap()
                .contains("can only replace a user message"),
            "{body}"
        );

        let missing = post("no-such-session".into(), j!({"atMessageId": s.own[0].id})).await;
        assert_eq!(missing.status(), 404);

        // A body the schema rejects is the router's 400, not the domain's.
        let malformed = post(s.target.id.clone(), j!({"atPart": 1})).await;
        assert_eq!(malformed.status(), 400);
        let body = testutil::body_json(malformed).await;
        assert!(
            body["error"].as_str().unwrap().contains("invalid body"),
            "{body}"
        );
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
        assert_eq!(
            message.parts,
            vec![Part::Text {
                text: "try again".into()
            }]
        );
    }

    // ---- unsend (port of `src/history/unsend.test.ts`) ----------------------

    async fn post_unsend(
        call: &Dispatcher,
        id: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        call.call(testutil::req(
            "POST",
            &format!("/sessions/{id}/unsend"),
            Some(body),
        ))
        .await
    }

    #[tokio::test]
    async fn the_last_user_message_and_the_answer_it_provoked_are_gone_the_rest_untouched() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "chat", None);
        seed_message(&fx, &s.id, Role::User, "first ask", 1_000);
        seed_message(&fx, &s.id, Role::Supervisor, "first answer", 1_001);
        let retracted = seed_message(&fx, &s.id, Role::User, "the typo", 1_002);
        let partial = seed_message(
            &fx,
            &s.id,
            Role::Supervisor,
            "half an answer to a typo",
            1_003,
        );

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
        assert_eq!(
            own_texts(&fx, &s.id),
            vec!["first ask", "first answer", "second ask"]
        );
    }

    #[tokio::test]
    async fn a_supervisor_message_is_refused_the_models_turns_are_not_the_users_to_retract() {
        let fx = testutil::fixture();
        let s = seed_session(&fx, "chat", None);
        seed_message(&fx, &s.id, Role::User, "ask", 1_000);
        let answer = seed_message(&fx, &s.id, Role::Supervisor, "answer", 1_001);

        let res = post_unsend(&call(&fx), &s.id, j!({"atMessageId": answer.id})).await;
        assert_eq!(res.status(), 400);
        assert_eq!(
            fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap().len(),
            2
        );
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

        assert_eq!(
            post_unsend(&call, "nope", j!({"atMessageId": m.id}))
                .await
                .status(),
            404
        );
        assert_eq!(
            post_unsend(&call, &s.id, j!({"atMessageId": "nope"}))
                .await
                .status(),
            400
        );
        assert_eq!(
            fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap().len(),
            1
        );
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
        assert!(
            claim.cancel.is_cancelled(),
            "the turn's token must be cancelled"
        );
        assert!(fx
            .ctx
            .db
            .lock()
            .unwrap()
            .messages_for(&s.id)
            .unwrap()
            .is_empty());
    }
}
