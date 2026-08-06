//! Session CRUD, thread assembly, and the message intake (port of
//! `src/server/sessions.ts`).
//!
//! The invariant this module holds is **derived visibility**: a session of a
//! COLLAPSING kind (`subagent`, `workflow_agent`, `schedule_run`) sits under
//! its `originId` and surfaces only on drill-in — because of what it *is*, not
//! because anything marked it. There is no archive, deprecate, hide or purge
//! verb here and no column behind one: `GET /sessions` filters on `kind`, and
//! `GET /sessions?originId=` is the drill-in that reveals what collapsed.
//!
//! Second invariant: **the thread is assembled, never stored.**
//! `GET /sessions/:id` returns `{session, thread}` where the thread is
//! ancestors root→parent plus the session's own messages (`db.thread_for`).
//!
//! Third: the turn starter arrives on the ctx rather than as an import — this
//! module persists and announces a user message and **does not know how a
//! turn runs**. A message that lands while a turn is already running is
//! persisted and left for the queue drain rather than racing the live turn or
//! being dropped.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use bough_core::errors::BoughError;
use bough_core::paths::bough_home;
use bough_core::prompt::project::{find_project_rules, rule_summaries};
use bough_core::schema::events::{EventInput, EventType};
use bough_core::schema::parts::{
    is_collapsed_kind, Message, Part, Role, Session, SessionKind, TurnStatus,
};
use bough_core::schema::requests::{
    CreateSessionBody, PatchSessionBody, PostMessageBody, PutModelSettingsBody, SetDraftBody,
};
use bough_core::turn::runner::DEFAULT_MODEL;
use bough_core::types::{AppCtx, Effort, Patch};
use bough_core::worker::cheap_model;

use crate::defaults::{default_path, load_defaults, save_defaults, ModelDefaults};
use crate::http::{handler, json, parse_body, Handler, Params};

// ---- derived visibility ------------------------------------------------------

/// True when the session surfaces only on drill-in under its `originId`.
pub fn is_collapsed(session: &Session) -> bool {
    is_collapsed_kind(session.kind)
}

/// A listed session plus the facts the sidebar needs at a glance, all DERIVED
/// at read time from turns and usage rows. None of them is a column on
/// `sessions`: `busy` would be stale the moment a server died mid-turn, and a
/// stored cost would be a second source of truth.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionListItem {
    #[serde(flatten)]
    pub session: Session,
    /// A turn is in flight. The UI keeps it live from events after this read.
    pub busy: bool,
    /// How the most recent turn ended — absent if it has never run one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn_status: Option<TurnStatus>,
    /// This session's own spend, omitted when zero so untouched rows stay small.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// input + output + reasoning — the three billed as fresh tokens. Cache
    /// traffic deliberately excluded: it is already priced into `costUsd`, and
    /// folding it in would make the rail's number jump by tens of thousands on
    /// a cache hit that cost almost nothing. Omitted when zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
}

/// Decorate a listing. Reads `busy_session_ids`/`latest_turn_statuses` once
/// for the whole page rather than per row.
fn decorate(ctx: &AppCtx, sessions: Vec<Session>) -> Result<Vec<SessionListItem>, BoughError> {
    let db = ctx.db.lock().unwrap();
    let busy = db.busy_session_ids()?;
    let statuses = db.latest_turn_statuses()?;
    sessions
        .into_iter()
        .map(|s| {
            let usage = db.session_usage(&s.id)?;
            let tokens = usage.input_tokens + usage.output_tokens + usage.reasoning_tokens;
            Ok(SessionListItem {
                busy: busy.contains(&s.id),
                last_turn_status: statuses.get(&s.id).copied(),
                cost_usd: (usage.cost_usd > 0.0).then_some(usage.cost_usd),
                tokens: (tokens > 0).then_some(tokens),
                session: s,
            })
        })
        .collect()
}

// ---- shared helpers ----------------------------------------------------------

/// Where the install's model defaults are read from. Injected on the ctx for
/// the same reason the TS `WithModelDefaults` seam exists: a handler test
/// asserting "a new session runs on `ctx.model`" must not pass or fail
/// depending on what the developer has pinned in their own `~/.bough`.
fn defaults_path_of(ctx: &AppCtx) -> PathBuf {
    ctx.model_defaults_path.clone().unwrap_or_else(default_path)
}

fn defaults_of(ctx: &AppCtx) -> ModelDefaults {
    load_defaults(&defaults_path_of(ctx))
}

/// 404 with a message naming the id, so a client's log says which was wrong.
fn require_session(ctx: &AppCtx, id: &str) -> Result<Session, BoughError> {
    ctx.db
        .lock()
        .unwrap()
        .get_session(id)?
        .ok_or_else(|| BoughError::not_found(format!("session {id} not found")))
}

fn param<'p>(params: &'p Params, name: &str) -> &'p str {
    params.get(name).map(String::as_str).unwrap_or("")
}

/// One query parameter, by simple form splitting — ids are opaque tokens used
/// verbatim.
fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    let query = query?;
    let prefix = format!("{name}=");
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix(prefix.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

fn effort_str(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh => "xhigh",
        Effort::Max => "max",
    }
}

fn kind_str(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Root => "root",
        SessionKind::Fork => "fork",
        SessionKind::Compaction => "compaction",
        SessionKind::Subagent => "subagent",
        SessionKind::WorkflowAgent => "workflow_agent",
        SessionKind::ScheduleRun => "schedule_run",
        SessionKind::Shell => "shell",
    }
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

// ---- workspace ---------------------------------------------------------------

