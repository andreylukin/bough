//! The jobs API (port of `src/server/jobs.ts`): list, start, kill, read
//! output — for a session AND its subagents.
//!
//! THE INVARIANT: **a session's job list covers the work done on its behalf,
//! not just the work its own turn started.** The walk is TRANSITIVE over the
//! kinds that collapse under this session (subagents, their subagents, its
//! schedules' firings) and each row keeps its own `sessionId` — a fork is
//! excluded because it is a sibling conversation the user drives.
//!
//! KILL IS BY ID, ACROSS SESSIONS: anything this endpoint can LIST it must
//! also be able to kill — scoping the kill to the session in the URL would
//! 404 on every subagent row the list had just returned.
//!
//! READING OUTPUT HERE IS NON-DESTRUCTIVE: the whole retained buffer, without
//! advancing the model's `bashOutput` cursor.

use std::collections::HashSet;

use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::hostfn::jobs::JobCtx;
use bough_core::schema::parts::is_collapsed_kind;
use bough_core::schema::requests::RunShellBody;
use bough_core::types::AppCtx;

use crate::http::{handler, json as json_res, parse_body, Handler};

/// The transcript job card's tail depth (the TS `jobTail` default).
const TAIL_LINES: usize = 5;

fn require_session(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    match ctx.db.lock().unwrap().get_session(id)? {
        Some(_) => Ok(()),
        None => Err(BoughError::not_found(format!(
            "no session {id} — jobs are listed per session, so open one that exists \
             (GET /sessions lists them).",
        ))),
    }
}

/// The session and every branch collapsed under it, transitively, ids only.
/// The `seen` set is what stops a lineage cycle from hanging the request.
fn job_session_ids(ctx: &AppCtx, session_id: &str) -> Result<Vec<String>, BoughError> {
    let db = ctx.db.lock().unwrap();
    let mut out: Vec<String> = vec![session_id.to_string()];
    let mut seen: HashSet<String> = HashSet::from([session_id.to_string()]);
    let mut i = 0;
    while i < out.len() {
        for child in db.sessions_by_origin(&out[i])? {
            if !is_collapsed_kind(child.kind) {
                continue;
            }
            if !seen.insert(child.id.clone()) {
                continue;
            }
            out.push(child.id);
        }
        i += 1;
    }
    Ok(out)
}

/// `GET /sessions/:id/jobs` — live and recently-exited background shells,
/// each row with its tail merged in (non-destructively).
pub fn list_jobs() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        let mut jobs: Vec<serde_json::Value> = Vec::new();
        for sid in job_session_ids(&ctx, &id)? {
            for job in ctx.host.jobs.list_jobs(&sid) {
                let mut row = serde_json::to_value(&job)
                    .map_err(|e| BoughError::bad_request(format!("unencodable job row: {e}")))?;
                if let Some(tail) = ctx.host.jobs.job_tail(&job.id, TAIL_LINES) {
                    row["tail"] = json!(tail.tail);
                    row["outputLines"] = json!(tail.output_lines);
                }
                jobs.push(row);
            }
        }
        Ok(json_res(&json!({ "jobs": jobs }), 200))
    })
}

/// `POST /sessions/:id/jobs` — the USER starts a shell in the session's
/// workspace. A `bashBg` shell with `wake: false`: no turn, no wake, no
/// thread entry — a command the user started is not the agent's to be
/// billed for, and its exit must not claim the output out of the card.
pub fn run_shell() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        let body: RunShellBody = parse_body(req, None).await?;
        let len = body.command.chars().count();
        if len == 0 || len > 4000 {
            return Err(BoughError::bad_request(
                "invalid body: command must be 1..4000 characters",
            ));
        }
        let workspace = ctx
            .db
            .lock()
            .unwrap()
            .get_session_runtime(&id)?
            .workspace
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|d| d.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| ".".to_string())
            });
        // The name is what the rail shows. The command IS the name here: the
        // user typed it and knows exactly what they meant.
        let name: String = body.command.trim().chars().take(60).collect();
        let started = ctx.host.jobs.bash_bg(
            &name,
            &body.command,
            &JobCtx { session_id: id, workspace },
            false,
        )?;
        let parsed: serde_json::Value = serde_json::from_str(&started)
            .map_err(|e| BoughError::bad_request(format!("unencodable spawn receipt: {e}")))?;
        Ok(json_res(&parsed, 201))
    })
}

/// `POST /sessions/:id/jobs/:jobId/kill` — SIGTERM with a SIGKILL backstop;
/// waits for the process to actually die, so the response reports the
/// outcome rather than the intent. `job.exited` follows via the bus.
pub fn kill_job() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        let job_id = params.get("jobId").cloned().unwrap_or_default();
        let message = ctx.host.jobs.kill_job(&job_id).await?;
        Ok(json_res(&json!({ "message": message }), 200))
    })
}

