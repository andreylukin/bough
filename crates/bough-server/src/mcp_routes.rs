//! The MCP registry/grants/connections routes (port of the HTTP surface at the
//! bottom of `src/mcp/status.ts`).
//!
//! THE SAME STATE, MUTATED. Every handler here answers with a body built by
//! `mcp::status::mcp_status_for` — the registry, the grants and the connections
//! — so the model (`bough mcp`), the panel and the response to a mutation cannot
//! be looking at different MCP states, and none of them can be looking at a
//! stale one. The domain lives in `bough-core::mcp`; this file is the only place
//! that turns it into a `Response`.
//!
//! WHERE THE REGISTRY IS. Handlers read it from the PROCESS MANAGER's own config
//! (`mcp_manager().config()`), which in production is the default
//! `~/.bough/mcp.json` and in a test is whatever hermetic file the test's manager
//! was built with. One object decides where the registry is, and it is the same
//! one that holds the connections — a handler that read a different file than the
//! connector would report a catalog nobody could call.
//!
//! The three `/mcp/servers/:name/auth` verbs and the OAuth callback are row
//! 3.5's, in `mcp_oauth.rs`.

use serde_json::{json, Map, Value};

use bough_core::errors::BoughError;
use bough_core::mcp::config::{
    is_stdio, load_registry, mcp_error, remove_server, require_server, revoke_everywhere,
    save_registry, set_activation, ttl_to_expires, upsert_server, McpConfigOptions,
};
use bough_core::mcp::manager::{mcp_manager, require_granted, GrantCtx, SpawnCtx, SHARED_SCOPE};
use bough_core::mcp::status::{mcp_status_for, McpStatus, McpStatusOptions};
use bough_core::schema::requests::McpActivationBody;
use bough_core::types::AppCtx;

use crate::http::{handler, json as json_res, parse_body, Handler, Params};

// ---- shared plumbing ---------------------------------------------------------

/// The registry/grant store every handler reads: the process manager's own.
fn config() -> McpConfigOptions {
    mcp_manager().config()
}

/// The `?session=` scope, validated against the database when present.
fn scope_of(req: &axum::extract::Request, ctx: &AppCtx) -> Result<Option<String>, BoughError> {
    let Some(session_id) = query_param(req.uri().query(), "session") else {
        return Ok(None);
    };
    if ctx.db.lock().unwrap().get_session(&session_id)?.is_none() {
        return Err(BoughError::not_found(format!(
            "no session {session_id} — GET /sessions lists them."
        )));
    }
    Ok(Some(session_id))
}

fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    let query = query?;
    let prefix = format!("{name}=");
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(prefix.as_str()).map(|v| v.to_string()))
}

fn param(params: &Params, name: &str) -> String {
    params.get(name).cloned().unwrap_or_default()
}

/// Where a server spawned for this session runs: its checkout, like every turn's.
fn workspace_of(ctx: &AppCtx, session_id: Option<&str>) -> String {
    let fallback = || {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    };
    let Some(id) = session_id else {
        return fallback();
    };
    ctx.db
        .lock()
        .unwrap()
        .get_session_runtime(id)
        .ok()
        .and_then(|r| r.workspace)
        .unwrap_or_else(fallback)
}

/// The whole state for one scope — the body of every reply in this file.
fn state_of(session_id: Option<&str>) -> McpStatus {
    mcp_status_for(&McpStatusOptions {
        config: config(),
        session_id: session_id.map(|s| s.to_string()),
        ..Default::default()
    })
}

/// `{…state, …extra}` — the TS `{...stateOf(id), changed}` spread, which every
/// client reads as one flat document.
fn state_with(session_id: Option<&str>, extra: Value) -> Value {
    let mut body = serde_json::to_value(state_of(session_id)).unwrap_or_else(|_| json!({}));
    if let (Some(map), Value::Object(more)) = (body.as_object_mut(), extra) {
        for (k, v) in more {
            map.insert(k, v);
        }
    }
    body
}