/// Expand `~` and make the path absolute. Kept pure — `home` is a parameter —
/// so the expansion is testable without touching the real one. A relative path
/// resolves against the server's cwd, which is the only interpretation
/// available: the client and the server share a machine.
pub fn normalize_workspace(raw: &str, home: &str) -> String {
    let trimmed = raw.trim();
    let expanded: String = if trimmed == "~" {
        home.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        format!("{}/{}", home.trim_end_matches('/'), rest)
    } else {
        // `~name` is a login, not this user's home — it must not expand.
        trimmed.to_string()
    };
    let absolute = if expanded.starts_with('/') {
        expanded
    } else {
        let cwd = std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".to_string());
        format!("{cwd}/{expanded}")
    };
    // Lexical normalization, like `path.resolve`: collapse `.`, `..`, `//`.
    let mut parts: Vec<&str> = Vec::new();
    for seg in absolute.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Why this rejects at creation rather than letting the session exist: a
/// nonexistent checkout otherwise surfaces one turn later as a shell failure
/// inside the program, which reads as the agent being broken.
fn require_directory(path: &str) -> Result<(), BoughError> {
    match std::fs::metadata(path) {
        Err(_) => Err(BoughError::bad_request(format!(
            "workspace does not exist: {path}"
        ))),
        Ok(info) if !info.is_dir() => Err(BoughError::bad_request(format!(
            "workspace is not a directory: {path}"
        ))),
        Ok(_) => Ok(()),
    }
}

// ---- handlers ----------------------------------------------------------------

/// `GET /sessions` — the top level, every collapsing kind excluded.
/// `GET /sessions?originId=<id>` — the drill-in: everything that branched from
/// that session, collapsed kinds AND forks, in creation order.
pub fn list_sessions() -> Handler {
    handler(|req, ctx, _params| async move {
        let origin_id = query_param(req.uri().query(), "originId");
        if let Some(origin_id) = origin_id {
            // A typo'd id answering `[]` reads as "nothing branched from it",
            // which is a different fact.
            require_session(&ctx, &origin_id)?;
            let rows = ctx.db.lock().unwrap().sessions_by_origin(&origin_id)?;
            return Ok(json(&decorate(&ctx, rows)?, 200));
        }
        let rows: Vec<Session> = ctx
            .db
            .lock()
            .unwrap()
            .list_sessions()?
            .into_iter()
            .filter(|s| !is_collapsed(s))
            .collect();
        Ok(json(&decorate(&ctx, rows)?, 200))
    })
}

/// `POST /sessions` — a user-facing session: a root, or a fork of an existing
/// one. `kind` defaults from `parentId` because that is the only pair that is
/// ever consistent.
pub fn create_session() -> Handler {
    handler(|req, ctx, _params| async move {
        let body: CreateSessionBody = parse_body(req, Some(json!({}))).await?;
        let kind = body.kind.unwrap_or(if body.parent_id.is_some() {
            SessionKind::Fork
        } else {
            SessionKind::Root
        });

        // Derived visibility, enforced at the door: these kinds are reachable
        // only through an `originId` the creation body cannot carry.
        if is_collapsed_kind(kind) {
            return Err(BoughError::bad_request(format!(
                "kind '{}' is created by agent()/spawn(), not over HTTP — it needs an origin to collapse under",
                kind_str(kind)
            )));
        }

        let parent_id = body.parent_id.clone();
        if let Some(parent) = &parent_id {
            if ctx.db.lock().unwrap().get_session(parent)?.is_none() {
                return Err(BoughError::bad_request(format!(
                    "parent {parent} not found"
                )));
            }
        }

        let mut workspace: Option<String> = None;
        if let Some(raw) = body.workspace.as_deref().filter(|w| !w.is_empty()) {
            let home = dirs::home_dir()
                .map(|h| h.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".to_string());
            let normalized = normalize_workspace(raw, &home);
            require_directory(&normalized)?;
            workspace = Some(normalized);
        }

        let session = Session {
            id: uuid::Uuid::new_v4().to_string(),
            // Untitled until the cheap tier names it. Empty string rather than
            // a placeholder sentinel: the client decides how an unnamed
            // session reads.
            title: body.title.clone().unwrap_or_default(),
            kind,
            created_at: (ctx.now)(),
            parent_id,
            origin_id: None,
            origin_message_id: None,
            // `originDir` mirrors `workspace` at creation and is never
            // rewritten — the stable record of WHICH project this is for.
            workspace: workspace.clone(),
            origin_dir: workspace.clone(),
            base: None,
            model: None,
            effort: None,
            draft: None,
            context_tokens: None,
            cached_tokens: None,
            last_llm_at: None,
            outcome_ok: None,
        };

        {
            let db = ctx.db.lock().unwrap();
            db.create_session(session.clone())?;

            // Pins are separate writes, not create columns. Applied before
            // the announce so the event and the response carry the same
            // session the database holds. The body wins, then the install
            // default (`~/.bough/model.json`), then nothing.
            let pinned = defaults_of(&ctx);
            let model = body.model.clone().or(pinned.model);
            let effort = body
                .effort
                .clone()
                .or(pinned.effort.map(|e| effort_str(e).to_string()));
            if let Some(model) = &model {
                db.set_session_model(&session.id, Some(model))?;
            }
            if let Some(effort) = &effort {
                db.set_session_effort(&session.id, Some(effort))?;
            }
        }

        // T8.5 — the sha this session starts from, which is the whole of the
        // Changes rail's state (the working tree is the tip, `base` is where
        // the session began, and `git diff <base>` plus untracked files is
        // the change set).
        //
        // Recorded HERE, at creation, rather than on the first turn:
        // everything that runs in the workspace moves the tree, so a base
        // captured any later attributes work already done to the commit it
        // started from and hides it from review.
        //
        // Only for an EXPLICIT workspace. A session that named none runs in
        // the server's own directory, and recording that repository's HEAD
        // would give the session a change set full of somebody else's
        // uncommitted work — with a revert button on it.
        //
        // Best-effort by construction (`vcs/repodiff.rs`): a non-repo
        // workspace stores nothing and the rail reports "not a repository"
        // rather than an empty diff, and a git failure costs the diff, never
        // the session.
        if let Some(workspace) = &workspace {
            let _ = bough_core::vcs::repodiff::record_base(&ctx.db, &session.id, workspace).await;
        }

        let stored = {
            let db = ctx.db.lock().unwrap();
            db.get_session(&session.id)?
                .ok_or_else(|| BoughError::not_found(format!("session {} not found", session.id)))?
        };

        ctx.bus.publish(EventInput {
            r#type: EventType::SessionCreated,
            session_id: Some(stored.id.clone()),
            data: to_value(&stored),
        });
        Ok(json(&stored, 201))
    })
}

/// `GET /sessions/:id` — `{session, thread, usage, effectiveModel,
/// contextLimit, primedTags, projectRules}`. The reconnect path: a client that
/// dropped its SSE connection re-fetches here and reconciles by message id.
pub fn get_session() -> Handler {
    handler(|_req, ctx, params| async move {
        let session = require_session(&ctx, param(&params, "id"))?;
        // Session pin first, then the global default, then the built-in —
        // resolved the same way the runner resolves it.
        let model = session
            .model
            .clone()
            .or(ctx.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        let (thread, usage, tree) = {
            let db = ctx.db.lock().unwrap();
            (
                db.thread_for(&session.id)?,
                db.session_usage(&session.id)?,
                db.tree_usage(&session.id)?,
            )
        };
        let mut usage_v = to_value(&usage);
        if let Some(obj) = usage_v.as_object_mut() {
            obj.insert("tree".to_string(), to_value(&tree));
        }

        // The `AGENTS.md` files the NEXT turn will inject, resolved exactly as
        // the runner resolves them — read per call from disk, never stored.
        // `[]` for a session with no workspace.
        let project_rules: Vec<Value> = match session.workspace.as_deref() {
            Some(ws) => {
                let files = find_project_rules(Path::new(ws), Some(&bough_home()));
                rule_summaries(&files, Path::new(ws))
                    .into_iter()
                    .map(|s| {
                        json!({
                            "label": s.label,
                            "path": s.path.to_string_lossy(),
                            "bytes": s.bytes,
                        })
                    })
                    .collect()
            }
            None => Vec::new(),
        };

        Ok(json(
            &json!({
                "session": session,
                "thread": thread,
                "usage": usage_v,
                "effectiveModel": model,
                // Null when the vendored catalog does not know the model — the
                // client falls back to the raw count rather than inventing a
                // denominator.
                "contextLimit": bough_core::llm::pricing::context_window_for(&model),
                // The tag set this session was primed with. history/stats is a
                // wave-2 stub (row 2.10); `[]` is the documented no-history
                // answer, and the FIELD stays present per the v1 scope cut.
                "primedTags": Vec::<String>::new(),
                "projectRules": project_rules,
            }),
            200,
        ))
    })
}

/// `POST /sessions/:id/messages` — persist the user message, announce it, and
/// hand off to the turn runner.
///
/// **202, not 200**: the turn outlives this response and reports over
/// `/events`. The body carries the stored message so a client can reconcile it
/// against the `message.started` event by id without a second fetch.
pub fn post_message() -> Handler {
    handler(|req, ctx, params| async move {
        let session = require_session(&ctx, param(&params, "id"))?;
        let body: PostMessageBody = parse_body(req, None).await?;

        let text = body.text.trim().to_string();
        let images = body.images.unwrap_or_default();
        if text.is_empty() && images.is_empty() {
            return Err(BoughError::bad_request(
                "empty message: text or at least one image is required",
            ));
        }

        // A handoff draft is consumed by the first post: whatever the user
        // actually sent supersedes it. Announced, unlike the draft PUT,
        // because here the client is not the one that changed it.
        if session.draft.is_some() {
            let updated = {
                let db = ctx.db.lock().unwrap();
                db.set_session_draft(&session.id, None)?;
                db.get_session(&session.id)?
            };
            if let Some(updated) = updated {
                ctx.bus.publish(EventInput {
                    r#type: EventType::SessionUpdated,
                    session_id: Some(session.id.clone()),
                    data: to_value(&updated),
                });
            }
        }

        // Image bytes never enter the parts JSON — the part carries the path
        // the caller already copied under ~/.bough/attachments.
        let mut parts: Vec<Part> = Vec::new();
        if !text.is_empty() {
            parts.push(Part::Text { text });
        }
        for i in images {
            parts.push(Part::Image {
                path: i.path,
                media_type: i.media_type,
                name: i.name,
                size: i.size,
            });
        }

        let (stored, queued) = {
            let db = ctx.db.lock().unwrap();
            let stored = db.create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session.id.clone(),
                role: Role::User,
                parts,
                // A user message is complete when it lands; `pending` is the
                // supervisor's streaming flag.
                pending: false,
                created_at: (ctx.now)(),
            })?;
            // Keyword search is maintained on insert; idempotent, so a
            // rebuild and this path agree.
            db.index_message(&stored)?;
            // One turn per session: `queued` is computed from the running
            // turns BEFORE deciding to start — a message that lands mid-turn
            // drains into a fresh turn when the running one ends.
            let queued = db.busy_session_ids()?.contains(&session.id);
            (stored, queued)
        };

        ctx.bus.publish(EventInput {
            r#type: EventType::MessageStarted,
            session_id: Some(session.id.clone()),
            data: to_value(&stored),
        });

        if !queued {
            if let Some(starter) = ctx.turn_starter() {
                // Fire and forget: a turn runs for minutes and this response
                // is a 202. The containment is not politeness — an escaped
                // panic here would fail a request whose message already landed.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    starter.start_turn(&ctx, &session, &stored)
                }));
                if outcome.is_err() {
                    tracing::error!("failed to start turn for session {}", session.id);
                }
            }
        }

        Ok(json(&json!({ "message": stored, "queued": queued }), 202))
    })
}

