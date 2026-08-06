//! The Changes rail (port of `src/server/changes.ts`, wave-1 stub).
//!
//! v1-STUB per server.md §8: `{available: false, reason: "…", …}` is a legal
//! permanent answer — GET is ALWAYS 200, because "no change set" is an answer
//! about a healthy session, not an error (the difference travels in
//! `available` + `reason`, never in a status code). The git layer
//! (`vcs/repodiff`) lands in wave 2 row 2.14; until then the reason says so.
//!
//! What is ported for real, because clients branch on it:
//! - the session check with the TS 404 message (verbatim);
//! - the TS no-workspace reason (verbatim) — that case's answer is already
//!   final, not a stub;
//! - revert's `paths: []` refusal (verbatim) — an explicit empty selection
//!   must NEVER mean "revert everything", and that guard must exist from the
//!   first build that answers this route;
//! - revert-with-nothing-to-revert-against as 400 carrying the rail's own
//!   reason (`nothing to revert: …`).

use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::schema::requests::RevertChangesBody;
use bough_core::types::AppCtx;

use crate::http::{handler, json as json_res, parse_body, Handler};

fn require_session(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    match ctx.db.lock().unwrap().get_session(id)? {
        Some(_) => Ok(()),
        None => Err(BoughError::not_found(format!(
            "no session {id} — changes are per session, so open one that exists \
             (GET /sessions lists them).",
        ))),
    }
}

/// The rail's payload for one session, in the unavailable shape. The
/// no-workspace reason is the TS text verbatim; a session WITH a workspace
/// gets the honest not-yet reason instead of a fabricated diff.
fn session_changes(ctx: &AppCtx, id: &str) -> Result<serde_json::Value, BoughError> {
    let runtime = ctx.db.lock().unwrap().get_session_runtime(id)?;
    let (reason, workspace) = match runtime.workspace {
        None => (
            "this session has no workspace, so there is no checkout to diff. Create a \
             session with a `workspace` to get a Changes rail."
                .to_string(),
            serde_json::Value::Null,
        ),
        Some(ws) => (
            "the Changes rail is not yet ported in this build".to_string(),
            json!(ws),
        ),
    };
    Ok(json!({
        "available": false,
        "reason": reason,
        "base": null,
        "files": [],
        "workspace": workspace,
    }))
}

/// `GET /sessions/:id/changes` — always 200.
pub fn get_changes() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        Ok(json_res(&session_changes(&ctx, &id)?, 200))
    })
}

/// `POST /sessions/:id/changes/revert` — with no change set, every revert is
/// the 400 carrying the rail's own reason; `paths: []` is refused first.
pub fn revert_changes() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        let body: RevertChangesBody = parse_body(req, Some(json!({}))).await?;
        if body.paths.as_ref().is_some_and(|p| p.is_empty()) {
            return Err(BoughError::bad_request(
                "revert was given an empty `paths` selection, so it reverted nothing. An \
                 empty list is not a wildcard — it is almost always a client that selected \
                 no rows, and revert deletes files. To revert one or more paths, name them; \
                 to revert the WHOLE change set, omit `paths` from the body entirely.",
            ));
        }
        let set = session_changes(&ctx, &id)?;
        let reason = set["reason"].as_str().unwrap_or("no change set");
        Err::<axum::response::Response, _>(BoughError::bad_request(format!(
            "nothing to revert: {reason}",
        )))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::schema::parts::{Session, SessionKind};
    use serde_json::json as j;

    fn seed_session(fx: &testutil::Fixture, workspace: Option<&str>) -> String {
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
                workspace: workspace.map(str::to_string),
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
    async fn no_workspace_is_a_200_unavailable_answer_with_the_ts_reason() {
        let fx = testutil::fixture();
        let id = seed_session(&fx, None);
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get(&format!("/sessions/{id}/changes"))).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["available"], false);
        assert!(body["reason"].as_str().unwrap().contains("no workspace"), "{body}");
        assert!(body["base"].is_null());
        assert_eq!(body["files"], j!([]));
        assert!(body["workspace"].is_null());
    }

    #[tokio::test]
    async fn a_workspace_session_still_answers_200_with_its_workspace_named() {
        let fx = testutil::fixture();
        let id = seed_session(&fx, Some("/tmp"));
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get(&format!("/sessions/{id}/changes"))).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["available"], false);
        assert_eq!(body["workspace"], "/tmp");
    }

    #[tokio::test]
    async fn an_unknown_session_is_the_ts_404() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/sessions/nope/changes")).await;
        assert_eq!(res.status(), 404);
        let body = testutil::body_json(res).await;
        assert!(body["error"].as_str().unwrap().contains("changes are per session"), "{body}");
    }

    #[tokio::test]
    async fn an_explicit_empty_paths_selection_is_refused_never_a_wildcard() {
        let fx = testutil::fixture();
        let id = seed_session(&fx, Some("/tmp"));
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/changes/revert"),
                Some(j!({"paths": []})),
            ))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("empty `paths` selection"), "{msg}");
        assert!(msg.contains("not a wildcard"), "{msg}");
    }

    #[tokio::test]
    async fn a_revert_with_no_change_set_is_a_400_carrying_the_rails_reason() {
        let fx = testutil::fixture();
        let id = seed_session(&fx, None);
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        // Absent paths (revert-all) and named paths land in the same 400.
        for body in [None, Some(j!({"paths": ["src/a.ts"]}))] {
            let res = call
                .call(testutil::req("POST", &format!("/sessions/{id}/changes/revert"), body))
                .await;
            assert_eq!(res.status(), 400);
            let parsed = testutil::body_json(res).await;
            assert!(
                parsed["error"].as_str().unwrap().starts_with("nothing to revert: "),
                "{parsed}"
            );
        }
    }
}
