//! The HTTP surface (port of `src/server/app.ts`): one route table, one
//! dispatcher, one catch.
//!
//! **HTTP lives here and nowhere else.** A domain module signals failure by
//! returning a `BoughError` carrying its status; the single catch below is
//! what turns that into a response. No module outside `bough-server`
//! constructs a `Response`.
//!
//! **The route table is APPEND-ONLY** and matched in table order, first match
//! wins — never reorder, never edit another row's entry (a reorder can
//! silently steal a route; `/saved-workflows` is top-level for exactly that
//! reason). Wave 1 carries the events/sessions/interrupt families (rows
//! 1.26–1.29); row 1.31 appends the rest below the marker.
//!
//! **No CORS headers, ever.** The only client is the native TUI. Their absence
//! is what stops a webpage the user happens to visit from reaching this
//! loopback API and driving the agent — the server binds loopback with no auth
//! layer, so this is the whole of its access control.

use std::sync::Arc;

use axum::extract::Request;
use axum::response::Response;
use axum::Router;
use futures::FutureExt;

use bough_core::types::AppCtx;

use crate::http::{error_response, route, Params, Route};
use crate::{
    artifact_lib, artifacts, attachments, changes, comments, config, events, fs, ghost,
    history_ops, jobs, mcp_oauth, mcp_routes, models, questions, schedules, search, sessions,
    skills, theme, turns, workflows,
};

// ---- the route table --------------------------------------------------------