/// `PUT /sessions/:id/draft` — the prefilled composer text. `null` clears.
///
/// **No `session.updated` event, deliberately.** The writer is the client that
/// is switching away; announcing its own write back to it would race the
/// prefill it is about to render and can blank a composer the user is typing
/// into.
pub fn put_draft() -> Handler {
    handler(|req, ctx, params| async move {
        let id = param(&params, "id").to_string();
        require_session(&ctx, &id)?;
        let body: SetDraftBody = parse_body(req, None).await?;
        ctx.db
            .lock()
            .unwrap()
            .set_session_draft(&id, body.draft.as_deref())?;
        Ok(json(&json!({ "ok": true, "draft": body.draft }), 200))
    })
}

/// `PATCH /sessions/:id` — the per-session `model` and `effort` overrides.
///
/// Absent field = leave alone; explicit `null` = clear the override and fall
/// back to the global default. The two are deliberately different, because
/// "don't touch this" and "there should be no pin here" are different requests
/// and a picker needs both.
pub fn patch_session() -> Handler {
    handler(|req, ctx, params| async move {
        let id = param(&params, "id").to_string();
        require_session(&ctx, &id)?;
        let body: PatchSessionBody = parse_body(req, None).await?;
        body.validate()?;
        let session = {
            let db = ctx.db.lock().unwrap();
            match &body.model {
                Patch::Keep => {}
                Patch::Clear => db.set_session_model(&id, None)?,
                Patch::Set(m) => db.set_session_model(&id, Some(m))?,
            }
            match body.effort {
                Patch::Keep => {}
                Patch::Clear => db.set_session_effort(&id, None)?,
                Patch::Set(e) => db.set_session_effort(&id, Some(effort_str(e)))?,
            }
            db.get_session(&id)?
                .ok_or_else(|| BoughError::not_found(format!("session {id} not found")))?
        };
        ctx.bus.publish(EventInput {
            r#type: EventType::SessionUpdated,
            session_id: Some(session.id.clone()),
            data: to_value(&session),
        });
        Ok(json(&session, 200))
    })
}