/// The request body as JSON, or `None` for an absent/unparseable one — the TS
/// `await req.json().catch(() => null)`.
async fn body_of(req: axum::extract::Request) -> Option<Value> {
    match parse_body::<Value>(req, Some(Value::Null)).await {
        Ok(Value::Null) => None,
        Ok(value) => Some(value),
        Err(_) => None,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- handlers ----------------------------------------------------------------

/// `GET /mcp/servers[?session=]` — the whole state, for the panel, the CLI and
/// the model. The four keys are fixed even when everything in them is empty.
pub fn get_mcp_servers() -> Handler {
    handler(|req, ctx, _params| async move {
        let session_id = scope_of(&req, &ctx)?;
        Ok(json_res(&state_of(session_id.as_deref()), 200))
    })
}

/// `PUT /mcp/servers` — replace the whole registry (GET → edit → PUT).
///
/// Only entries that CHANGED lose their connections. A bulk edit that reset every
/// session's unrelated servers would make one typo cost every open session its
/// live MCP state; grants are untouched either way (`save_registry` merges them
/// back, and drops only the ones naming a server that no longer exists).
pub fn put_mcp_servers() -> Handler {
    handler(|req, ctx, _params| async move {
        let session_id = scope_of(&req, &ctx)?;
        let Some(body) = body_of(req).await else {
            return Err(mcp_error(
                400,
                "the body must be the registry document: {\"servers\": {…}}.",
            ));
        };
        let before = load_registry(&config()).servers;
        let registry = save_registry(&body, &config())?;
        let changed: Vec<String> = before
            .keys()
            .chain(registry.servers.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|name| before.get(name) != registry.servers.get(name))
            .collect();
        for name in &changed {
            mcp_manager().drop_server(name).await;
        }
        Ok(json_res(
            &state_with(session_id.as_deref(), json!({ "changed": changed })),
            200,
        ))
    })
}

/// `PUT /mcp/servers/:name` — register or update ONE entry.
///
/// The shape the panel uses, so a registration cannot mangle sibling entries (or
/// their `${VAR}` secret references) in a read-modify-write of the whole file.
/// Validated by `config::ServerConfig`, the schema the FILE is written with — a
/// narrower wire subset would silently strip a stdio entry's `cwd`.
pub fn put_mcp_server() -> Handler {
    handler(|req, ctx, params| async move {
        let name = param(&params, "name");
        let session_id = scope_of(&req, &ctx)?;
        let Some(body) = body_of(req).await else {
            return Err(mcp_error(
                400,
                "the body must be one server entry: {\"command\": \"…\", \"args\": []} for a \
                 local server, or {\"url\": \"https://…\"} for a remote one.",
            ));
        };
        upsert_server(&name, &body, &config())?;
        // A changed definition cannot keep serving from the old one.
        mcp_manager().drop_server(&name).await;
        Ok(json_res(&state_of(session_id.as_deref()), 200))
    })
}

/// `DELETE /mcp/servers/:name` — remove the entry, its grants, and its
/// connections.
pub fn delete_mcp_server() -> Handler {
    handler(|req, ctx, params| async move {
        let name = param(&params, "name");
        let session_id = scope_of(&req, &ctx)?;
        if !remove_server(&name, &config())? {
            return Err(BoughError::not_found(format!(
                "no MCP server named \"{name}\" is registered, so there is nothing to remove."
            )));
        }
        mcp_manager().drop_server(&name).await;
        Ok(json_res(&state_of(session_id.as_deref()), 200))
    })
}

/// `POST /mcp/servers/:name/connect?session=` — connect now and report the
/// catalog.
///
/// The "prove it" step: without it, a registration or a grant could only be
/// tested by starting a turn, and a typo'd command surfaced a turn later as an
/// unavailable server. Connecting is NOT a grant — the grant is checked on every
/// call — so this proves the command works and nothing more.
///
/// A server that fails to start answers **200** with `connected: false` and the
/// reason. That is not a swallowed error: the request succeeded, and "this server
/// is broken, here is why" is the answer it asked for. The same reason appears in
/// `connections` as a `failed` row, so the next `bough mcp` says it too.
pub fn connect_mcp_server() -> Handler {
    handler(|req, ctx, params| async move {
        let name = param(&params, "name");
        let session_id = scope_of(&req, &ctx)?;
        let server = require_server(&name, &config())?;
        // ONLY A STDIO SERVER NEEDS A CONVERSATION, and only because it is a
        // subprocess that has to be spawned somewhere — the session's checkout. A
        // remote server is a URL: its connection is shared by every conversation,
        // and requiring a session to reach it made the panel unusable before the
        // first message was sent.
        if session_id.is_none() && is_stdio(&server) {
            return Err(mcp_error(
                400,
                format!(
                    "\"{name}\" is a local command, so it runs in a conversation's checkout — \
                     open a conversation and try again, or pass ?session=<id>."
                ),
            ));
        }
        let workspace = workspace_of(&ctx, session_id.as_deref());
        let catalogs = mcp_manager()
            .ensure(
                session_id.as_deref().unwrap_or(SHARED_SCOPE),
                std::slice::from_ref(&name),
                &SpawnCtx::new(workspace),
            )
            .await;
        let catalog = catalogs.into_iter().next().unwrap_or_default();
        let tools: Vec<Value> = catalog
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t
                        .description
                        .as_deref()
                        .unwrap_or("")
                        .split('\n')
                        .next()
                        .unwrap_or("")
                        .trim(),
                })
            })
            .collect();
        let mut extra = Map::new();
        extra.insert("server".into(), json!(name));
        extra.insert("connected".into(), json!(catalog.error.is_none()));
        if let Some(error) = &catalog.error {
            extra.insert("error".into(), json!(error));
        }
        extra.insert("tools".into(), json!(tools));
        Ok(json_res(
            &state_with(session_id.as_deref(), Value::Object(extra)),
            200,
        ))
    })
}