/// Build the shared table. APPEND-ONLY: add new entries at the end, below the
/// marker, one line per route. The table starts after the marker on purpose:
/// `GET /` and the 404 are fallbacks in the dispatcher, not entries.
pub fn routes() -> Vec<Route> {
    vec![
        // ── append new routes below this line, never above it ──
        route("GET", "/events", events::events()),
        // sessions and messages
        route("GET", "/sessions", sessions::list_sessions()),
        route("POST", "/sessions", sessions::create_session()),
        route("GET", "/sessions/:id", sessions::get_session()),
        route("PATCH", "/sessions/:id", sessions::patch_session()),
        route("POST", "/sessions/:id/messages", sessions::post_message()),
        route("PUT", "/sessions/:id/draft", sessions::put_draft()),
        // model settings (what a NEW conversation runs on)
        route("GET", "/model-settings", sessions::get_model_settings_h()),
        route("PUT", "/model-settings", sessions::put_model_settings_h()),
        // the user interrupt
        route(
            "POST",
            "/sessions/:id/interrupt",
            turns::interrupt_session(),
        ),
        // the live cost meter
        route(
            "GET",
            "/sessions/:id/usage",
            sessions::get_session_usage_h(),
        ),
        // what the last turn actually put in the window
        route(
            "GET",
            "/sessions/:id/prompt",
            sessions::get_session_prompt(),
        ),
        // ── row 1.31: the remaining route families (v1 stubs are marked in
        //    their own modules; the table keeps the TS append order) ──
        // workflows (rows 3.9–3.12), engine-backed and live
        route("GET", "/workflows", workflows::list_workflows()),
        route("POST", "/workflows", workflows::create_workflow()),
        route("GET", "/workflows/:id", workflows::get_workflow()),
        route(
            "POST",
            "/workflows/:id/stop",
            workflows::stop_workflow_route(),
        ),
        route(
            "POST",
            "/workflows/:id/pause",
            workflows::pause_workflow_route(),
        ),
        route(
            "POST",
            "/workflows/:id/resume",
            workflows::resume_workflow_route(),
        ),
        route(
            "POST",
            "/workflows/:id/rerun",
            workflows::rerun_workflow_route(),
        ),
        route(
            "POST",
            "/workflows/:id/agents/:agentId/:action",
            workflows::control_workflow_agent(),
        ),
        // schedules
        route("GET", "/schedules", schedules::list_schedules()),
        route("POST", "/schedules", schedules::create_schedule()),
        route("PATCH", "/schedules/:id", schedules::patch_schedule()),
        route("DELETE", "/schedules/:id", schedules::delete_schedule()),
        // ask() holds
        route("GET", "/questions", questions::list_questions()),
        route(
            "POST",
            "/sessions/:id/questions/:qid",
            questions::answer_question(),
        ),
        // artifacts (listing is filesystem-backed, deliberately no session check)
        route(
            "GET",
            "/sessions/:id/artifacts",
            artifacts::list_artifacts(),
        ),
        route(
            "GET",
            "/sessions/:id/artifacts/versions",
            artifacts::list_artifact_versions(),
        ),
        route(
            "POST",
            "/sessions/:id/artifacts/restore",
            artifacts::restore_artifact_version(),
        ),
        // config: every hook, skill and extension the harness injects, grouped
        // by where it came from, with the switch on each of them
        route("GET", "/config", config::list_route()),
        route("POST", "/config/:id", config::toggle()),
        // The vendored chart engines, ahead of the catch-all it would otherwise
        // fall into. Session ids are uuids, so `_lib` shadows no real session.
        route("GET", "/artifacts/_lib/:file", artifact_lib::get_lib_file()),
        route("GET", "/artifacts/:id/:path*", artifacts::get_artifact()),
        // jobs — a session's list covers its subagents' work too
        route("GET", "/sessions/:id/jobs", jobs::list_jobs()),
        route("POST", "/sessions/:id/jobs", jobs::run_shell()),
        route("POST", "/sessions/:id/jobs/:jobId/kill", jobs::kill_job()),
        route(
            "GET",
            "/sessions/:id/jobs/:jobId/output",
            jobs::job_output(),
        ),
        // workflow relaunch/replay + saving (top-level /saved-workflows so the
        // append-order table cannot let /workflows/:id swallow it)
        route(
            "POST",
            "/workflows/:id/relaunch",
            workflows::relaunch_workflow_route(),
        ),
        route("GET", "/workflows/:id/replay", workflows::workflow_replay()),
        route(
            "POST",
            "/workflows/:id/save",
            workflows::save_workflow_route(),
        ),
        route(
            "GET",
            "/saved-workflows",
            workflows::list_saved_workflows_route(),
        ),
        route(
            "GET",
            "/saved-workflows/:name",
            workflows::get_saved_workflow(),
        ),
        route(
            "PUT",
            "/saved-workflows/:name",
            workflows::put_saved_workflow(),
        ),
        route(
            "POST",
            "/saved-workflows/:name/runs",
            workflows::run_saved_workflow(),
        ),
        // the picker's catalog (static table + discovered rows, 2.5s deadline)
        route("GET", "/models", models::get_models()),
        // the composer's `@` completion
        route("GET", "/sessions/:id/files", fs::list_files()),
        route("GET", "/files", fs::list_files_for_workspace()),
        route("GET", "/fs/entries", fs::list_dir_entries_h()),
        route("GET", "/fs/branch", fs::branch()),
        route(
            "GET",
            "/workflow-settings",
            workflows::get_workflow_settings(),
        ),
        route(
            "PUT",
            "/workflow-settings",
            workflows::put_workflow_settings(),
        ),
        // MCP: the registry, the grants and the connections (rows 3.1-3.3);
        // the OAuth verbs are row 3.5's, in `mcp_oauth.rs`
        route("GET", mcp_oauth::CALLBACK_PATH, mcp_oauth::oauth_callback()),
        route("GET", "/mcp/servers/:name/auth", mcp_oauth::auth_status()),
        route("POST", "/mcp/servers/:name/auth", mcp_oauth::begin_auth()),
        route("DELETE", "/mcp/servers/:name/auth", mcp_oauth::clear_auth()),
        route("GET", "/mcp/servers", mcp_routes::get_mcp_servers()),
        route("PUT", "/mcp/servers", mcp_routes::put_mcp_servers()),
        route("PUT", "/mcp/servers/:name", mcp_routes::put_mcp_server()),
        route(
            "DELETE",
            "/mcp/servers/:name",
            mcp_routes::delete_mcp_server(),
        ),
        route(
            "POST",
            "/mcp/servers/:name/connect",
            mcp_routes::connect_mcp_server(),
        ),
        route(
            "POST",
            "/mcp/servers/:name/tools/:tool",
            mcp_routes::call_mcp_tool(),
        ),
        route(
            "POST",
            "/mcp/servers/:name/restart",
            mcp_routes::restart_mcp_server(),
        ),
        route(
            "POST",
            "/mcp/servers/:name/enable",
            mcp_routes::set_mcp_activation(true),
        ),
        route(
            "POST",
            "/mcp/servers/:name/disable",
            mcp_routes::set_mcp_activation(false),
        ),
        // history operations (wave-2/3 subsystem; session typos still 404 first)
        route("POST", "/sessions/:id/fork", history_ops::fork_session()),
        route(
            "POST",
            "/sessions/:id/compact",
            history_ops::compact_session(),
        ),
        route("POST", "/sessions/:id/sections", history_ops::sections()),
        route("POST", "/sessions/:id/extract", history_ops::extract()),
        route("POST", "/sessions/:id/move-into", history_ops::move_into()),
        route("POST", "/sessions/:id/handoff", history_ops::handoff()),
        // the Changes rail
        route("GET", "/sessions/:id/changes", changes::get_changes()),
        route(
            "POST",
            "/sessions/:id/changes/revert",
            changes::revert_changes_h(),
        ),
        // transcript search
        route("GET", "/search", search::search()),
        route("POST", "/search/reindex", search::reindex()),
        // theming
        route("GET", "/theme", theme::get_theme()),
        route("PUT", "/theme", theme::put_theme()),
        route("DELETE", "/theme", theme::delete_theme()),
        // composer ghost text
        route("POST", "/sessions/:id/ghost", ghost::ghost_text()),
        // skills
        route("GET", "/skills", skills::list_skills_h()),
        route("GET", "/skills/:name", skills::get_skill()),
        // clipboard images
        route("POST", "/attachments", attachments::upload_attachment()),
        // the take-back
        route(
            "POST",
            "/sessions/:id/unsend",
            history_ops::unsend_message(),
        ),
        // artifact comments (row 3.14). These are what the layer injected into
        // every served HTML artifact talks to. `/comments/send` is listed
        // before `/comments/:cid` for reading order; the two cannot collide
        // anyway (different methods).
        route("GET", "/sessions/:id/comments", comments::list_comments()),
        route("POST", "/sessions/:id/comments", comments::post_comment()),
        route(
            "POST",
            "/sessions/:id/comments/send",
            comments::send_comments(),
        ),
        route(
            "DELETE",
            "/sessions/:id/comments/:cid",
            comments::delete_comment_route(),
        ),
        // the session log — what `milestone()` wrote, oldest first
        route("GET", "/sessions/:id/log", sessions::get_session_log()),
    ]
}