/// `GET /sessions/:id/jobs/:jobId/output` — the whole retained buffer (head +
/// tail with omission marker, as the model sees it). Never advances the
/// model's cursor.
pub fn job_output() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        let job_id = params.get("jobId").cloned().unwrap_or_default();
        let Some((output, job)) = ctx.host.jobs.job_output(&job_id) else {
            return Err(BoughError::not_found(format!(
                "no background shell {job_id} — it may have aged out of the job list, or \
                 belong to a session this server has not seen since it restarted (shells \
                 are in-memory and die with the process).",
            )));
        };
        Ok(json_res(&json!({ "output": output, "job": job }), 200))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions, Dispatcher};
    use crate::http::testutil::{self, Fixture};
    use bough_core::schema::parts::{Session, SessionKind};
    use serde_json::json as j;

    fn call(fx: &Fixture) -> Dispatcher {
        create_handler(fx.ctx.clone(), CreateHandlerOptions::default())
    }

    fn seed(fx: &Fixture, kind: SessionKind, origin: Option<&str>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: id.clone(),
                title: "t".into(),
                kind,
                created_at: (fx.ctx.now)(),
                parent_id: origin.map(str::to_string),
                origin_id: origin.map(str::to_string),
                origin_message_id: None,
                workspace: Some(std::env::temp_dir().to_string_lossy().into_owned()),
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
    async fn an_unknown_session_is_the_ts_404_and_a_fresh_one_lists_no_jobs() {
        let fx = testutil::fixture();
        let res = call(&fx).call(testutil::get("/sessions/nope/jobs")).await;
        assert_eq!(res.status(), 404);
        let body = testutil::body_json(res).await;
        assert!(body["error"].as_str().unwrap().contains("jobs are listed per session"));

        let id = seed(&fx, SessionKind::Root, None);
        let res = call(&fx).call(testutil::get(&format!("/sessions/{id}/jobs"))).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"jobs": []}));
    }

    #[tokio::test]
    async fn a_user_shell_starts_as_201_appears_in_the_list_and_its_output_reads_back() {
        let fx = testutil::fixture();
        let id = seed(&fx, SessionKind::Root, None);
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{id}/jobs"),
                Some(j!({"command": "echo user-shell-output"})),
            ))
            .await;
        assert_eq!(res.status(), 201);
        let started = testutil::body_json(res).await;
        let job_id = started["id"].as_str().unwrap().to_string();
        assert_eq!(started["name"], "echo user-shell-output");
        assert!(started["pid"].as_i64().unwrap() > 0);

        // The listing carries the row (with its own sessionId) and, after the
        // command finishes, the output read returns the true buffer.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let res = call(&fx)
                .call(testutil::get(&format!("/sessions/{id}/jobs/{job_id}/output")))
                .await;
            assert_eq!(res.status(), 200);
            let body = testutil::body_json(res).await;
            assert_eq!(body["job"]["sessionId"], id.as_str());
            if body["output"].as_str().unwrap().contains("user-shell-output") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "output never arrived: {body}");
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let res = call(&fx).call(testutil::get(&format!("/sessions/{id}/jobs"))).await;
        let body = testutil::body_json(res).await;
        let jobs = body["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["id"], job_id.as_str());
        assert!(jobs[0]["tail"].is_array(), "tail merged into the card row: {body}");
    }

    #[tokio::test]
    async fn an_empty_or_oversize_command_is_a_400() {
        let fx = testutil::fixture();
        let id = seed(&fx, SessionKind::Root, None);
        for cmd in [String::new(), "x".repeat(4001)] {
            let res = call(&fx)
                .call(testutil::req(
                    "POST",
                    &format!("/sessions/{id}/jobs"),
                    Some(j!({"command": cmd})),
                ))
                .await;
            assert_eq!(res.status(), 400);
        }
    }

    #[tokio::test]
    async fn the_walk_is_transitive_over_collapsed_kinds_but_excludes_forks() {
        let fx = testutil::fixture();
        let root = seed(&fx, SessionKind::Root, None);
        let sub = seed(&fx, SessionKind::Subagent, Some(&root));
        let subsub = seed(&fx, SessionKind::Subagent, Some(&sub));
        let fork = seed(&fx, SessionKind::Fork, Some(&root));

        // One live shell per branch, registered under its own session id.
        for sid in [&sub, &subsub, &fork] {
            fx.ctx
                .host
                .jobs
                .bash_bg(
                    "sleeper",
                    "sleep 5",
                    &bough_core::hostfn::jobs::JobCtx {
                        session_id: sid.to_string(),
                        workspace: std::env::temp_dir().to_string_lossy().into_owned(),
                    },
                    false,
                )
                .unwrap();
        }

        let res = call(&fx).call(testutil::get(&format!("/sessions/{root}/jobs"))).await;
        let body = testutil::body_json(res).await;
        let owners: Vec<&str> = body["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|jb| jb["sessionId"].as_str().unwrap())
            .collect();
        assert!(owners.contains(&sub.as_str()), "subagent's job listed: {owners:?}");
        assert!(owners.contains(&subsub.as_str()), "transitive: sub-subagent listed");
        assert!(!owners.contains(&fork.as_str()), "a fork is a sibling, not delegated work");

        // Kill resolves a subagent's job via the spawner's URL.
        let sub_job = body["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|jb| jb["sessionId"] == sub.as_str())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{root}/jobs/{sub_job}/kill"),
                None,
            ))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert!(body["message"].is_string());
        // Cleanup: stop the remaining sleepers.
        fx.ctx.host.jobs.kill_all();
    }

    #[tokio::test]
    async fn an_unknown_job_output_read_is_the_ts_404() {
        let fx = testutil::fixture();
        let id = seed(&fx, SessionKind::Root, None);
        let res = call(&fx)
            .call(testutil::get(&format!("/sessions/{id}/jobs/bg_404/output")))
            .await;
        assert_eq!(res.status(), 404);
        let body = testutil::body_json(res).await;
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("no background shell bg_404"), "{msg}");
        assert!(msg.contains("in-memory"), "{msg}");
    }
}
