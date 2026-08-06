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

/// `PUT /saved-workflows/:name` — 400 "not yet".
pub fn workflow_not_yet() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Err::<axum::response::Response, _>(not_yet())
    })
}

// ---- workflow settings ------------------------------------------------------
//
// These are CONSTANTS and environment reads, not the workflow engine: the size
// guideline, the thresholds derived from it, and the advice sentence. Gating
// them behind "not yet ported" was wrong twice over — a GET answering 400 is
// the wrong status for a well-formed request, and the TUI reads the guideline
// to label a run it is perfectly able to display. Engine-backed routes stay
// stubbed; this one answers for real (parity.sh pins it against the TS server).

/// `GUIDELINE_TARGET` (report.ts:355). `unrestricted` is `Infinity` in TS,
/// which serializes as `null` — the route emits `null` for it, hence `Option`.
fn guideline_target(guideline: &str) -> Option<i64> {
    match guideline {
        "small" => Some(5),
        "large" => Some(50),
        "unrestricted" => None,
        _ => Some(15), // medium, the default
    }
}

/// The stored setting, else `BOUGH_WORKFLOW_SIZE`, else `medium`. Read on every
/// call, never cached: its readers are view functions a route renders per
/// request, and a cache here would trade a staleness bug for nothing.
fn active_guideline() -> String {
    let parse = |raw: &str| -> Option<String> {
        let word = raw.trim();
        matches!(word, "small" | "medium" | "large" | "unrestricted")
            .then(|| word.to_string())
    };
    let stored = std::fs::read_to_string(bough_core::paths::workflows_dir().join("size-guideline"))
        .ok()
        .and_then(|raw| parse(&raw));
    stored
        .or_else(|| std::env::var("BOUGH_WORKFLOW_SIZE").ok().as_deref().and_then(parse))
        .unwrap_or_else(|| "medium".to_string())
}

/// The sentence handed to whoever writes the script (report.ts::guidelineAdvice),
/// verbatim — phrased as a target with an explicit override clause, because a
/// guideline the model reads as a hard cap under-fans a job that needs 200 agents.
fn guideline_advice(guideline: &str) -> String {
    let target = guideline_target(guideline)
        .map_or_else(|| "Infinity".to_string(), |n| n.to_string());
    format!(
        "Workflow size guideline: {guideline} — aim for fewer than {target} agents in a \
         generated script. This is advice, not a cap: if the request plainly needs a wider \
         fan-out, write it and say why."
    )
}

/// `BOUGH_WORKFLOW_TOKEN_WARN`, else 1_000_000 (report.ts::tokenWarnThreshold).
fn token_warn_threshold() -> i64 {
    std::env::var("BOUGH_WORKFLOW_TOKEN_WARN")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map_or(1_000_000, |n| n as i64)
}

/// Up to 16 at once, fewer on a small machine: two cores are left for
/// everything that is NOT a workflow agent (the turn runner, the program
/// worker, the subagent turns those spawn). `BOUGH_WORKFLOW_CONCURRENCY` moves it.
fn workflow_concurrency() -> i64 {
    if let Some(n) = std::env::var("BOUGH_WORKFLOW_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
    {
        return n as i64;
    }
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get() as i64);
    (cores - 2).clamp(1, 16)
}

/// A runaway-loop backstop set far above any real workflow (run.ts:163).
const MAX_AGENTS_PER_RUN: i64 = 1000;

/// `GET /workflow-settings` — the size guideline and the thresholds derived
/// from it. `advice` rides along because a client that shows the setting
/// without it turns a guideline into a mystery number.
pub fn get_workflow_settings() -> Handler {
    handler(|_req, _ctx, _params| async move {
        let guideline = active_guideline();
        Ok(json_res(
            &json!({
                "sizeGuideline": guideline,
                "target": guideline_target(&guideline),
                "advice": guideline_advice(&guideline),
                "tokenWarnThreshold": token_warn_threshold(),
                "concurrency": workflow_concurrency(),
                "maxAgentsPerRun": MAX_AGENTS_PER_RUN,
                "advisory": true,
            }),
            200,
        ))
    })
}