// ---- dispatch ---------------------------------------------------------------

/// The pointer served at `GET /`. There is no web UI; this origin is the API.
pub const ROOT_POINTER: &str = "bough server — drive it with the `bough` TUI.\n\
There is no web UI: this origin is the JSON API, the /events SSE stream, and \
artifact hosting.\n";

/// Where a panic escaping a handler is reported. Such a panic is a bug, not a
/// domain outcome, so it is logged rather than swallowed; a test passes a
/// collector so the isolation can be asserted instead of inferred.
pub type UnexpectedErrorHook = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
pub struct CreateHandlerOptions {
    /// Override the route table. Production takes the default; a router test
    /// passes a fabricated table so it exercises dispatch, params and error
    /// mapping without depending on which endpoints happen to exist yet.
    pub routes: Option<Vec<Route>>,
    pub on_unexpected_error: Option<UnexpectedErrorHook>,
}

/// The dispatcher bound to a ctx. `boot` wraps it in an axum `Router`; tests
/// call [`Dispatcher::call`] directly with a `Request` and never bind a socket.
#[derive(Clone)]
pub struct Dispatcher {
    ctx: AppCtx,
    table: Arc<Vec<Route>>,
    on_unexpected_error: UnexpectedErrorHook,
}

/// Build the dispatcher bound to a ctx.
pub fn create_handler(ctx: AppCtx, opts: CreateHandlerOptions) -> Dispatcher {
    let table = Arc::new(opts.routes.unwrap_or_else(routes));
    let on_unexpected_error = opts.on_unexpected_error.unwrap_or_else(|| {
        Arc::new(|msg: &str| tracing::error!("unhandled error in handler: {msg}"))
    });
    Dispatcher {
        ctx,
        table,
        on_unexpected_error,
    }
}

