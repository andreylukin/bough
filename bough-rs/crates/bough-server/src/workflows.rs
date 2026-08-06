//! The workflow REST surface (port of `src/server/workflows.ts` + the two
//! handlers in `src/workflow/relaunch.ts`).
//!
//! Every handler is a thin translation: parse the body with the frozen schema,
//! call one function in the workflow layer, answer with what it returned.
//! Domain failures arrive as `BoughError` and are rendered by the single catch
//! in `app.rs`, so there is not one try/catch in this file.
//!
//! Why a start answers 201 and the verbs answer 200: a start CREATES a run and
//! hands back its row — immediately, because the script is detached from the
//! request that started it. The response is the receipt, not the result;
//! progress arrives on `/events` as `workflow.updated` / `workflow.agent` /
//! `workflow.log`, and completion posts a system note into the owning session.
//!
//! `/saved-workflows` is a TOP-LEVEL collection rather than `/workflows/saved`
//! because the route table is first-match in append order: `/workflows/saved`
//! would be swallowed by `/workflows/:id` above it and answer 404 for a run id
//! of "saved". The same reasoning puts the guideline at `/workflow-settings`.
//!
//! PORT STATUS (rows 3.10 + 3.12). Live for real: the whole saved-workflow
//! surface, `GET /workflows/:id/replay`, and `/workflow-settings`. Still
//! answering the unknown-run 404: everything that needs
//! `workflow::engine::start_workflow` / `stop_workflow` / `workflow_summary`
//! (row 3.9), which is not landed. Those are NOT faked — a 201 for a run that
//! was never started is the exact failure the replay accounting exists to
//! prevent — and no run can exist in this process, so the 404 is the honest
//! answer rather than a placeholder.

use serde::Deserialize;
use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::types::AppCtx;
use bough_core::workflow::control::workflow_detail;
use bough_core::workflow::engine::{
    pause_workflow, resume_workflow, stop_workflow, workflow_summary,
};
use bough_core::workflow::relaunch::relaunch_report;
use bough_core::workflow::report::{
    active_guideline, guideline_advice, set_guideline, token_warn_threshold, SizeGuideline,
};
use bough_core::workflow::saved::{
    list_saved_workflows, read_saved_workflow, save_run_as, save_workflow,
};

use crate::http::{handler, json as json_res, parse_body, Handler, Params};

/// 404 naming the id, so a client's log says which run was wrong.
fn require_workflow(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    ctx.db
        .lock()
        .unwrap()
        .get_workflow(id)?
        .map(|_| ())
        .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))
}

fn no_run(params: &Params) -> BoughError {
    let id = params.get("id").map(String::as_str).unwrap_or("");
    BoughError::not_found(format!("workflow {id} not found"))
}

fn name_of(params: &Params) -> String {
    params.get("name").cloned().unwrap_or_default()
}

// ---- runs (engine-backed; row 3.9) -----------------------------------------

/// `GET /workflows[?session=|?sessionId=]` — every run, newest first.
///
/// Summaries, not rows: the script text is the largest field by far and a list
/// carrying N copies of it is a payload nobody reads.
pub fn list_workflows() -> Handler {
    handler(|req, ctx, _params| async move {
        let query = req.uri().query().unwrap_or("").to_string();
        let session = query_param(&query, "session").or_else(|| query_param(&query, "sessionId"));
        let runs = ctx.db.lock().unwrap().list_workflows(session.as_deref())?;
        let summaries: Vec<_> = runs.iter().map(|r| workflow_summary(&ctx.db, r)).collect();
        Ok(json_res(&json!({ "workflows": summaries }), 200))
    })
}

/// One `key=value` out of a raw query string, percent-decoded for the two
/// escapes a session id can carry. The route table hands params raw and this is
/// the only query read in this module.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.replace('+', " ").replace("%2F", "/"))
    })
}