/// `POST /mcp/servers/:name/tools/:tool?session=` — call one tool and return its
/// result.
///
/// WHY THIS ROUTE EXISTS. It is the whole of what the `mcp()` host function used
/// to be, moved to where every other MCP verb already lived. A program reaches a
/// tool the same way it reaches everything else on this machine — by running a
/// command — instead of through a bridged name that had to be granted,
/// documented in its own prompt section, and kept in step with both.
///
/// THE GRANT IS ENFORCED HERE, AND IT IS THE SAME CHECK, given the same scope and
/// resolved fresh so a grant revoked between two calls is gone from the second.
/// What moved is the caller, not the rule. It is explicitly not a security
/// boundary: any local process can pass any session id and programs are not
/// sandboxed — the grant check is what stops a MISTAKE, not an attacker with the
/// user's own shell.
pub fn call_mcp_tool() -> Handler {
    handler(|req, ctx, params| async move {
        let name = param(&params, "name");
        let tool = param(&params, "tool");
        let session_id = scope_of(&req, &ctx)?;
        let server = require_server(&name, &config())?;
        let args =
            match body_of(req).await {
                None => json!({}),
                Some(Value::Object(map)) => Value::Object(map),
                Some(_) => return Err(mcp_error(
                    400,
                    "the body must be the tool's arguments as a plain object, or empty for none.",
                )),
            };
        require_granted(
            &GrantCtx::for_session(session_id.clone().unwrap_or_default()),
            &name,
            &config(),
        )?;
        if session_id.is_none() && is_stdio(&server) {
            return Err(mcp_error(
                400,
                format!(
                    "\"{name}\" is a local command, so it runs in a conversation's checkout — \
                     pass ?session=<id>."
                ),
            ));
        }
        let workspace = workspace_of(&ctx, session_id.as_deref());
        let result = mcp_manager()
            .call(
                session_id.as_deref().unwrap_or(SHARED_SCOPE),
                &name,
                &tool,
                args,
                &SpawnCtx::new(workspace),
            )
            .await?;
        Ok(json_res(
            &json!({ "server": name, "tool": tool, "result": result }),
            200,
        ))
    })
}