impl Dispatcher {
    pub async fn call(&self, req: Request) -> Response {
        let pathname = req.uri().path().to_string();
        let method = req.method().as_str().to_string();

        for entry in self.table.iter() {
            if entry.method != method {
                continue;
            }
            let Some(params) = entry.pattern.matches(&pathname) else {
                continue;
            };
            return self.run(entry, req, params).await;
        }

        if method == "GET" && pathname == "/" {
            return Response::builder()
                .status(200)
                .header("content-type", "text/plain; charset=utf-8")
                .body(axum::body::Body::from(ROOT_POINTER))
                .expect("static response parts");
        }

        // The path exists but not for this method. Saying so beats a 404 that
        // reads as "endpoint missing" and sends the caller after the wrong bug.
        let mut allowed: Vec<&str> = Vec::new();
        for r in self.table.iter() {
            if r.pattern.matches(&pathname).is_some() && !allowed.contains(&r.method) {
                allowed.push(r.method);
            }
        }
        if !allowed.is_empty() {
            let list = allowed.join(", ");
            let body = serde_json::json!({
                "error": format!("{method} not allowed on {pathname} — try {list}"),
            });
            return Response::builder()
                .status(405)
                .header("content-type", "application/json; charset=utf-8")
                .header("allow", list)
                .body(axum::body::Body::from(body.to_string()))
                .expect("static response parts");
        }

        error_response(404, &format!("no route for {method} {pathname}"))
    }

    /// THE one catch. A `BoughError` carries its own status; a panic is a
    /// defect: report it and answer 500 rather than dropping the connection.
    async fn run(&self, entry: &Route, req: Request, params: Params) -> Response {
        let fut = (entry.handler)(req, self.ctx.clone(), params);
        match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
            Ok(Ok(res)) => res,
            Ok(Err(e)) => error_response(e.status(), &e.to_string()),
            Err(panic) => {
                let msg = panic_message(panic);
                (self.on_unexpected_error)(&msg);
                error_response(500, &msg)
            }
        }
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unexpected error".to_string()
    }
}