/// `GET /workflows/:id` — the run, its agents with live activity, and the
/// script file. The reconnect path for a run, the way `GET /sessions/:id` is
/// for a session.
pub fn get_workflow() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let run = ctx
            .db
            .lock()
            .unwrap()
            .get_workflow(&id)?
            .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))?;
        // The live-agent registry is `control.rs`'s un-ported half (row 3.10);
        // an empty set means every row reads `live: false`, which is the truth
        // in a process holding no control handles.
        Ok(json_res(
            &workflow_detail(&ctx.db, &run, &[], (ctx.now)())?,
            200,
        ))
    })
}

/// `POST /workflows/:id/stop` — kill the worker AND interrupt every subagent
/// turn the run started.
///
/// Both halves, always: terminating the worker only stops the script, and a
/// stop that left four subagents running would leave a fan-out billing with
/// nobody reading it. The interrupt travels on the run's abort signal, which
/// the engine cascades into each child's turn.
///
/// Idempotent on a run that is already finished: it answers with the row rather
/// than 409ing, because "stop" on something already stopped is the state the
/// caller wanted.
pub fn stop_workflow_route() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_workflow(&ctx, &id)?;
        Ok(json_res(
            &stop_workflow(&ctx.db, &ctx.bus, Some(&ctx.now), &id)?,
            200,
        ))
    })
}

/// `POST /workflows/:id/pause` — gate NEW `agent()` calls; the ones in flight
/// finish.
///
/// A 409 on a run that is not live in this process is the honest answer, not a
/// courtesy 200: pausing is an instruction to a running worker, and there is no
/// worker to instruct. The 404 comes first, so an unknown id never reads as
/// "running somewhere else".
pub fn pause_workflow_route() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_workflow(&ctx, &id)?;
        Ok(json_res(&pause_workflow(&ctx.db, &ctx.bus, &id)?, 200))
    })
}

/// `POST /workflows/:id/resume` — open the gate; parked calls release FIFO.
pub fn resume_workflow_route() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_workflow(&ctx, &id)?;
        Ok(json_res(&resume_workflow(&ctx.db, &ctx.bus, &id)?, 200))
    })
}

/// `POST /workflows` — 400 "not yet ported" (the only way a run could exist).
pub fn create_workflow() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Err::<axum::response::Response, _>(BoughError::bad_request(
            "workflows are not yet ported in this build",
        ))
    })
}

/// The `/workflows/:id`-scoped verbs that still need `control.rs`'s submit
/// boundary (`start_workflow_run` / `rerun_workflow_run`): rerun and relaunch.
pub fn workflow_not_found() -> Handler {
    handler(|_req, _ctx, params| async move { Err::<axum::response::Response, _>(no_run(&params)) })
}

/// `POST /workflows/:id/agents/:agentId/:action` — the action is validated
/// first (a typo must not silently become `stop`), then the run lookup 404s.
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

/// `GET /workflows/:id/replay` — how many of this run's calls were served from
/// a journal and how many cost an agent.
///
/// Its own endpoint rather than a field on the run, because it answers a
/// question about MONEY and must be readable WHILE the run is going: an audit
/// that is replaying nothing is worth catching at call 3, not in the bill. It
/// is a pure fold over rows, so it answers for real with no engine in the
/// process — `relaunch_report` 404s the unknown run itself.
pub fn workflow_replay() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        Ok(json_res(&relaunch_report(&ctx.db, &id)?.to_json(), 200))
    })
}

// ---- saved workflows (row 3.12) --------------------------------------------

/// `POST /workflows/:id/save` — keep this run's script as a named workflow.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SaveWorkflowBody {
    name: String,
}

/// `PUT /saved-workflows/:name` — save a script, or the script a run ran.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PutSavedBody {
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
}

/// `POST /saved-workflows/:name/runs` — invoke a saved workflow, parameterized.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[allow(dead_code)] // both fields are read by `start_workflow` (row 3.9); today
                    // they are the request VALIDATION — a body with no
                    // `sessionId` must be a 400 before the name is resolved.
