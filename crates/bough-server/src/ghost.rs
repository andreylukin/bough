//! `POST /sessions/:id/ghost` — composer ghost text (route port of
//! `src/worker/ghost.ts`'s `ghostTextH`; the shaping and the tier method live
//! in `bough_core::worker::ghost`).
//!
//! ALWAYS 200 for a session that exists, `{ghost: null}` standing in for
//! every failure there is — a cheap-model outcome must never reach the
//! composer as an error banner (a 5xx would put a red banner on a feature
//! whose entire value is that you can ignore it). POST rather than GET
//! because the half-typed prefix is user text that has no business in a URL
//! or a log. 404 is the only failure: an unknown session id is a real client
//! bug worth surfacing.

use serde::Deserialize;
use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::worker::ghost::ghost_for;

use crate::http::{handler, json as json_res, parse_body, Handler};

/// `{prefix?}` — what the user has already typed; the model continues it.
/// Strict, like the TS zod schema: an unknown key is a 400.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GhostBody {
    #[serde(default)]
    prefix: Option<String>,
}

/// `POST /sessions/:id/ghost` — `{ghost: string|null}`.
pub fn ghost_text() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        if ctx.db.lock().unwrap().get_session(&id)?.is_none() {
            return Err(BoughError::not_found(format!("session {id} not found")));
        }
        let body: GhostBody = parse_body(req, Some(json!({}))).await?;
        let ghost = ghost_for(
            &ctx.db,
            ctx.cheap.as_ref(),
            &id,
            body.prefix.as_deref().unwrap_or(""),
        )
        .await?;
        Ok(json_res(&json!({ "ghost": ghost }), 200))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::schema::parts::{Message, Part, Role, Session, SessionKind};
    use bough_core::types::CheapTier;
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

    fn say(fx: &testutil::Fixture, session_id: &str, role: Role, text: &str) {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst) as i64;
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role,
                parts: vec![Part::Text { text: text.into() }],
                pending: false,
                created_at: (fx.ctx.now)() + n,
            })
            .unwrap();
    }

    /// A tier that answers a canned ghost and records the prompt it was
    /// handed; `title`/`activity` answer nothing.
    struct Tier {
        ghost: Option<String>,
        calls: AtomicUsize,
        seen: Mutex<String>,
    }
    impl Tier {
        fn new(ghost: Option<&str>) -> Arc<Tier> {
            Arc::new(Tier {
                ghost: ghost.map(String::from),
                calls: AtomicUsize::new(0),
                seen: Mutex::new(String::new()),
            })
        }
    }
    #[async_trait::async_trait]
    impl CheapTier for Tier {
        async fn title(&self, _f: &str, _glossary: &[String]) -> Option<String> {
            None
        }
        async fn ghost_text(&self, prompt: &str) -> Option<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.seen.lock().unwrap() = prompt.to_string();
            self.ghost.clone()
        }
        async fn activity(&self, _r: &str) -> Option<String> {
            None
        }
    }

    #[tokio::test]
    async fn a_session_with_history_gets_a_suggestion() {
        let mut fx = testutil::fixture();
        fx.ctx.cheap = Some(Tier::new(Some("run the tests")));
        let id = seed_session(&fx);
        say(&fx, &id, Role::User, "add the theme route");
        say(
            &fx,
            &id,
            Role::Supervisor,
            "added it; the tests are not run yet",
        );
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/ghost"),
                None,
            ))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"ghost": "run the tests"})
        );
    }

    #[tokio::test]
    async fn an_empty_conversation_is_a_null_ghost_and_buys_nothing() {
        let mut fx = testutil::fixture();
        let tier = Tier::new(Some("nope"));
        fx.ctx.cheap = Some(tier.clone());
        let id = seed_session(&fx);
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/ghost"),
                None,
            ))
            .await;
        assert_eq!(testutil::body_json(res).await, j!({"ghost": null}));
        assert_eq!(
            tier.calls.load(Ordering::SeqCst),
            0,
            "there is nothing to predict from"
        );
    }

    #[tokio::test]
    async fn the_typed_prefix_reaches_the_model() {
        let mut fx = testutil::fixture();
        let tier = Tier::new(Some("run the tests"));
        fx.ctx.cheap = Some(tier.clone());
        let id = seed_session(&fx);
        say(&fx, &id, Role::User, "add the theme route");
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        call.call(testutil::req(
            "POST",
            &format!("/sessions/{id}/ghost"),
            Some(j!({"prefix": "run the"})),
        ))
        .await;
        let seen = tier.seen.lock().unwrap().clone();
        assert!(seen.contains("has started typing: run the"), "{seen}");
    }

    #[tokio::test]
    async fn ghost_is_always_200_with_null_when_no_cheap_tier_exists() {
        let fx = testutil::fixture();
        let id = seed_session(&fx);
        say(&fx, &id, Role::User, "hello");
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
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/ghost"),
                None,
            ))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"ghost": null}));
    }

    #[tokio::test]
    async fn a_panicking_cheap_tier_is_200_ghost_null_never_a_5xx() {
        struct Panicking;
        #[async_trait::async_trait]
        impl CheapTier for Panicking {
            async fn title(&self, _f: &str, _glossary: &[String]) -> Option<String> {
                None
            }
            async fn ghost_text(&self, _p: &str) -> Option<String> {
                panic!("provider is down")
            }
            async fn activity(&self, _r: &str) -> Option<String> {
                None
            }
        }
        let mut fx = testutil::fixture();
        fx.ctx.cheap = Some(Arc::new(Panicking));
        let id = seed_session(&fx);
        say(&fx, &id, Role::User, "add the theme route");
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/ghost"),
                None,
            ))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"ghost": null}));
    }

    #[tokio::test]
    async fn an_unknown_session_is_a_404_not_a_null_ghost() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                "/sessions/nope/ghost",
                Some(j!({"prefix": "x"})),
            ))
            .await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"error": "session nope not found"})
        );
    }
}
