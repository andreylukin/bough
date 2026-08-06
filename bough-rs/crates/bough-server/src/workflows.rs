//! Workflow routes (port of the route surface of `src/server/workflows.ts` +
//! `src/workflow/relaunch.ts`, wave-1 stub).
//!
//! v1-STUB per server.md §8: "all routes → 404/`{workflows: []}` until the
//! workflow subsystem lands" (wave 2, rows 2.9–2.13). No run can exist in
//! this process — nothing can create one (`POST /workflows` says so with a
//! 400) — so every `/workflows/:id/...` verb answers the TS unknown-run 404
//! (`workflow ${id} not found`, verbatim) and every `/saved-workflows/:name`
//! read answers "nothing saved". The one piece of real validation kept live
//! is the agent-action typo check, because its teaching message is product
//! surface and validating BEFORE the run lookup is the TS order.

use serde_json::json;

use bough_core::errors::BoughError;

use crate::http::{handler, json as json_res, Handler};

fn not_yet() -> BoughError {
    BoughError::bad_request("workflows are not yet ported in this build")
}

fn no_run(params: &crate::http::Params) -> BoughError {
    let id = params.get("id").map(String::as_str).unwrap_or("");
    BoughError::not_found(format!("workflow {id} not found"))
}

/// `GET /workflows[?session=|?sessionId=]` — `{workflows: []}`.
pub fn list_workflows() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Ok(json_res(&json!({ "workflows": [] }), 200))
    })
}

/// `POST /workflows` — 400 "not yet ported" (the only way a run could exist).
pub fn create_workflow() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Err::<axum::response::Response, _>(not_yet())
    })
}

/// Every `/workflows/:id`-scoped verb: the unknown-run 404 (none can exist).
/// Covers get/stop/pause/resume/rerun/relaunch/replay/save.
pub fn workflow_not_found() -> Handler {
    handler(|_req, _ctx, params| async move {
        Err::<axum::response::Response, _>(no_run(&params))
    })
}

/// `POST /workflows/:id/agents/:agentId/:action` — the action is validated
/// first (verbatim TS message: a typo must not silently become `stop`), then
/// the run lookup 404s.
pub fn control_workflow_agent() -> Handler {
    handler(|_req, _ctx, params| async move {
        let action = params.get("action").map(String::as_str).unwrap_or("");
        if action != "stop" && action != "restart" {
            return Err(BoughError::bad_request(format!(
                "unknown workflow agent action '{action}' — it is 'stop' (fail this one \
                 call, the run continues) or 'restart' (re-issue it on a fresh subagent \
                 session)",
            )));
        }
        Err::<axum::response::Response, _>(no_run(&params))
    })
}

/// `GET /saved-workflows` — `{saved: []}`.
pub fn list_saved_workflows() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Ok(json_res(&json!({ "saved": [] }), 200))
    })
}

/// `GET /saved-workflows/:name` and `POST /saved-workflows/:name/runs` —
/// nothing is saved, so every name is a 404.
pub fn saved_workflow_not_found() -> Handler {
    handler(|_req, _ctx, params| async move {
        let name = params.get("name").cloned().unwrap_or_default();
        Err::<axum::response::Response, _>(BoughError::not_found(format!(
            "no saved workflow \"{name}\"",
        )))
    })
}

/// `PUT /saved-workflows/:name`, `GET|PUT /workflow-settings` — 400 "not yet".
pub fn workflow_not_yet() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Err::<axum::response::Response, _>(not_yet())
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use serde_json::json as j;

    #[tokio::test]
    async fn listings_are_empty_and_run_scoped_verbs_are_the_unknown_run_404() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/workflows")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"workflows": []}));
        let res = call.call(testutil::get("/saved-workflows")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"saved": []}));

        for (method, path) in [
            ("GET", "/workflows/wf_1"),
            ("POST", "/workflows/wf_1/stop"),
            ("POST", "/workflows/wf_1/pause"),
            ("POST", "/workflows/wf_1/resume"),
            ("POST", "/workflows/wf_1/rerun"),
            ("POST", "/workflows/wf_1/relaunch"),
            ("GET", "/workflows/wf_1/replay"),
            ("POST", "/workflows/wf_1/save"),
        ] {
            let res = call.call(testutil::req(method, path, None)).await;
            assert_eq!(res.status(), 404, "{method} {path}");
            let body = testutil::body_json(res).await;
            assert_eq!(body["error"], "workflow wf_1 not found", "{method} {path}");
        }
    }

    #[tokio::test]
    async fn an_agent_action_typo_is_a_400_teaching_both_verbs_before_the_run_404() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req("POST", "/workflows/wf_1/agents/a1/pause", None))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("unknown workflow agent action 'pause'"), "{msg}");
        assert!(msg.contains("'stop'"), "{msg}");
        assert!(msg.contains("'restart'"), "{msg}");
        // A valid action reaches the run lookup and 404s (no run can exist).
        let res = call
            .call(testutil::req("POST", "/workflows/wf_1/agents/a1/stop", None))
            .await;
        assert_eq!(res.status(), 404);
    }

    #[tokio::test]
    async fn create_and_settings_are_400_not_yet_and_saved_reads_404() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req("POST", "/workflows", Some(j!({"sessionId": "s", "script": "x"}))))
            .await;
        assert_eq!(res.status(), 400);
        let res = call.call(testutil::get("/workflow-settings")).await;
        assert_eq!(res.status(), 400);
        let res = call.call(testutil::get("/saved-workflows/nightly")).await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"error": "no saved workflow \"nightly\""})
        );
    }
}