struct RunSavedBody {
    session_id: String,
    #[serde(default)]
    args: Option<serde_json::Value>,
}

/// `PUT /workflow-settings` — the size guideline. Advice to the script's author.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SettingsBody {
    /// `z.unknown()` in TS: present-but-anything, and absent parses too (the
    /// 400 then quotes it as `undefined`).
    #[serde(default)]
    size_guideline: serde_json::Value,
}

/// `POST /workflows/:id/save` — save a finished run's script under a name.
///
/// The script saved is the one the run would relaunch: the edited mirror if
/// there is one, else the stored row. Saving the row instead would quietly save
/// the version the user replaced — the opposite of "the script that did what
/// you wanted".
pub fn save_workflow_route() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_workflow(&ctx, &id)?;
        let body: SaveWorkflowBody = parse_body(req, None).await?;
        let saved = save_run_as(&ctx.db, &id, &body.name, (ctx.now)())?;
        Ok(json_res(&saved, 201))
    })
}

/// `GET /saved-workflows` — every named workflow, with its `meta.description`.
pub fn list_saved_workflows_route() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Ok(json_res(&json!({ "saved": list_saved_workflows() }), 200))
    })
}

/// `GET /saved-workflows/:name` — one saved workflow, script included.
pub fn get_saved_workflow() -> Handler {
    handler(|_req, _ctx, params| async move {
        Ok(json_res(&read_saved_workflow(&name_of(&params))?, 200))
    })
}

/// `PUT /saved-workflows/:name` — save a script directly, or copy a run's.
///
/// Idempotent on the name: a saved workflow is a command, not a version
/// history. The run's own journal is where history lives.
pub fn put_saved_workflow() -> Handler {
    handler(|req, ctx, params| async move {
        let name = name_of(&params);
        let body: PutSavedBody = parse_body(req, Some(json!({}))).await?;
        if let Some(run_id) = body.run_id.filter(|s| !s.is_empty()) {
            let saved = save_run_as(&ctx.db, &run_id, &name, (ctx.now)())?;
            return Ok(json_res(&saved, 201));
        }
        let script = body.script.filter(|s| !s.is_empty()).ok_or_else(|| {
            BoughError::bad_request(
                "PUT /saved-workflows/:name needs {script} or {runId} — the script to save, \
                 or the finished run whose script to save.",
            )
        })?;
        Ok(json_res(&save_workflow(&name, &script, (ctx.now)())?, 201))
    })
}

/// `POST /saved-workflows/:name/runs` — invoke a saved workflow by name.
///
/// `args` is the parameterization: the same orchestration against a different
/// branch, a different file list, a different threshold. A new run every time,
/// with no `resumeOf` — invoking a saved workflow is not a relaunch of
/// anything, and nothing replays.
///
/// The saved-workflow read happens FIRST, which is the TS order and the half
/// that is ported: an unknown name is a 404 naming it, and only a name that
/// resolves reaches the engine (row 3.9) that is not here yet.
pub fn run_saved_workflow() -> Handler {
    handler(|req, _ctx, params| async move {
        let name = name_of(&params);
        let _body: RunSavedBody = parse_body(req, None).await?;
        let _saved = read_saved_workflow(&name)?;
        Err::<axum::response::Response, _>(BoughError::bad_request(
            "workflows are not yet ported in this build",
        ))
    })
}

// ---- workflow settings ------------------------------------------------------

/// A runaway-loop backstop set far above any real workflow (`run.ts:163`).
/// Lives with the engine in TS; quoted here because the settings route is the
/// only thing that reads it and the engine is not landed.
const MAX_AGENTS_PER_RUN: i64 = 1000;

/// Up to 16 at once, fewer on a small machine: two cores are left for
/// everything that is NOT a workflow agent (the turn runner, the program
/// worker, the subagent turns those spawn). `BOUGH_WORKFLOW_CONCURRENCY` moves
/// it.
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