/// `GET /sessions/:id/usage` — this session's totals and its tree's, nothing
/// else. The poll-while-running cost meter: the number genuinely moves
/// mid-turn as the runner folds each round's usage in.
pub fn get_session_usage_h() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = param(&params, "id").to_string();
        require_session(&ctx, &id)?;
        let (usage, tree) = {
            let db = ctx.db.lock().unwrap();
            (db.session_usage(&id)?, db.tree_usage(&id)?)
        };
        Ok(json(&json!({ "usage": usage, "tree": tree }), 200))
    })
}

/// The `GET /model-settings` answer, shared with the PUT (which re-answers
/// after saving).
fn model_settings_json(ctx: &AppCtx) -> Value {
    // The picker's own write comes first: `ctx.model` is `BOUGH_MODEL` read
    // once at start-up and frozen for the process, so a stored default that
    // did not outrank it could never be reported back.
    let pinned = defaults_of(ctx);
    json!({
        "defaultModel": pinned.model.or(ctx.model.clone()).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        "cheapModel": cheap_model(),
        "defaultEffort": pinned.effort.or(ctx.effort),
    })
}

/// `GET /model-settings` — what a NEW conversation will run on. ALL tiers, not
/// just the frontier one: the cheap tier is set and bills continuously on
/// titles, ghost text and activity blurbs. `defaultEffort` is `null` when
/// nothing pins one — a different fact from "low".
pub fn get_model_settings_h() -> Handler {
    handler(|_req, ctx, _params| async move { Ok(json(&model_settings_json(&ctx), 200)) })
}