/// Wrap the dispatcher in an axum `Router` (every request falls through to the
/// table walk, which owns 404/405/root-pointer semantics).
pub fn build_router(ctx: AppCtx) -> Router {
    let dispatcher = create_handler(ctx, CreateHandlerOptions::default());
    Router::new().fallback(move |req: Request| {
        let d = dispatcher.clone();
        async move { d.call(req).await }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::http::{handler, json, parse_body, testutil, Handler};
    use bough_core::errors::{BoughError, ErrorKind};
    use serde_json::json as j;

    fn ok() -> Handler {
        handler(|_r, _c, _p| async { Ok(json(&j!({"ok": true}), 200)) })
    }

    struct H {
        call: Dispatcher,
        reported: Arc<Mutex<Vec<String>>>,
        _fx: testutil::Fixture,
    }

    fn with_handler(table: Vec<Route>) -> H {
        let fx = testutil::fixture();
        let reported = Arc::new(Mutex::new(Vec::new()));
        let sink = reported.clone();
        let call = create_handler(
            fx.ctx.clone(),
            CreateHandlerOptions {
                routes: Some(table),
                on_unexpected_error: Some(Arc::new(move |msg: &str| {
                    sink.lock().unwrap().push(msg.to_string())
                })),
            },
        );
        H {
            call,
            reported,
            _fx: fx,
        }
    }

    #[tokio::test]
    async fn dispatches_a_matching_method_and_pathname_to_its_handler() {
        let h = with_handler(vec![route("GET", "/sessions", ok())]);
        let res = h.call.call(testutil::get("/sessions")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "application/json; charset=utf-8"
        );
        assert_eq!(testutil::body_json(res).await, j!({"ok": true}));
    }

    #[tokio::test]
    async fn extracts_named_groups_as_params() {
        let seen: Arc<Mutex<Option<Params>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let capture = handler(move |_r, _c, p| {
            let s = s.clone();
            async move {
                *s.lock().unwrap() = Some(p);
                Ok(json(&j!({}), 200))
            }
        });
        let h = with_handler(vec![route("GET", "/sessions/:id/jobs/:jobId", capture)]);
        h.call.call(testutil::get("/sessions/abc/jobs/bg_1")).await;
        let got = seen.lock().unwrap().clone().unwrap();
        assert_eq!(got.get("id").unwrap(), "abc");
        assert_eq!(got.get("jobId").unwrap(), "bg_1");
    }

    #[tokio::test]
    async fn omits_an_optional_group_that_did_not_match() {
        let seen: Arc<Mutex<Option<Params>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let capture = handler(move |_r, _c, p| {
            let s = s.clone();
            async move {
                *s.lock().unwrap() = Some(p);
                Ok(json(&j!({}), 200))
            }
        });
        let h = with_handler(vec![route("GET", "/artifacts/:id/:path*", capture)]);
        h.call.call(testutil::get("/artifacts/s1")).await;
        let got = seen.lock().unwrap().clone().unwrap();
        assert!(!got.contains_key("path"));
        assert_eq!(got.get("id").unwrap(), "s1");
        h.call
            .call(testutil::get("/artifacts/s1/deep/page.html"))
            .await;
        let got = seen.lock().unwrap().clone().unwrap();
        assert_eq!(got.get("path").unwrap(), "deep/page.html");
    }

    #[tokio::test]
    async fn matches_on_pathname_only_query_string_does_not_affect_routing() {
        let h = with_handler(vec![route("GET", "/events", ok())]);
        let res = h.call.call(testutil::get("/events?sessionId=abc")).await;
        assert_eq!(res.status(), 200);
    }

    #[tokio::test]
    async fn first_match_wins_so_appending_never_steals_an_existing_route() {
        let first = handler(|_r, _c, _p| async { Ok(json(&j!({"which": "first"}), 200)) });
        let second = handler(|_r, _c, _p| async { Ok(json(&j!({"which": "second"}), 200)) });
        let h = with_handler(vec![
            route("GET", "/sessions/new", first),
            route("GET", "/sessions/:id", second),
        ]);
        let res = h.call.call(testutil::get("/sessions/new")).await;
        assert_eq!(testutil::body_json(res).await, j!({"which": "first"}));
        let res = h.call.call(testutil::get("/sessions/x1")).await;
        assert_eq!(testutil::body_json(res).await, j!({"which": "second"}));
    }

    #[tokio::test]
    async fn maps_a_returned_error_to_its_status_and_message() {
        let missing = handler(|_r, _c, _p| async {
            Err::<Response, _>(BoughError::not_found("session not found"))
        });
        let h = with_handler(vec![route("GET", "/sessions/:id", missing)]);
        let res = h.call.call(testutil::get("/sessions/nope")).await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"error": "session not found"})
        );
        // A domain error is an outcome, not a defect: nothing is reported.
        assert!(h.reported.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn maps_each_error_status_including_ones_no_generic_catch_could_guess() {
        let h = with_handler(vec![
            route(
                "POST",
                "/conflict",
                handler(|_r, _c, _p| async {
                    Err::<Response, _>(BoughError::conflict("that subagent already finished"))
                }),
            ),
            route(
                "POST",
                "/teapot",
                handler(|_r, _c, _p| async {
                    Err::<Response, _>(BoughError::http(
                        413,
                        ErrorKind::Turn,
                        "context window exceeded: 200000 tokens",
                    ))
                }),
            ),
        ]);
        let res = h.call.call(testutil::req("POST", "/conflict", None)).await;
        assert_eq!(res.status(), 409);
        let overflow = h.call.call(testutil::req("POST", "/teapot", None)).await;
        assert_eq!(overflow.status(), 413);
        assert_eq!(
            testutil::body_json(overflow).await,
            j!({"error": "context window exceeded: 200000 tokens"})
        );
    }

    #[tokio::test]
    async fn turns_an_unexpected_panic_into_a_reported_500_never_a_dropped_request() {
        let boom = handler(|_r, _c, _p| async {
            panic!("cannot read properties of undefined");
            #[allow(unreachable_code)]
            Ok(json(&j!({}), 200))
        });
        let h = with_handler(vec![route("GET", "/x", boom)]);
        let res = h.call.call(testutil::get("/x")).await;
        assert_eq!(res.status(), 500);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"error": "cannot read properties of undefined"})
        );
        // Reported exactly once: it is a defect and must be visible in the log.
        assert_eq!(
            *h.reported.lock().unwrap(),
            vec!["cannot read properties of undefined".to_string()]
        );
    }

    #[tokio::test]
    async fn one_failing_request_does_not_poison_the_next() {
        let h = with_handler(vec![
            route(
                "GET",
                "/bad",
                handler(|_r, _c, _p| async {
                    panic!("boom");
                    #[allow(unreachable_code)]
                    Ok(json(&j!({}), 200))
                }),
            ),
            route("GET", "/good", ok()),
        ]);
        assert_eq!(h.call.call(testutil::get("/bad")).await.status(), 500);
        assert_eq!(h.call.call(testutil::get("/good")).await.status(), 200);
    }

    #[tokio::test]
    async fn get_root_returns_a_plain_text_pointer() {
        let h = with_handler(vec![]);
        let res = h.call.call(testutil::get("/")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
        let body = testutil::body_text(res).await;
        assert!(body.contains("bough server"), "{body}");
        assert!(body.contains("no web UI"), "{body}");
    }

    #[tokio::test]
    async fn an_unknown_path_is_a_404_naming_the_method_and_path() {
        let h = with_handler(vec![route("GET", "/sessions", ok())]);
        let res = h.call.call(testutil::get("/nope")).await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"error": "no route for GET /nope"})
        );
    }

    #[tokio::test]
    async fn a_known_path_with_the_wrong_method_is_a_405_naming_the_allowed_ones() {
        let h = with_handler(vec![
            route("GET", "/sessions/:id", ok()),
            route("POST", "/sessions/:id", ok()),
        ]);
        let res = h
            .call
            .call(testutil::req("DELETE", "/sessions/a", None))
            .await;
        assert_eq!(res.status(), 405);
        assert_eq!(res.headers().get("allow").unwrap(), "GET, POST");
        let body = testutil::body_json(res).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("DELETE not allowed on /sessions/a"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn the_root_pointer_wins_over_a_405_when_some_other_method_owns_root() {
        let h = with_handler(vec![route("POST", "/", ok())]);
        let res = h.call.call(testutil::get("/")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn an_invalid_body_becomes_a_400_through_the_routers_one_catch() {
        #[derive(serde::Deserialize)]
        struct B {
            #[allow(dead_code)]
            text: String,
        }
        let h = with_handler(vec![route(
            "POST",
            "/m",
            handler(|req, _c, _p| async {
                let _: B = parse_body(req, None).await?;
                Ok(json(&j!({}), 200))
            }),
        )]);
        let res = h
            .call
            .call(testutil::req("POST", "/m", Some(j!({"text": 42}))))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("invalid body: "),
            "{body}"
        );
        // A 400 is a domain outcome, not a defect — it must not be logged as one.
        assert!(h.reported.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn malformed_json_is_a_400_not_a_500() {
        #[derive(serde::Deserialize)]
        struct B {
            #[allow(dead_code)]
            text: String,
        }
        let h = with_handler(vec![route(
            "POST",
            "/m",
            handler(|req, _c, _p| async {
                let _: B = parse_body(req, None).await?;
                Ok(json(&j!({}), 200))
            }),
        )]);
        let req = axum::extract::Request::builder()
            .method("POST")
            .uri("/m")
            .body(axum::body::Body::from("{not json"))
            .unwrap();
        let res = h.call.call(req).await;
        assert_eq!(res.status(), 400);
        assert!(h.reported.lock().unwrap().is_empty());
    }

    #[test]
    fn the_shared_route_table_has_no_duplicate_method_pathname_entry() {
        let mut seen = std::collections::HashSet::new();
        for entry in routes() {
            let key = format!("{} {}", entry.method, entry.pattern.pathname);
            assert!(seen.insert(key.clone()), "duplicate route appended: {key}");
        }
    }

    #[tokio::test]
    async fn create_handler_defaults_to_the_shared_route_table() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        assert_eq!(call.call(testutil::get("/")).await.status(), 200);
        assert_eq!(
            call.call(testutil::get("/__no_such_route__"))
                .await
                .status(),
            404
        );
    }

    /// `/artifacts/_lib/*` must be matched by the bundle route, not swallowed
    /// by `/artifacts/:id/:path*` — the catch-all matches that path too, and
    /// first-match-wins means the ONLY thing keeping the engines reachable is
    /// their entry sitting earlier in the table. Reorder it and every chart in
    /// every artifact silently 404s, with nothing else failing.
    #[tokio::test]
    async fn the_bundle_route_wins_over_the_artifact_catch_all() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/artifacts/_lib/flint.js")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/javascript; charset=utf-8"
        );
    }

    /// The gate's required shape: the wrapped axum Router answers via
    /// `tower::ServiceExt::oneshot`, no socket bound.
    #[tokio::test]
    async fn build_router_dispatches_via_oneshot() {
        use tower::ServiceExt;
        let fx = testutil::fixture();
        let router = build_router(fx.ctx.clone());
        let res = router.clone().oneshot(testutil::get("/")).await.unwrap();
        assert_eq!(res.status(), 200);
        let res = router
            .clone()
            .oneshot(testutil::req("DELETE", "/sessions", None))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            405,
            "the table's 405 semantics survive the Router wrap"
        );
        assert_eq!(res.headers().get("allow").unwrap(), "GET, POST");
    }
}