/// The GET body. `advice` rides along because a client that shows the setting
/// without it turns a guideline into a mystery number.
fn settings_body(g: SizeGuideline) -> serde_json::Value {
    json!({
        "sizeGuideline": g.as_str(),
        "target": g.target(),
        "advice": guideline_advice(g),
        "tokenWarnThreshold": token_warn_threshold(),
        "concurrency": workflow_concurrency(),
        "maxAgentsPerRun": MAX_AGENTS_PER_RUN,
        "advisory": true,
    })
}

/// `GET /workflow-settings` — the size guideline and the thresholds derived
/// from it.
pub fn get_workflow_settings() -> Handler {
    handler(
        |_req, _ctx, _params| async move { Ok(json_res(&settings_body(active_guideline()), 200)) },
    )
}

/// `PUT /workflow-settings` — set the size guideline.
///
/// It changes what the next script is ADVISED to aim for and what the run view
/// flags. It caps nothing: no run is refused, paused or throttled by this
/// value, and a run already flagged stays exactly as fast as it was.
///
/// The response is the GET body MINUS `concurrency` and `maxAgentsPerRun` —
/// TS's `putWorkflowSettingsH` omits both, because neither is a thing this
/// request changed. Echoing them here would put two shapes on one route name.
pub fn put_workflow_settings() -> Handler {
    handler(|req, _ctx, _params| async move {
        let body: SettingsBody = parse_body(req, Some(json!({}))).await?;
        let guideline = set_guideline(&body.size_guideline)?;
        Ok(json_res(
            &json!({
                "sizeGuideline": guideline.as_str(),
                "target": guideline.target(),
                "advice": guideline_advice(guideline),
                "tokenWarnThreshold": token_warn_threshold(),
                "advisory": true,
            }),
            200,
        ))
    })
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::schema::parts::{WorkflowRun, WorkflowStatus};
    use serde_json::json as j;

    /// Relocates `BOUGH_HOME` for the duration. Handler tests here touch the
    /// saved store, so they must not write into the developer's real `~/.bough`.
    struct TempHome {
        path: std::path::PathBuf,
        prior: Option<String>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl TempHome {
        fn new() -> TempHome {
            let guard = testutil::home_lock();
            let path =
                std::env::temp_dir().join(format!("bough-wfroutes-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            let prior = std::env::var("BOUGH_HOME").ok();
            std::env::set_var("BOUGH_HOME", &path);
            TempHome {
                path,
                prior,
                _guard: guard,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var("BOUGH_HOME", v),
                None => std::env::remove_var("BOUGH_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// A root session, straight into the fixture's in-memory database.
    fn seed_session(fx: &testutil::Fixture) -> String {
        use bough_core::schema::parts::{Session, SessionKind};
        let id = uuid::Uuid::new_v4().to_string();
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: id.clone(),
                title: "s".into(),
                kind: SessionKind::Root,
                created_at: 1,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some("/tmp/w".into()),
                origin_dir: Some("/tmp/w".into()),
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

    fn seed_run(fx: &testutil::Fixture, id: &str, script: &str) {
        let session_id = seed_session(fx);
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_workflow(WorkflowRun {
                id: id.into(),
                session_id,
                name: "branch-review".into(),
                description: "review a branch".into(),
                script: script.into(),
                phases: vec![],
                status: WorkflowStatus::Done,
                current_phase: None,
                result: None,
                error: None,
                args: None,
                resume_of: None,
                created_at: 1,
                finished_at: Some(2),
            })
            .unwrap();
    }

    const META: &str =
        "export const meta = { name: 'branch-review', description: 'review a branch' }\n";

    /// The whole saved surface over HTTP: PUT writes, GET lists with the
    /// description off the script's `meta`, GET by name carries the script, and
    /// an unknown name is a 404 that names it.
    #[tokio::test]
    async fn the_saved_workflow_surface_answers_over_http() {
        let _home = TempHome::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        // Nothing saved yet — an empty listing, not an error.
        let res = call.call(testutil::get("/saved-workflows")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({ "saved": [] }));

        // PUT {script} → 201 with the meta description read back.
        let res = call
            .call(testutil::req(
                "PUT",
                "/saved-workflows/branch-review",
                Some(j!({ "script": format!("{META}return 1") })),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let body = testutil::body_json(res).await;
        assert_eq!(body["name"], "branch-review");
        assert_eq!(body["description"], "review a branch");
        assert!(
            body["path"]
                .as_str()
                .unwrap()
                .ends_with("/saved/branch-review.js"),
            "{body}"
        );
        assert!(body["bytes"].as_u64().unwrap() > 0);

        // The listing now carries it.
        let res = call.call(testutil::get("/saved-workflows")).await;
        let body = testutil::body_json(res).await;
        assert_eq!(body["saved"].as_array().unwrap().len(), 1);
        assert_eq!(body["saved"][0]["description"], "review a branch");
        // …and never the script (that is the detail read).
        assert!(body["saved"][0].get("script").is_none(), "{body}");

        // GET by name carries the script.
        let res = call
            .call(testutil::get("/saved-workflows/branch-review"))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert!(
            body["script"].as_str().unwrap().contains("return 1"),
            "{body}"
        );

        // A trailing `.js` names the same workflow, not `branch-review.js.js`.
        let res = call
            .call(testutil::get("/saved-workflows/branch-review.js"))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await["name"], "branch-review");

        // An unknown name is a 404 naming it and pointing at the listing.
        let res = call.call(testutil::get("/saved-workflows/nightly")).await;
        assert_eq!(res.status(), 404);
        let msg = testutil::body_json(res).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            msg.starts_with("no saved workflow named \"nightly\" — "),
            "{msg}"
        );
        assert!(
            msg.contains("GET /saved-workflows lists what is saved"),
            "{msg}"
        );
    }

    /// The invariant, asserted where the name actually arrives: through the
    /// URL, on the route. A name is one file inside `saved/` or it is a 400.
    #[tokio::test]
    async fn a_name_in_the_url_cannot_address_a_file_outside_the_saved_dir() {
        let _home = TempHome::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        for name in ["..", ".hidden", "%2e%2e", "a..b/c", "-dash"] {
            let path = format!("/saved-workflows/{name}");
            let res = call.call(testutil::get(&path)).await;
            assert!(
                res.status() == 400 || res.status() == 404,
                "{path} answered {}",
                res.status()
            );
            if res.status() == 400 {
                let msg = testutil::body_json(res).await["error"]
                    .as_str()
                    .unwrap()
                    .to_string();
                assert!(msg.contains("saved workflow name"), "{path}: {msg}");
            }
        }
        // And nothing escaped onto disk.
        assert!(!_home.path.join("workflows/saved/../escaped.js").exists());
    }

    /// PUT is strict on its body: `{script}` or `{runId}`, nothing else, and a
    /// body with neither is a 400 that names both.
    #[tokio::test]
    async fn put_takes_script_or_run_id_and_refuses_anything_else() {
        let _home = TempHome::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        // Neither.
        let res = call
            .call(testutil::req("PUT", "/saved-workflows/x", Some(j!({}))))
            .await;
        assert_eq!(res.status(), 400);
        let msg = testutil::body_json(res).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.contains("{script} or {runId}"), "{msg}");

        // An unknown key is refused rather than ignored.
        let res = call
            .call(testutil::req(
                "PUT",
                "/saved-workflows/x",
                Some(j!({ "scripts": "return 1" })),
            ))
            .await;
        assert_eq!(
            res.status(),
            400,
            "an unknown key must not be silently dropped"
        );

        // {runId} saves the script that run would relaunch — the MIRROR when
        // there is one, which is what makes "edit the file, save it" work.
        seed_run(&fx, "run-1", &format!("{META}return 'the row version'"));
        std::fs::create_dir_all(bough_core::paths::workflows_dir()).unwrap();
        std::fs::write(
            bough_core::paths::workflow_script_path("run-1"),
            format!("{META}return 'the EDITED version'"),
        )
        .unwrap();
        let res = call
            .call(testutil::req(
                "PUT",
                "/saved-workflows/from-run",
                Some(j!({"runId": "run-1"})),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let res = call.call(testutil::get("/saved-workflows/from-run")).await;
        let script = testutil::body_json(res).await["script"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(script.contains("the EDITED version"), "{script}");

        // An unknown runId is the run 404, not a saved-workflow 404.
        let res = call
            .call(testutil::req(
                "PUT",
                "/saved-workflows/y",
                Some(j!({"runId": "nope"})),
            ))
            .await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await["error"],
            "workflow nope not found"
        );
    }

    /// `POST /workflows/:id/save` looks the run up for real, so it 404s an
    /// unknown id and saves a real one.
    #[tokio::test]
    async fn saving_a_run_by_id_looks_the_run_up_for_real() {
        let _home = TempHome::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let res = call
            .call(testutil::req(
                "POST",
                "/workflows/nope/save",
                Some(j!({"name": "x"})),
            ))
            .await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await["error"],
            "workflow nope not found"
        );

        seed_run(&fx, "run-2", &format!("{META}return 2"));
        let res = call
            .call(testutil::req(
                "POST",
                "/workflows/run-2/save",
                Some(j!({"name": "kept"})),
            ))
            .await;
        assert_eq!(res.status(), 201);
        assert_eq!(testutil::body_json(res).await["name"], "kept");
        let res = call.call(testutil::get("/saved-workflows/kept")).await;
        assert!(testutil::body_json(res).await["script"]
            .as_str()
            .unwrap()
            .contains("return 2"));
    }

    /// The route table's first-match order: `/saved-workflows` is its own
    /// collection, and `/workflows/saved` is a RUN id of "saved" — which is
    /// exactly why the saved collection is not mounted there.
    #[tokio::test]
    async fn saved_workflows_is_top_level_and_workflows_saved_is_a_run_id() {
        let _home = TempHome::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        // The collection answers.
        let res = call.call(testutil::get("/saved-workflows")).await;
        assert_eq!(res.status(), 200);
        assert!(testutil::body_json(res).await.get("saved").is_some());

        // `/workflows/saved` reaches `/workflows/:id` with id = "saved".
        let res = call.call(testutil::get("/workflows/saved")).await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await["error"],
            "workflow saved not found"
        );

        // A saved workflow named "saved" does not change that — the two
        // namespaces do not touch.
        call.call(testutil::req(
            "PUT",
            "/saved-workflows/saved",
            Some(j!({ "script": "return 1" })),
        ))
        .await;
        let res = call.call(testutil::get("/workflows/saved")).await;
        assert_eq!(res.status(), 404, "a saved workflow is not a run");
        let res = call.call(testutil::get("/saved-workflows/saved")).await;
        assert_eq!(res.status(), 200);
    }

    /// Invoking a saved workflow reads the NAME first: an unknown one is a 404
    /// naming it, and only a name that resolves reaches the engine.
    #[tokio::test]
    async fn invoking_a_saved_workflow_resolves_the_name_before_the_engine() {
        let _home = TempHome::new();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let session_id = seed_session(&fx);

        let res = call
            .call(testutil::req(
                "POST",
                "/saved-workflows/nightly/runs",
                Some(j!({ "sessionId": session_id })),
            ))
            .await;
        assert_eq!(res.status(), 404);
        let msg = testutil::body_json(res).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            msg.starts_with("no saved workflow named \"nightly\""),
            "{msg}"
        );

        call.call(testutil::req(
            "PUT",
            "/saved-workflows/nightly",
            Some(j!({ "script": "return 1" })),
        ))
        .await;
        let res = call
            .call(testutil::req(
                "POST",
                "/saved-workflows/nightly/runs",
                Some(j!({ "sessionId": session_id })),
            ))
            .await;
        assert_eq!(
            res.status(),
            400,
            "the name resolved; the ENGINE is what is missing"
        );
        assert_eq!(
            testutil::body_json(res).await["error"],
            "workflows are not yet ported in this build"
        );
    }

    /// `GET /workflows/:id/replay` answers off real rows, mid-run, and its
    /// buckets sum to the total.
    #[tokio::test]
    async fn the_replay_endpoint_answers_off_real_rows() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let res = call.call(testutil::get("/workflows/nope/replay")).await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await["error"],
            "workflow nope not found"
        );

        seed_run(&fx, "run-3", "return 1");
        let res = call.call(testutil::get("/workflows/run-3/replay")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["runId"], "run-3");
        assert_eq!(body["total"], 0);
        assert_eq!(body["final"], true, "the wire spells it `final`");
        assert_eq!(body["line"], "no agent calls");
        for key in [
            "sourceId",
            "replayed",
            "ranLive",
            "pending",
            "available",
            "forced",
        ] {
            assert!(body.get(key).is_some(), "missing {key}: {body}");
        }
    }

    /// The settings shape parity.sh diffs against the TS server, and the two
    /// verbatim strings a stub got wrong: `unrestricted` has its OWN advice
    /// sentence, and PUT answers a SMALLER body than GET.
    #[tokio::test]
    async fn the_settings_routes_answer_the_guideline_and_its_derived_thresholds() {
        let _home = TempHome::new();
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
        assert_eq!(
            body["advisory"], true,
            "the guideline is advice, never a cap"
        );
        assert_eq!(body["maxAgentsPerRun"], 1000);
        assert!(
            body["advice"]
                .as_str()
                .unwrap()
                .contains("advice, not a cap"),
            "{body}"
        );

        // PUT persists, echoes, and omits the two fields it did not change.
        let res = call
            .call(testutil::req(
                "PUT",
                "/workflow-settings",
                Some(j!({ "sizeGuideline": "unrestricted" })),
            ))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["sizeGuideline"], "unrestricted");
        assert_eq!(
            body["target"],
            serde_json::Value::Null,
            "Infinity is null on the wire"
        );
        assert_eq!(
            body["advice"],
            "Workflow size guideline: unrestricted — fan out as wide as the job needs."
        );
        assert!(
            body.get("concurrency").is_none(),
            "PUT answers a smaller body: {body}"
        );
        assert!(body.get("maxAgentsPerRun").is_none(), "{body}");

        // …and the GET now reads it back.
        let res = call.call(testutil::get("/workflow-settings")).await;
        assert_eq!(
            testutil::body_json(res).await["sizeGuideline"],
            "unrestricted"
        );

        // An unknown value is a 400 listing all four.
        let res = call
            .call(testutil::req(
                "PUT",
                "/workflow-settings",
                Some(j!({ "sizeGuideline": "huge" })),
            ))
            .await;
        assert_eq!(res.status(), 400);
        let msg = testutil::body_json(res).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.contains("small, medium, large, unrestricted"), "{msg}");
        assert!(msg.contains("never a cap"), "{msg}");
    }

    /// The lifecycle verbs, on a run that exists but is NOT live in this
    /// process — the state every run is in after a restart.
    ///
    /// `pause`/`resume` answer 409 rather than a courtesy 200: pausing is an
    /// instruction to a running worker and there is no worker to instruct.
    /// `stop` is idempotent and answers the row. All three 404 an unknown id
    /// FIRST, so a typo never reads as "running somewhere else".
    #[tokio::test]
    async fn pause_is_409_when_the_run_is_not_live_and_stop_is_idempotent() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        seed_run(&fx, "run-live", "return 1");

        for verb in ["pause", "resume"] {
            let res = call
                .call(testutil::req(
                    "POST",
                    &format!("/workflows/run-live/{verb}"),
                    None,
                ))
                .await;
            assert_eq!(res.status(), 409, "{verb} on a run no worker owns");
            let msg = testutil::body_json(res).await["error"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(msg.contains("not running in this process"), "{verb}: {msg}");
            // The 404 comes first: an unknown id is not a 409.
            let res = call
                .call(testutil::req(
                    "POST",
                    &format!("/workflows/nope/{verb}"),
                    None,
                ))
                .await;
            assert_eq!(res.status(), 404, "{verb} on an unknown id");
            assert_eq!(
                testutil::body_json(res).await["error"],
                "workflow nope not found"
            );
        }

        // Stop on a finished run answers the row rather than 409ing — "stop"
        // on something already stopped is the state the caller wanted.
        let res = call
            .call(testutil::req("POST", "/workflows/run-live/stop", None))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await["id"], "run-live");
        let res = call
            .call(testutil::req("POST", "/workflows/run-live/stop", None))
            .await;
        assert_eq!(res.status(), 200, "idempotent");
        let res = call
            .call(testutil::req("POST", "/workflows/nope/stop", None))
            .await;
        assert_eq!(res.status(), 404);
    }

    /// `GET /workflows` and `GET /workflows/:id` answer off real rows, with the
    /// summary's agent counts and the detail's three accounting fields.
    #[tokio::test]
    async fn the_run_list_and_the_run_detail_answer_off_real_rows() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        seed_run(&fx, "run-a", "return 1");

        let res = call.call(testutil::get("/workflows")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        let rows = body["workflows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "run-a");
        assert_eq!(rows[0]["agents"]["total"], 0);
        assert!(
            rows[0].get("script").is_none(),
            "a listing never carries the script"
        );

        // `?session=` scopes, and a session with no runs lists empty.
        let res = call
            .call(testutil::get("/workflows?session=no-such-session"))
            .await;
        assert_eq!(
            testutil::body_json(res).await["workflows"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let res = call.call(testutil::get("/workflows/run-a")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        for key in [
            "workflow",
            "agents",
            "scriptFile",
            "live",
            "replay",
            "cost",
            "warning",
            "guideline",
        ] {
            assert!(body.get(key).is_some(), "missing {key}: {body}");
        }
        assert_eq!(body["live"], false, "no worker owns it in this process");
        assert_eq!(body["replay"]["total"], 0);
        assert_eq!(body["replay"]["replayed"], 0);

        let res = call.call(testutil::get("/workflows/nope")).await;
        assert_eq!(res.status(), 404);
    }

    /// The verbs that still need `control.rs`'s submit boundary answer the
    /// unknown-run 404, and the agent-action typo is caught BEFORE the lookup.
    #[tokio::test]
    async fn unported_verbs_404_and_an_action_typo_is_a_400_first() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        for (method, path) in [
            ("POST", "/workflows/wf_1/rerun"),
            ("POST", "/workflows/wf_1/relaunch"),
        ] {
            let res = call.call(testutil::req(method, path, None)).await;
            assert_eq!(res.status(), 404, "{method} {path}");
            let body = testutil::body_json(res).await;
            assert_eq!(body["error"], "workflow wf_1 not found", "{method} {path}");
        }

        let res = call
            .call(testutil::req(
                "POST",
                "/workflows/wf_1/agents/a1/pause",
                None,
            ))
            .await;
        assert_eq!(res.status(), 400);
        let msg = testutil::body_json(res).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            msg.contains("unknown workflow agent action 'pause'"),
            "{msg}"
        );
        assert!(msg.contains("'stop'") && msg.contains("'restart'"), "{msg}");
        // A valid action reaches the run lookup and 404s.
        let res = call
            .call(testutil::req(
                "POST",
                "/workflows/wf_1/agents/a1/stop",
                None,
            ))
            .await;
        assert_eq!(res.status(), 404);
    }
}