/// `PUT /model-settings` — pin what a NEW conversation runs on. A partial: an
/// absent key is left alone, and an explicit `null` clears the pin.
pub fn put_model_settings_h() -> Handler {
    handler(|req, ctx, _params| async move {
        let body: PutModelSettingsBody = parse_body(req, None).await?;
        body.validate()?;
        let current = defaults_of(&ctx);
        save_defaults(
            &ModelDefaults {
                model: body.model.apply(current.model),
                effort: body.effort.apply(current.effort),
            },
            &defaults_path_of(&ctx),
        );
        Ok(json(&model_settings_json(&ctx), 200))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions, Dispatcher};
    use crate::http::testutil::{self, Fixture};
    use bough_core::schema::parts::{Turn, TurnStatus, Usage};
    use serde_json::json as j;

    fn call(fx: &Fixture) -> Dispatcher {
        create_handler(fx.ctx.clone(), CreateHandlerOptions::default())
    }

    async fn new_session(fx: &Fixture, body: Value) -> Session {
        let res = call(fx)
            .call(testutil::req("POST", "/sessions", Some(body)))
            .await;
        assert_eq!(res.status(), 201);
        serde_json::from_value(testutil::body_json(res).await).unwrap()
    }

    /// Insert a delegated session directly — `agent()`/`spawn()` own this path.
    fn seed_delegated(fx: &Fixture, kind: SessionKind, origin: &Session, title: &str) -> Session {
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: uuid::Uuid::new_v4().to_string(),
                title: title.to_string(),
                kind,
                created_at: (fx.ctx.now)(),
                // A subagent's thread is task-only: no parent, so no inherited
                // context.
                parent_id: None,
                origin_id: Some(origin.id.clone()),
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

    fn seed_running_turn(fx: &Fixture, session_id: &str) {
        let db = fx.ctx.db.lock().unwrap();
        let m = db
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role: Role::Supervisor,
                parts: vec![],
                pending: true,
                created_at: (fx.ctx.now)(),
            })
            .unwrap();
        db.create_turn(Turn {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            message_id: m.id,
            status: TurnStatus::Running,
            step: "model".to_string(),
            created_at: (fx.ctx.now)(),
            updated_at: (fx.ctx.now)(),
            error: None,
            usage: None,
        })
        .unwrap();
    }

    fn text_of(m: &Value) -> String {
        m["parts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["type"] == "text")
            .map(|p| p["text"].as_str().unwrap())
            .collect()
    }

    // ---- derived visibility --------------------------------------------------

    #[tokio::test]
    async fn a_subagent_session_is_absent_top_level_and_present_under_its_origin() {
        let fx = testutil::fixture();
        let root = new_session(&fx, j!({"title": "root"})).await;
        let child = seed_delegated(&fx, SessionKind::Subagent, &root, "review handlers");

        let top = testutil::body_json(call(&fx).call(testutil::get("/sessions")).await).await;
        let ids: Vec<&str> = top
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![root.id.as_str()]);

        let drill = testutil::body_json(
            call(&fx)
                .call(testutil::get(&format!("/sessions?originId={}", root.id)))
                .await,
        )
        .await;
        let drill = drill.as_array().unwrap();
        assert_eq!(drill.len(), 1);
        assert_eq!(drill[0]["id"], child.id.as_str());
        assert_eq!(drill[0]["title"], "review handlers");
        assert_eq!(drill[0]["kind"], "subagent");
    }

    #[tokio::test]
    async fn a_workflow_agent_session_collapses_the_same_way_a_subagent_does() {
        let fx = testutil::fixture();
        let root = new_session(&fx, j!({})).await;
        let agent = seed_delegated(&fx, SessionKind::WorkflowAgent, &root, "verify: title");
        let top = testutil::body_json(call(&fx).call(testutil::get("/sessions")).await).await;
        assert!(!top
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == agent.id.as_str()));
        let drill = testutil::body_json(
            call(&fx)
                .call(testutil::get(&format!("/sessions?originId={}", root.id)))
                .await,
        )
        .await;
        assert_eq!(drill.as_array().unwrap()[0]["id"], agent.id.as_str());
    }

    #[tokio::test]
    async fn roots_forks_and_compactions_are_always_listed() {
        let fx = testutil::fixture();
        let root = new_session(&fx, j!({"title": "root"})).await;
        let fork = new_session(&fx, j!({"title": "fork", "parentId": root.id})).await;
        let compaction = new_session(
            &fx,
            j!({"title": "compaction", "parentId": root.id, "kind": "compaction"}),
        )
        .await;
        assert_eq!(fork.kind, SessionKind::Fork); // derived from parentId
        let top = testutil::body_json(call(&fx).call(testutil::get("/sessions")).await).await;
        let mut ids: Vec<String> = top
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        let mut want = vec![root.id, fork.id, compaction.id];
        want.sort();
        assert_eq!(ids, want);
    }

    #[tokio::test]
    async fn the_drill_in_returns_every_branch_collapsed_kinds_and_forks_alike() {
        let fx = testutil::fixture();
        let root = new_session(&fx, j!({})).await;
        let sub = seed_delegated(&fx, SessionKind::Subagent, &root, "a");
        // A fork of the same session shares the origin edge; splitting the two
        // would make the tree view ask twice for one node's children.
        let branch = fx
            .ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: uuid::Uuid::new_v4().to_string(),
                title: "branch".to_string(),
                kind: SessionKind::Fork,
                created_at: (fx.ctx.now)(),
                parent_id: Some(root.id.clone()),
                origin_id: Some(root.id.clone()),
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
        let drill = testutil::body_json(
            call(&fx)
                .call(testutil::get(&format!("/sessions?originId={}", root.id)))
                .await,
        )
        .await;
        let mut ids: Vec<String> = drill
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        let mut want = vec![sub.id, branch.id];
        want.sort();
        assert_eq!(ids, want);
    }

    #[tokio::test]
    async fn an_unknown_origin_id_is_a_404_not_an_empty_list() {
        let fx = testutil::fixture();
        let res = call(&fx)
            .call(testutil::get("/sessions?originId=nope"))
            .await;
        assert_eq!(res.status(), 404);
        let body = testutil::body_json(res).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("session nope not found"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn post_sessions_refuses_a_collapsed_kind_no_listing_could_reach() {
        let fx = testutil::fixture();
        for kind in ["subagent", "workflow_agent"] {
            let res = call(&fx)
                .call(testutil::req("POST", "/sessions", Some(j!({"kind": kind}))))
                .await;
            assert_eq!(res.status(), 400);
            let body = testutil::body_json(res).await;
            assert!(
                body["error"].as_str().unwrap().contains("agent()/spawn()"),
                "{body}"
            );
        }
        // And nothing was persisted by the refused calls.
        assert!(fx
            .ctx
            .db
            .lock()
            .unwrap()
            .list_sessions()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn is_collapsed_is_the_whole_visibility_rule_no_stored_flag_exists() {
        let base = |kind| Session {
            id: "x".into(),
            title: "t".into(),
            kind,
            created_at: 0,
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
        };
        assert!(is_collapsed(&base(SessionKind::Subagent)));
        assert!(is_collapsed(&base(SessionKind::WorkflowAgent)));
        assert!(!is_collapsed(&base(SessionKind::Root)));
        assert!(!is_collapsed(&base(SessionKind::Fork)));
        assert!(!is_collapsed(&base(SessionKind::Compaction)));
    }

    // ---- creation ------------------------------------------------------------

    #[tokio::test]
    async fn post_sessions_announces_the_session_it_stored_byte_for_byte() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({"title": "hello"})).await;
        assert_eq!(session.kind, SessionKind::Root);
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .get_session(&session.id)
                .unwrap()
                .unwrap(),
            session
        );
        let events = fx.events.lock().unwrap();
        let created: Vec<_> = events
            .iter()
            .filter(|e| e.r#type == EventType::SessionCreated)
            .collect();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].data, serde_json::to_value(&session).unwrap());
        assert_eq!(created[0].session_id.as_deref(), Some(session.id.as_str()));
    }

    #[tokio::test]
    async fn model_and_effort_pins_are_stored_before_the_announce_not_after() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({"model": "openai:gpt-5", "effort": "high"})).await;
        assert_eq!(session.model.as_deref(), Some("openai:gpt-5"));
        assert_eq!(session.effort.as_deref(), Some("high"));
        let events = fx.events.lock().unwrap();
        let created = events
            .iter()
            .find(|e| e.r#type == EventType::SessionCreated)
            .unwrap();
        // The event carried the pins too: a client that renders from the event
        // and one that renders from the response must not disagree.
        assert_eq!(created.data, serde_json::to_value(&session).unwrap());
    }

    #[tokio::test]
    async fn an_unknown_parent_is_a_400_naming_it_and_nothing_is_created() {
        let fx = testutil::fixture();
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                "/sessions",
                Some(j!({"parentId": "ghost"})),
            ))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("parent ghost not found"),
            "{body}"
        );
        assert!(fx
            .ctx
            .db
            .lock()
            .unwrap()
            .list_sessions()
            .unwrap()
            .is_empty());
        assert!(fx.events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_workspace_that_exists_is_recorded_and_origin_dir_mirrors_it() {
        let fx = testutil::fixture();
        let dir = std::env::temp_dir().join(format!("bough-ws-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let session = new_session(&fx, j!({"workspace": dir.to_string_lossy()})).await;
        let want = normalize_workspace(&dir.to_string_lossy(), "/");
        assert_eq!(session.workspace.as_deref(), Some(want.as_str()));
        assert_eq!(session.origin_dir.as_deref(), Some(want.as_str()));
        let _ = std::fs::remove_dir(&dir);
    }

    #[tokio::test]
    async fn a_workspace_that_does_not_exist_is_rejected_at_creation() {
        let fx = testutil::fixture();
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                "/sessions",
                Some(j!({"workspace": "/no/such/checkout"})),
            ))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("workspace does not exist"),
            "{body}"
        );
        assert!(fx
            .ctx
            .db
            .lock()
            .unwrap()
            .list_sessions()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_workspace_that_is_a_file_not_a_directory_says_so() {
        let fx = testutil::fixture();
        let dir = std::env::temp_dir().join(format!("bough-ws-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not-a-checkout");
        std::fs::write(&file, "").unwrap();
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                "/sessions",
                Some(j!({"workspace": file.to_string_lossy()})),
            ))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_json(res).await;
        assert!(
            body["error"].as_str().unwrap().contains("not a directory"),
            "{body}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_workspace_expands_tilde_and_absolutizes_with_home_as_a_parameter() {
        assert_eq!(normalize_workspace("~", "/home/dev"), "/home/dev");
        assert_eq!(
            normalize_workspace("~/src/bough", "/home/dev"),
            "/home/dev/src/bough"
        );
        assert_eq!(normalize_workspace("  /srv/x  ", "/home/dev"), "/srv/x");
        // `~name` is a login, not this user's home — it must not expand.
        assert_ne!(
            normalize_workspace("~other/x", "/home/dev"),
            "/home/dev/other/x"
        );
    }

    // ---- the session view ----------------------------------------------------

    #[tokio::test]
    async fn get_session_returns_thread_with_ancestors_before_own_messages() {
        let fx = testutil::fixture();
        let root = new_session(&fx, j!({"title": "root"})).await;
        call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", root.id),
                Some(j!({"text": "one"})),
            ))
            .await;
        let child = new_session(&fx, j!({"parentId": root.id})).await;
        call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", child.id),
                Some(j!({"text": "two"})),
            ))
            .await;

        let body = testutil::body_json(
            call(&fx)
                .call(testutil::get(&format!("/sessions/{}", child.id)))
                .await,
        )
        .await;
        assert_eq!(body["session"]["id"], child.id.as_str());
        let thread = body["thread"].as_array().unwrap();
        // Thread inheritance: the ancestor's message is present without being
        // copied.
        assert_eq!(
            thread.iter().map(text_of).collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        assert_eq!(
            thread
                .iter()
                .map(|m| m["sessionId"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![root.id.as_str(), child.id.as_str()]
        );
        assert_eq!(body["usage"]["costUsd"], 0.0);
        assert_eq!(body["usage"]["tree"]["costUsd"], 0.0);
        // The reconnect fields are present even in v1's degraded answers.
        assert_eq!(body["effectiveModel"], "test-model");
        assert_eq!(body["primedTags"], j!([]));
        assert_eq!(body["projectRules"], j!([]));
    }

    #[tokio::test]
    async fn get_session_on_an_unknown_id_is_a_404_naming_it() {
        let fx = testutil::fixture();
        let res = call(&fx).call(testutil::get("/sessions/ghost")).await;
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"error": "session ghost not found"})
        );
    }

    // ---- messages ------------------------------------------------------------

    #[tokio::test]
    async fn post_messages_persists_announces_and_hands_off_to_the_turn_runner() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "  ship it  "})),
            ))
            .await;
        assert_eq!(res.status(), 202);
        let body = testutil::body_json(res).await;
        assert_eq!(body["queued"], false);
        let message = &body["message"];
        assert_eq!(message["role"], "user");
        assert_eq!(message["pending"], false);
        assert_eq!(text_of(message), "ship it");
        let stored = fx.ctx.db.lock().unwrap().messages_for(&session.id).unwrap();
        assert_eq!(
            serde_json::to_value(&stored).unwrap(),
            j!([message.clone()])
        );

        let events = fx.events.lock().unwrap();
        let started: Vec<_> = events
            .iter()
            .filter(|e| e.r#type == EventType::MessageStarted)
            .collect();
        assert_eq!(started.len(), 1);
        assert_eq!(&started[0].data, message);
        drop(events);

        // The turn runner receives the session and the exact stored message.
        let started = fx.started.lock().unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(serde_json::to_value(&started[0].1).unwrap(), *message);
        assert_eq!(started[0].0.id, session.id);
    }

    #[tokio::test]
    async fn a_message_posted_while_a_turn_runs_is_persisted_and_queued_not_started() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        // A running turn is what makes the session busy (one turn per session).
        seed_running_turn(&fx, &session.id);

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "also this"})),
            ))
            .await;
        assert_eq!(res.status(), 202);
        let body = testutil::body_json(res).await;
        assert_eq!(body["queued"], true);
        // Persisted and announced — never dropped; only the START is deferred.
        let id = body["message"]["id"].as_str().unwrap();
        assert!(fx.ctx.db.lock().unwrap().get_message(id).unwrap().is_some());
        assert!(fx
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.r#type == EventType::MessageStarted));
        assert!(fx.started.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn image_attachments_become_parts_carrying_a_path_never_bytes() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        let image = j!({
            "path": "/home/dev/.bough/attachments/a.png",
            "mediaType": "image/png",
            "name": "a.png",
            "size": 1234,
        });
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "look", "images": [image]})),
            ))
            .await;
        let body = testutil::body_json(res).await;
        let mut want_image = image.as_object().unwrap().clone();
        want_image.insert("type".to_string(), j!("image"));
        assert_eq!(
            body["message"]["parts"],
            j!([{"type": "text", "text": "look"}, want_image])
        );
    }

    #[tokio::test]
    async fn an_image_only_message_is_allowed_an_entirely_empty_one_is_a_400() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        let image = j!({"path": "/a.png", "mediaType": "image/png", "name": "a.png", "size": 1});
        let ok = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "", "images": [image]})),
            ))
            .await;
        assert_eq!(ok.status(), 202);
        let body = testutil::body_json(ok).await;
        assert_eq!(
            body["message"]["parts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| &p["type"])
                .collect::<Vec<_>>(),
            vec!["image"]
        );

        let empty = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "   "})),
            ))
            .await;
        assert_eq!(empty.status(), 400);
        let err = testutil::body_json(empty).await;
        assert!(
            err["error"].as_str().unwrap().contains("empty message"),
            "{err}"
        );
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .messages_for(&session.id)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn posting_into_an_unknown_session_is_a_404_and_starts_nothing() {
        let fx = testutil::fixture();
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                "/sessions/ghost/messages",
                Some(j!({"text": "hi"})),
            ))
            .await;
        assert_eq!(res.status(), 404);
        assert!(fx.started.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_first_post_consumes_the_handoff_draft_and_announces_the_clear() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        fx.ctx
            .db
            .lock()
            .unwrap()
            .set_session_draft(&session.id, Some("a prefilled opening prompt"))
            .unwrap();
        call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "my own words"})),
            ))
            .await;
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .get_session(&session.id)
                .unwrap()
                .unwrap()
                .draft,
            None
        );
        let events = fx.events.lock().unwrap();
        let updated: Vec<_> = events
            .iter()
            .filter(|e| e.r#type == EventType::SessionUpdated)
            .collect();
        assert_eq!(updated.len(), 1);
        // Nullish, not absent — sessions.test.ts:512 asserts `draft ?? null`.
        assert!(
            updated[0].data["draft"].is_null(),
            "the announced row carries no draft"
        );
    }

    #[tokio::test]
    async fn a_turn_starter_that_panics_is_contained_the_post_still_answers_202() {
        let fx = testutil::fixture();
        testutil::install_panicking_starter(&fx.ctx, "no llm configured");
        let session = new_session(&fx, j!({})).await;
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "go"})),
            ))
            .await;
        assert_eq!(res.status(), 202);
        // The message survived the failed start: the transcript is the source
        // of truth.
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .messages_for(&session.id)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn no_turn_starter_wired_the_message_still_lands() {
        let fx = testutil::fixture_bare();
        let session = new_session(&fx, j!({})).await;
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "hi"})),
            ))
            .await;
        assert_eq!(res.status(), 202);
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .messages_for(&session.id)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_posted_message_is_keyword_searchable_immediately() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        call(&fx)
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", session.id),
                Some(j!({"text": "reticulating splines"})),
            ))
            .await;
        let hits = fx
            .ctx
            .db
            .lock()
            .unwrap()
            .search_messages("splines", None, None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, session.id);
    }

    // ---- listing decorations -------------------------------------------------

    #[tokio::test]
    async fn listing_carries_busy_and_last_turn_status_derived_from_turns_not_columns() {
        let fx = testutil::fixture();
        let idle = new_session(&fx, j!({"title": "idle"})).await;
        let busy = new_session(&fx, j!({"title": "busy"})).await;
        seed_running_turn(&fx, &busy.id);

        let rows = testutil::body_json(call(&fx).call(testutil::get("/sessions")).await).await;
        let by_id = |id: &str| {
            rows.as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
                .cloned()
                .unwrap()
        };
        assert_eq!(by_id(&busy.id)["busy"], true);
        assert_eq!(by_id(&busy.id)["lastTurnStatus"], "running");
        assert_eq!(by_id(&idle.id)["busy"], false);
        assert_eq!(by_id(&idle.id).get("lastTurnStatus"), None);
        // Cost is omitted rather than zero, so an untouched row stays small.
        assert_eq!(by_id(&idle.id).get("costUsd"), None);
    }

    #[tokio::test]
    async fn a_listed_session_carries_tokens_omitted_when_zero_and_excluding_cache_traffic() {
        let fx = testutil::fixture();
        let spent = new_session(&fx, j!({"title": "spent"})).await;
        let idle = new_session(&fx, j!({"title": "idle"})).await;
        fx.ctx
            .db
            .lock()
            .unwrap()
            .add_session_usage(
                &spent.id,
                &Usage {
                    input_tokens: 1_000,
                    output_tokens: 200,
                    reasoning_tokens: Some(50),
                    cache_read_tokens: Some(90_000),
                    cache_write_tokens: Some(4_000),
                    cost_usd: Some(0.1),
                },
                (fx.ctx.now)(),
            )
            .unwrap();

        let rows = testutil::body_json(call(&fx).call(testutil::get("/sessions")).await).await;
        let by_id = |id: &str| {
            rows.as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
                .cloned()
                .unwrap()
        };
        assert_eq!(by_id(&spent.id)["tokens"], 1_250);
        assert_eq!(by_id(&idle.id).get("tokens"), None);
    }

    // ---- draft ---------------------------------------------------------------

    #[tokio::test]
    async fn put_draft_stores_the_text_and_deliberately_emits_no_event() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        let before = fx.events.lock().unwrap().len();
        let res = call(&fx)
            .call(testutil::req(
                "PUT",
                &format!("/sessions/{}/draft", session.id),
                Some(j!({"draft": "half-typed"})),
            ))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"ok": true, "draft": "half-typed"})
        );
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .get_session(&session.id)
                .unwrap()
                .unwrap()
                .draft
                .as_deref(),
            Some("half-typed")
        );
        // The writer is the client switching away; echoing session.updated
        // back at it would race the prefill it is about to render.
        assert_eq!(fx.events.lock().unwrap().len(), before);
    }

    #[tokio::test]
    async fn put_draft_with_null_clears_it() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        call(&fx)
            .call(testutil::req(
                "PUT",
                &format!("/sessions/{}/draft", session.id),
                Some(j!({"draft": "x"})),
            ))
            .await;
        call(&fx)
            .call(testutil::req(
                "PUT",
                &format!("/sessions/{}/draft", session.id),
                Some(j!({"draft": null})),
            ))
            .await;
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .get_session(&session.id)
                .unwrap()
                .unwrap()
                .draft,
            None
        );
    }

    #[tokio::test]
    async fn put_draft_rejects_a_missing_session_and_a_wrong_shaped_body() {
        let fx = testutil::fixture();
        let missing = call(&fx)
            .call(testutil::req(
                "PUT",
                "/sessions/ghost/draft",
                Some(j!({"draft": "x"})),
            ))
            .await;
        assert_eq!(missing.status(), 404);
        let session = new_session(&fx, j!({})).await;
        let bad = call(&fx)
            .call(testutil::req(
                "PUT",
                &format!("/sessions/{}/draft", session.id),
                Some(j!({"draft": 42})),
            ))
            .await;
        assert_eq!(bad.status(), 400);
        let body = testutil::body_json(bad).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("invalid body: "),
            "{body}"
        );
    }

    // ---- PATCH (pins) --------------------------------------------------------

    #[tokio::test]
    async fn patch_session_pins_a_model_and_an_explicit_null_clears_it() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({"title": "pin me"})).await;
        let patch = |body: Value| {
            let d = call(&fx);
            let path = format!("/sessions/{}", session.id);
            async move {
                testutil::body_json(d.call(testutil::req("PATCH", &path, Some(body))).await).await
            }
        };

        let pinned = patch(j!({"model": "openai:gpt-x"})).await;
        assert_eq!(pinned["model"], "openai:gpt-x");

        // An ABSENT field leaves the pin alone — the case a naive port gets
        // wrong by collapsing undefined into null and silently unpinning.
        let untouched = patch(j!({"effort": "high"})).await;
        assert_eq!(untouched["model"], "openai:gpt-x");
        assert_eq!(untouched["effort"], "high");

        // An EXPLICIT null clears it — back to the global default. "Cleared"
        // is nullish, not key-absent: `toSession` always emits the key (as
        // `null`), and sessions.test.ts:694 asserts `cleared.model ?? null`.
        let cleared = patch(j!({"model": null})).await;
        assert!(cleared["model"].is_null());
        assert_eq!(
            cleared["effort"], "high",
            "clearing one override must not clear the other"
        );
    }

    #[tokio::test]
    async fn patch_session_rejects_an_unknown_effort_and_a_missing_session() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        let bad = call(&fx)
            .call(testutil::req(
                "PATCH",
                &format!("/sessions/{}", session.id),
                Some(j!({"effort": "turbo"})),
            ))
            .await;
        assert_eq!(bad.status(), 400);
        let missing = call(&fx)
            .call(testutil::req(
                "PATCH",
                "/sessions/nope",
                Some(j!({"model": "x"})),
            ))
            .await;
        assert_eq!(missing.status(), 404);
    }

    #[tokio::test]
    async fn patch_session_persists_model_and_effort_to_the_row_not_just_the_response() {
        let fx = testutil::fixture();
        let session = new_session(&fx, j!({})).await;
        call(&fx)
            .call(testutil::req(
                "PATCH",
                &format!("/sessions/{}", session.id),
                Some(j!({"model": "claude-opus-4-8", "effort": "high"})),
            ))
            .await;
        let stored = fx
            .ctx
            .db
            .lock()
            .unwrap()
            .get_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(
            stored.effort.as_deref(),
            Some("high"),
            "effort landing as null is the reported regression"
        );
    }

    // ---- the live cost meter -------------------------------------------------

    #[tokio::test]
    async fn get_usage_answers_both_totals_and_404s_on_an_unknown_id() {
        let fx = testutil::fixture();
        let root = new_session(&fx, j!({"title": "root"})).await;
        let child = seed_delegated(&fx, SessionKind::Subagent, &root, "delegated");
        {
            let db = fx.ctx.db.lock().unwrap();
            db.add_session_usage(
                &root.id,
                &Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    reasoning_tokens: Some(5),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cost_usd: Some(0.25),
                },
                (fx.ctx.now)(),
            )
            .unwrap();
            db.add_session_usage(
                &child.id,
                &Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    reasoning_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cost_usd: Some(0.5),
                },
                (fx.ctx.now)(),
            )
            .unwrap();
        }

        let body = testutil::body_json(
            call(&fx)
                .call(testutil::get(&format!("/sessions/{}/usage", root.id)))
                .await,
        )
        .await;
        assert_eq!(body["usage"]["inputTokens"], 100);
        assert_eq!(body["usage"]["outputTokens"], 20);
        assert_eq!(body["usage"]["costUsd"], 0.25);
        // The subagent's spend rolls up here and nowhere in `usage`.
        assert_eq!(body["tree"]["costUsd"], 0.75);
        assert_eq!(body["tree"]["inputTokens"], 110);

        assert_eq!(
            call(&fx)
                .call(testutil::get("/sessions/ghost/usage"))
                .await
                .status(),
            404
        );
    }

    // ---- model settings ------------------------------------------------------

    #[tokio::test]
    async fn get_model_settings_names_every_tier_not_just_the_frontier_one() {
        let fx = testutil::fixture();
        let body =
            testutil::body_json(call(&fx).call(testutil::get("/model-settings")).await).await;
        assert_eq!(body["defaultModel"], "test-model");
        assert_eq!(body["cheapModel"], cheap_model());
        assert_eq!(body["defaultEffort"], Value::Null);
    }

    #[tokio::test]
    async fn get_model_settings_reports_a_pinned_global_effort() {
        let fx = testutil::fixture();
        let mut ctx = fx.ctx.clone();
        ctx.effort = Some(Effort::High);
        let body = testutil::body_json(
            create_handler(ctx, CreateHandlerOptions::default())
                .call(testutil::get("/model-settings"))
                .await,
        )
        .await;
        assert_eq!(body["defaultEffort"], "high");
    }

    #[tokio::test]
    async fn put_model_settings_is_a_partial_absent_keeps_null_clears() {
        let fx = testutil::fixture();
        let d = call(&fx);
        let put = |body: Value| {
            let d = d.clone();
            async move {
                testutil::body_json(
                    d.call(testutil::req("PUT", "/model-settings", Some(body)))
                        .await,
                )
                .await
            }
        };

        let pinned = put(j!({"model": "openai:gpt-x", "effort": "low"})).await;
        assert_eq!(pinned["defaultModel"], "openai:gpt-x");
        assert_eq!(pinned["defaultEffort"], "low");

        // Absent model: the pin is kept while effort changes.
        let kept = put(j!({"effort": "max"})).await;
        assert_eq!(kept["defaultModel"], "openai:gpt-x");
        assert_eq!(kept["defaultEffort"], "max");

        // Explicit nulls clear both — back to ctx.model and no effort.
        let cleared = put(j!({"model": null, "effort": null})).await;
        assert_eq!(cleared["defaultModel"], "test-model");
        assert_eq!(cleared["defaultEffort"], Value::Null);
    }
}