/// `POST /mcp/servers/:name/restart?session=` — drop the child and start a new
/// one.
pub fn restart_mcp_server() -> Handler {
    handler(|req, ctx, params| async move {
        let name = param(&params, "name");
        let Some(session_id) = scope_of(&req, &ctx)? else {
            return Err(mcp_error(
                400,
                "restarting is per-session — pass ?session=<id>.",
            ));
        };
        require_server(&name, &config())?;
        let workspace = workspace_of(&ctx, Some(&session_id));
        let restarted = mcp_manager()
            .restart(&session_id, &name, Some(&SpawnCtx::new(workspace)))
            .await?;
        Ok(json_res(
            &state_with(
                Some(&session_id),
                json!({ "restarted": serde_json::to_value(&restarted).unwrap_or(Value::Null) }),
            ),
            200,
        ))
    })
}

/// `POST /mcp/servers/:name/enable` and `/disable` — the grant itself.
///
/// `sessionId: ""` is the GLOBAL scope, which is why the body can require the
/// field: `""` means "every session" rather than "unspecified". `ttl` resolves to
/// an ABSOLUTE expiry, so a grant meant to last two hours cannot be silently
/// extended by a later rewrite of the file.
///
/// Disabling DROPS the connection: revoking a grant while its subprocess keeps
/// running would leave the thing the human just switched off alive and holding
/// their credentials. Revoking globally drops it for every session, for the same
/// reason. A grant takes effect on the NEXT call, not the next turn — and an
/// in-flight subagent keeps the snapshot it was spawned with.
pub fn set_mcp_activation(on: bool) -> Handler {
    handler(move |req, ctx, params| async move {
        let name = param(&params, "name");
        let Ok(body) = parse_body::<McpActivationBody>(req, Some(json!({}))).await else {
            return Err(mcp_error(
                400,
                "the body must be {\"sessionId\": \"<id>\"} — use \"\" for the global scope, \
                 meaning every session — with an optional {\"ttl\": \"2h\"}.",
            ));
        };
        let session_id = body.session_id;
        if !session_id.is_empty() && ctx.db.lock().unwrap().get_session(&session_id)?.is_none() {
            return Err(BoughError::not_found(format!(
                "no session {session_id} — GET /sessions lists them."
            )));
        }
        if on {
            require_server(&name, &config())?;
        }
        let expires = match (
            on,
            body.ttl.as_deref().map(str::trim).filter(|t| !t.is_empty()),
        ) {
            (true, Some(ttl)) => Some(ttl_to_expires(ttl, now_ms())?),
            _ => None,
        };
        // A GLOBAL revoke means every scope, not just the global row: grants made
        // one conversation at a time (which is how they were all made before the
        // panel's ⏎ became global) would otherwise survive a revoke that said it
        // covered them.
        if !on && session_id.is_empty() {
            revoke_everywhere(&name, &config())?;
        } else {
            set_activation(
                Some(session_id.as_str()).filter(|s| !s.is_empty()),
                &name,
                on,
                expires.as_deref(),
                &config(),
            )?;
        }
        if !on {
            if session_id.is_empty() {
                mcp_manager().drop_server(&name).await;
            } else {
                mcp_manager().drop_conn(&session_id, &name).await;
            }
        } else if session_id.is_empty() {
            // GRANTED GLOBALLY: connect it now, in the service's own scope,
            // rather than leaving it to whichever conversation happens to want it
            // first. Awaited, so the response the panel renders already reflects
            // the attempt — "granted" and "not connected" on the same row is the
            // state this removes.
            let _ = bough_core::mcp::service::reconcile_mcp(&Default::default()).await;
        }
        let scope = if session_id.is_empty() {
            json!("global")
        } else {
            json!({ "sessionId": session_id })
        };
        let for_scope = if session_id.is_empty() {
            None
        } else {
            Some(session_id.as_str())
        };
        Ok(json_res(
            &state_with(for_scope, json!({ "scope": scope })),
            200,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use async_trait::async_trait;
    use bough_core::mcp::client::{McpCallResult, McpConnection, McpContentBlock, McpToolInfo};
    use bough_core::mcp::manager::{
        set_mcp_manager, ConnectSpec, Connector, McpManager, McpManagerOptions,
    };
    use bough_core::schema::parts::{Session, SessionKind};
    use futures::FutureExt;
    use serde_json::json as j;
    use std::sync::{Arc, Mutex};

    /// The process manager is ONE object and these tests swap it, so they run one
    /// at a time. (Production has exactly one too — that is the point of it.)
    /// The crate-wide one, not a private lock: `boot` swaps the same global
    /// manager these tests do, and a lock only this module took left that race
    /// live — which is what it was failing on.
    fn manager_lock() -> &'static tokio::sync::Mutex<()> {
        &crate::MCP_MANAGER_LOCK
    }

    struct FakeConnection {
        name: String,
        alive: Mutex<bool>,
    }

    #[async_trait]
    impl McpConnection for FakeConnection {
        fn name(&self) -> &str {
            &self.name
        }
        async fn list_tools(&self) -> Result<Vec<McpToolInfo>, BoughError> {
            Ok(vec![McpToolInfo {
                name: "echo".into(),
                description: Some("the echo tool\nand a second line the panel drops".into()),
                ..Default::default()
            }])
        }
        async fn call_tool(&self, tool: &str, args: Value) -> Result<McpCallResult, BoughError> {
            Ok(McpCallResult {
                content: Some(vec![McpContentBlock {
                    r#type: "text".into(),
                    text: Some(format!("{tool}:{args}")),
                }]),
                ..Default::default()
            })
        }
        async fn close(&self) {
            *self.alive.lock().unwrap() = false;
        }
        fn alive(&self) -> bool {
            *self.alive.lock().unwrap()
        }
        fn stderr_tail(&self) -> String {
            String::new()
        }
    }

    fn fake_connector() -> Connector {
        Arc::new(|spec: ConnectSpec| {
            async move {
                Ok(Arc::new(FakeConnection {
                    name: spec.name.clone(),
                    alive: Mutex::new(true),
                }) as Arc<dyn McpConnection>)
            }
            .boxed()
        })
    }

    /// A hermetic registry file, installed on the process manager — which is
    /// where the handlers read it from, so no test has to touch `BOUGH_HOME`.
    fn install_manager(connect: Option<Connector>) -> Arc<McpManager> {
        let dir = std::env::temp_dir().join(format!("bough-mcp-routes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        set_mcp_manager(Arc::new(McpManager::new(McpManagerOptions {
            config: Some(McpConfigOptions::with_file(dir.join("mcp.json"))),
            connect,
            ..Default::default()
        })))
    }

    fn seed_session(ctx: &AppCtx) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        ctx.db
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
                workspace: Some(std::env::temp_dir().display().to_string()),
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
            .unwrap();
        id
    }

    #[tokio::test]
    async fn the_api_registers_grants_connects_and_revokes_and_every_reply_is_the_state() {
        let _guard = manager_lock().lock().await;
        let previous = install_manager(Some(fake_connector()));
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let session = seed_session(&fx.ctx);
        let q = format!("?session={session}");

        let empty = testutil::body_json(call.call(testutil::get("/mcp/servers")).await).await;
        assert_eq!(
            empty,
            j!({"registry": {"servers": {}}, "auth": {}, "active": [], "connections": []})
        );

        // Register one. The reply is the whole state, not an ack.
        let put = call
            .call(testutil::req(
                "PUT",
                "/mcp/servers/echo",
                Some(j!({"command": "/bin/echo", "args": ["hi"], "cwd": "/tmp"})),
            ))
            .await;
        assert_eq!(put.status(), 200);
        let registered = testutil::body_json(put).await;
        assert_eq!(
            registered["registry"]["servers"]["echo"]["command"],
            j!("/bin/echo")
        );
        // `cwd` survived: the entry is validated by the schema the FILE is
        // written with, not by a narrower wire subset that would drop it.
        assert_eq!(registered["registry"]["servers"]["echo"]["cwd"], j!("/tmp"));
        assert_eq!(registered["active"], j!([]), "registering granted nothing");

        // An invalid entry is a 400 whose message names the fix.
        let bad = call
            .call(testutil::req("PUT", "/mcp/servers/echo", Some(j!({}))))
            .await;
        assert_eq!(bad.status(), 400);
        let message = testutil::body_json(bad).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("exactly one of `command`"), "{message}");

        // Enabling is the grant, and it is scoped to the session that asked.
        let enabled = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/mcp/servers/echo/enable",
                Some(j!({"sessionId": session, "ttl": "2h"})),
            ))
            .await,
        )
        .await;
        assert_eq!(enabled["active"], j!(["echo"]));
        assert_eq!(enabled["scope"], j!({"sessionId": session}));
        let other = testutil::body_json(call.call(testutil::get("/mcp/servers")).await).await;
        assert_eq!(other["active"], j!([]), "another scope sees nothing");

        // Connect proves the command works and reports the catalog.
        let connected = testutil::body_json(
            call.call(testutil::req(
                "POST",
                &format!("/mcp/servers/echo/connect{q}"),
                None,
            ))
            .await,
        )
        .await;
        assert_eq!(connected["connected"], j!(true));
        assert_eq!(
            connected["tools"],
            j!([{"name": "echo", "description": "the echo tool"}])
        );
        assert_eq!(connected["connections"][0]["state"], j!("connected"));

        // Revoking takes the connection with it — a switched-off server must not
        // keep running with the user's credentials.
        let disabled = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/mcp/servers/echo/disable",
                Some(j!({"sessionId": session})),
            ))
            .await,
        )
        .await;
        assert_eq!(disabled["active"], j!([]));
        assert_eq!(disabled["connections"], j!([]));

        // Removing the entry is a 404 the second time, and says so plainly.
        assert_eq!(
            call.call(testutil::req("DELETE", "/mcp/servers/echo", None))
                .await
                .status(),
            200
        );
        let gone = call
            .call(testutil::req("DELETE", "/mcp/servers/echo", None))
            .await;
        assert_eq!(gone.status(), 404);
        assert!(testutil::body_json(gone).await["error"]
            .as_str()
            .unwrap()
            .contains("nothing to remove"));

        // A connect for a server nobody registered names the alternatives.
        let missing = call
            .call(testutil::req(
                "POST",
                &format!("/mcp/servers/echo/connect{q}"),
                None,
            ))
            .await;
        assert_eq!(missing.status(), 404);
        assert!(testutil::body_json(missing).await["error"]
            .as_str()
            .unwrap()
            .contains("No servers are registered yet"));

        set_mcp_manager(previous).drop_all().await;
    }

    #[tokio::test]
    async fn the_route_calls_a_tool_and_the_grant_is_enforced_there() {
        let _guard = manager_lock().lock().await;
        let previous = install_manager(Some(fake_connector()));
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let session = seed_session(&fx.ctx);
        let q = format!("?session={session}");

        call.call(testutil::req(
            "PUT",
            "/mcp/servers/echo",
            Some(j!({"command": "/bin/echo", "args": []})),
        ))
        .await;

        // UNGRANTED IS A REFUSAL, and it names what would fix it. Registering is
        // not granting, and the CLI relays this sentence verbatim.
        let refused = call
            .call(testutil::req(
                "POST",
                &format!("/mcp/servers/echo/tools/echo{q}"),
                Some(j!({"text": "x"})),
            ))
            .await;
        assert_eq!(refused.status(), 403);
        let message = testutil::body_json(refused).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("registered but not granted"), "{message}");
        assert!(
            message.contains("a program cannot grant itself one"),
            "{message}"
        );

        call.call(testutil::req(
            "POST",
            "/mcp/servers/echo/enable",
            Some(j!({"sessionId": session})),
        ))
        .await;
        let ok = call
            .call(testutil::req(
                "POST",
                &format!("/mcp/servers/echo/tools/echo{q}"),
                Some(j!({"text": "through the route"})),
            ))
            .await;
        assert_eq!(ok.status(), 200);
        let body = testutil::body_json(ok).await;
        assert_eq!(body["server"], j!("echo"));
        assert_eq!(body["tool"], j!("echo"));
        // The tool's own return value, verbatim, not a wrapper the caller has to
        // dig through — the CLI prints exactly this and a program parses it.
        assert_eq!(body["result"], j!("echo:{\"text\":\"through the route\"}"));

        // A body that is not a plain object is refused before anything runs.
        let bad = call
            .call(testutil::req(
                "POST",
                &format!("/mcp/servers/echo/tools/echo{q}"),
                Some(j!([1])),
            ))
            .await;
        assert_eq!(bad.status(), 400);

        set_mcp_manager(previous).drop_all().await;
    }

    #[tokio::test]
    async fn a_stdio_server_needs_a_conversation_and_a_bad_session_is_a_404() {
        let _guard = manager_lock().lock().await;
        let previous = install_manager(Some(fake_connector()));
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        call.call(testutil::req(
            "PUT",
            "/mcp/servers/echo",
            Some(j!({"command": "/bin/echo", "args": []})),
        ))
        .await;

        let no_session = call
            .call(testutil::req("POST", "/mcp/servers/echo/connect", None))
            .await;
        assert_eq!(no_session.status(), 400);
        assert!(testutil::body_json(no_session).await["error"]
            .as_str()
            .unwrap()
            .contains("runs in a conversation's checkout"));

        let typo = call
            .call(testutil::req(
                "POST",
                "/mcp/servers/echo/connect?session=nope",
                None,
            ))
            .await;
        assert_eq!(typo.status(), 404);

        // Restarting is per-session and says so.
        let restart = call
            .call(testutil::req("POST", "/mcp/servers/echo/restart", None))
            .await;
        assert_eq!(restart.status(), 400);

        set_mcp_manager(previous).drop_all().await;
    }

    #[tokio::test]
    async fn a_server_that_fails_to_start_answers_200_with_connected_false() {
        // The request succeeded, and "this server is broken, here is why" is the
        // answer it asked for.
        let _guard = manager_lock().lock().await;
        // No injected connector: the REAL one, against a command that is not there.
        let previous = install_manager(None);
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let session = seed_session(&fx.ctx);
        call.call(testutil::req(
            "PUT",
            "/mcp/servers/broken",
            Some(j!({"command": "/nonexistent/mcp-server-binary"})),
        ))
        .await;
        let res = call
            .call(testutil::req(
                "POST",
                &format!("/mcp/servers/broken/connect?session={session}"),
                None,
            ))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["connected"], j!(false));
        assert!(body["error"].as_str().unwrap().contains("failed to start"));
        // …and the same reason appears in `connections` as a `failed` row.
        assert_eq!(body["connections"][0]["state"], j!("failed"));

        set_mcp_manager(previous).drop_all().await;
    }

    #[tokio::test]
    async fn a_global_grant_is_every_sessions_and_a_global_revoke_reaches_every_scope() {
        let _guard = manager_lock().lock().await;
        let previous = install_manager(Some(fake_connector()));
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let session = seed_session(&fx.ctx);
        call.call(testutil::req(
            "PUT",
            "/mcp/servers/echo",
            Some(j!({"command": "/bin/echo", "args": []})),
        ))
        .await;
        // Granted one conversation at a time…
        call.call(testutil::req(
            "POST",
            "/mcp/servers/echo/enable",
            Some(j!({"sessionId": session})),
        ))
        .await;
        // …and revoked globally: the session's own row goes too, or the screen
        // would say "off everywhere" while the next turn could still call it.
        let revoked = testutil::body_json(
            call.call(testutil::req(
                "POST",
                "/mcp/servers/echo/disable",
                Some(j!({"sessionId": ""})),
            ))
            .await,
        )
        .await;
        assert_eq!(revoked["scope"], j!("global"));
        let scoped = testutil::body_json(
            call.call(testutil::get(&format!("/mcp/servers?session={session}")))
                .await,
        )
        .await;
        assert_eq!(scoped["active"], j!([]));

        set_mcp_manager(previous).drop_all().await;
    }
}