/// `PUT /workflow-settings` — persist the guideline and echo the same shape
/// back, so a client needs no second request to refresh what it just changed.
pub fn put_workflow_settings() -> Handler {
    handler(|req, _ctx, _params| async move {
        let body: serde_json::Value = crate::http::parse_body(req, None).await?;
        let obj = body.as_object().ok_or_else(|| BoughError::bad_request("expected an object"))?;
        if obj.len() != 1 || !obj.contains_key("sizeGuideline") {
            return Err(BoughError::bad_request(
                "expected { sizeGuideline } and nothing else",
            ));
        }
        let value = obj["sizeGuideline"].as_str().unwrap_or_default();
        if guideline_target(value).is_none() && value != "unrestricted" {
            return Err(BoughError::bad_request(format!(
                "unknown size guideline \"{value}\" — one of small, medium, large, unrestricted",
            )));
        }
        let dir = bough_core::paths::workflows_dir();
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(dir.join("size-guideline"), format!("{value}\n")))
            .map_err(|e| BoughError::bad_request(format!("could not store the guideline: {e}")))?;
        Ok(json_res(
            &json!({
                "sizeGuideline": value,
                "target": guideline_target(value),
                "advice": guideline_advice(value),
                "tokenWarnThreshold": token_warn_threshold(),
                "concurrency": workflow_concurrency(),
                "maxAgentsPerRun": MAX_AGENTS_PER_RUN,
                "advisory": true,
            }),
            200,
        ))
    })
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;

    /// The shape `workflows.ts::getWorkflowSettingsH` returns, key for key —
    /// parity.sh diffs this route against the live TS server.
    #[tokio::test]
    async fn the_settings_route_answers_the_guideline_and_its_derived_thresholds() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/workflow-settings")).await;
        assert_eq!(res.status(), 200, "a well-formed GET is never a 400");
        let body = testutil::body_json(res).await;
        for key in [
            "sizeGuideline",
            "target",
            "advice",
            "tokenWarnThreshold",
            "concurrency",
            "maxAgentsPerRun",
            "advisory",
        ] {
            assert!(body.get(key).is_some(), "missing {key}: {body}");
        }
        assert_eq!(body["advisory"], true, "the guideline is advice, never a cap");
        assert_eq!(body["maxAgentsPerRun"], 1000);
        let advice = body["advice"].as_str().unwrap();
        assert!(advice.starts_with("Workflow size guideline: "), "{advice}");
        assert!(advice.contains("advice, not a cap"), "{advice}");
    }

    #[test]
    fn the_guideline_targets_match_the_ts_table_and_unrestricted_is_null() {
        assert_eq!(guideline_target("small"), Some(5));
        assert_eq!(guideline_target("medium"), Some(15));
        assert_eq!(guideline_target("large"), Some(50));
        // `Infinity` is not representable in JSON; TS emits null, so do we.
        assert_eq!(guideline_target("unrestricted"), None);
    }

    #[test]
    fn concurrency_leaves_two_cores_for_everything_that_is_not_a_workflow_agent() {
        let n = workflow_concurrency();
        assert!((1..=16).contains(&n), "concurrency out of range: {n}");
    }
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
    async fn starting_a_run_is_400_not_yet_and_saved_reads_404() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req("POST", "/workflows", Some(j!({"sessionId": "s", "script": "x"}))))
            .await;
        assert_eq!(res.status(), 400);
        // `/workflow-settings` is NOT in this list: it is constants and env
        // reads, not the engine, and it answers for real (see settings_tests).
        let res = call.call(testutil::get("/saved-workflows/nightly")).await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"error": "no saved workflow \"nightly\""})
        );
    }
}
