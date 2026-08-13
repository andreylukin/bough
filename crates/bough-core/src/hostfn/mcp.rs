//! `mcp.call` / `mcp.list` — reaching an MCP server without a shell in the way.
//!
//! WHY THIS EXISTS, AND WHY IT IS THE ONE MEMORY-ADJACENT SURFACE THAT GOT A
//! BRIDGE. MCP was the only major capability with no code-mode door: the
//! prompt taught `bash("bough mcp call SERVER TOOL '''{\"arg\":\"v\"}'''")`,
//! and the field database says how that went — 1,848 calls, 267 of them
//! failing, and the modal failure is not a broken server but
//!
//!   the arguments were not valid JSON. Pass a plain object matching the
//!   tool's parameters, e.g. '{"query":"x"}'
//!
//! a JSON object that did not survive being written inside a shell word
//! inside a JavaScript string. Nothing about the server, the tool or the
//! arguments was wrong; the quoting was. `mcp.call("notion", "search", {…})`
//! takes an object and serializes it once, on the way out, so that entire
//! failure class stops existing.
//!
//! THIS IS NOT A SECOND OPINION ABOUT MCP STATE. Every question of "does this
//! server exist, is it granted, what does it advertise" is answered by
//! `mcp/manager.rs` and `mcp/status.rs`, exactly as `bough mcp` answers it —
//! this file resolves arguments and hands them over. In particular the grant
//! check is [`require_granted`] and nothing else, so a program cannot reach
//! through this door anything it could not reach through the CLI one.
//!
//! `list` IS LIVE AND CONNECTS. That is the difference between it and the
//! remembered catalog in the prompt (`mcp/catalog.rs`), and the reason it is
//! worth having: when the model genuinely needs to know what a server
//! advertises right now, this answers in-process instead of costing a shell
//! command. The prompt's own catalog is what it should normally be reading.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::errors::BoughError;
use crate::mcp::config::mcp_error;
use crate::mcp::manager::{mcp_manager, require_granted, GrantCtx, McpManager, SpawnCtx};
use crate::types::{HostFn, TurnCtx};

/// What the verbs need from the world. Injected so a test drives a fake
/// manager rather than spawning a real server.
#[derive(Clone, Default)]
pub struct McpDeps {
    /// Absent = the process manager, which is what production uses.
    pub manager: Option<Arc<McpManager>>,
}

/// `mcp.call(server, tool, args)` — invoke one tool.
///
/// `args` is whatever the program passed, already parsed from the wire, so an
/// object arrives as an object. Absent or `null` is an empty object: a tool
/// that takes no parameters should not need `{}` spelled out.
async fn call_verb(
    ctx: &CallCtx,
    args: &Value,
    manager: &Arc<McpManager>,
) -> Result<Value, BoughError> {
    let server = string_field(args, "server")?;
    let tool = string_field(args, "tool")?;
    let tool_args = match args.get("args") {
        None | Some(Value::Null) => json!({}),
        Some(v) => v.clone(),
    };
    if !tool_args.is_object() {
        return Err(mcp_error(
            400,
            format!(
                "mcp.call(\"{server}\", \"{tool}\", …): the arguments must be an object matching \
                 the tool's parameters, e.g. {{query: \"x\"}} — got {}.",
                kind_of(&tool_args)
            ),
        ));
    }
    require_granted(&ctx.grant, &server, &ctx.config)?;
    manager
        .call(&ctx.session_id, &server, &tool, tool_args, &ctx.spawn)
        .await
}

/// `mcp.list(server?)` — the live catalog, connecting on demand.
///
/// With a server: that one's tools. Without: every server granted here, each
/// with its tools or the reason it has none. Failure is DATA in the no-argument
/// shape — one broken server must not deny the program the other three.
async fn list_verb(
    ctx: &CallCtx,
    args: &Value,
    manager: &Arc<McpManager>,
) -> Result<Value, BoughError> {
    let servers: Vec<String> = match args.get("server") {
        Some(Value::String(one)) => {
            require_granted(&ctx.grant, one, &ctx.config)?;
            vec![one.clone()]
        }
        _ => crate::mcp::manager::resolve_grant(&ctx.grant, &ctx.config),
    };
    if servers.is_empty() {
        return Ok(json!([]));
    }
    let catalogs = manager.ensure(&ctx.session_id, &servers, &ctx.spawn).await;
    Ok(Value::Array(
        catalogs
            .into_iter()
            .map(|c| match c.error {
                Some(error) => json!({"server": c.name, "tools": [], "error": error}),
                // Names, matching what the prompt's catalog renders: the
                // question `list` answers is "what may I call", and a wall of
                // schemas is not that answer.
                None => {
                    let names: Vec<&str> = c.tools.iter().map(|t| t.name.as_str()).collect();
                    json!({"server": c.name, "tools": names})
                }
            })
            .collect(),
    ))
}

/// Everything a verb resolves against, frozen at bridge time from the turn.
///
/// `config` is read from the MANAGER rather than from the process global, so
/// the registry a grant is checked against is always the one the connection
/// will be made through — the two cannot drift, and a test needs no global.
#[derive(Clone)]
struct CallCtx {
    session_id: String,
    spawn: SpawnCtx,
    grant: GrantCtx,
    config: crate::mcp::config::McpConfigOptions,
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// A required string field, with the call shape in the error — the message is
/// the documentation the model reads at the moment it needs it.
fn string_field(args: &Value, field: &str) -> Result<String, BoughError> {
    match args.get(field) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
        _ => Err(mcp_error(
            400,
            format!(
                "mcp.call needs a server and a tool: \
                 await mcp.call(\"notion\", \"search\", {{query: \"x\"}}) — `{field}` was missing."
            ),
        )),
    }
}

/// Bridge `mcp` into one turn. The grant is read from the turn, so a subagent
/// gets the snapshot it inherited and an ordinary session gets a LIVE read
/// that sees a revocation on the very next call.
pub fn create_mcp_host_fn(ctx: &TurnCtx, deps: McpDeps) -> HostFn {
    let session_id = ctx.session_id.clone();
    let spawn = SpawnCtx::new(ctx.workspace.clone());
    let grant = GrantCtx::from_turn(ctx);
    let manager = deps.manager.clone();
    Arc::new(move |args: Vec<String>| {
        let manager = manager.clone().unwrap_or_else(mcp_manager);
        let call_ctx = CallCtx {
            session_id: session_id.clone(),
            spawn: spawn.clone(),
            grant: grant.clone(),
            config: manager.config(),
        };
        let verb = args.first().cloned().unwrap_or_default();
        let args_json = args.get(1).cloned().unwrap_or_default();
        Box::pin(async move {
            let parsed: Value = if args_json.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&args_json).map_err(|_| {
                    mcp_error(400, format!("mcp.{verb}: arguments were not valid JSON"))
                })?
            };
            let result = match verb.as_str() {
                "call" => call_verb(&call_ctx, &parsed, &manager).await?,
                "list" => list_verb(&call_ctx, &parsed, &manager).await?,
                other => {
                    return Err(mcp_error(
                        400,
                        format!("mcp.{other} is not a verb. There are two: call, list."),
                    ))
                }
            };
            serde_json::to_string(&result).map_err(|e| {
                mcp_error(500, format!("mcp.{verb}: could not encode the result: {e}"))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::client::{McpCallResult, McpConnection, McpContentBlock, McpToolInfo};
    use crate::mcp::config::{save_registry, set_activation, McpConfigOptions};
    use crate::mcp::manager::{ConnectSpec, Connector, McpManagerOptions};
    use futures::FutureExt;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn tmp_registry() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-hostfn-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("mcp.json")
    }

    /// A connection that records the arguments each call was given and answers
    /// ok. The point of every test here is what the tool RECEIVED.
    struct Recorder {
        name: String,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    #[async_trait::async_trait]
    impl McpConnection for Recorder {
        fn name(&self) -> &str {
            &self.name
        }
        async fn list_tools(&self) -> Result<Vec<McpToolInfo>, BoughError> {
            Ok(vec![McpToolInfo {
                name: "search".into(),
                ..Default::default()
            }])
        }
        async fn call_tool(&self, _name: &str, args: Value) -> Result<McpCallResult, BoughError> {
            self.seen.lock().unwrap().push(args);
            Ok(McpCallResult {
                content: Some(vec![McpContentBlock {
                    r#type: "text".into(),
                    text: Some("ok".into()),
                }]),
                ..Default::default()
            })
        }
        async fn close(&self) {}
        fn alive(&self) -> bool {
            true
        }
        fn stderr_tail(&self) -> String {
            String::new()
        }
    }

    fn recording_connector(seen: Arc<Mutex<Vec<Value>>>) -> Connector {
        Arc::new(move |spec: ConnectSpec| {
            let seen = seen.clone();
            async move {
                Ok(Arc::new(Recorder {
                    name: spec.name.clone(),
                    seen,
                }) as Arc<dyn McpConnection>)
            }
            .boxed()
        })
    }

    /// A registry with `notion` registered, and granted to `s1` unless told
    /// otherwise. Returns the bridged host fn and what the tool saw.
    fn fixture(granted: bool) -> (HostFn, Arc<Mutex<Vec<Value>>>) {
        let file = tmp_registry();
        let cfg = McpConfigOptions::with_file(&file);
        save_registry(&json!({"servers": {"notion": {"command": "fake"}}}), &cfg).unwrap();
        if granted {
            set_activation(Some("s1"), "notion", true, None, &cfg).unwrap();
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let manager = Arc::new(McpManager::new(McpManagerOptions {
            config: Some(cfg),
            catalog: Some(crate::mcp::catalog::CatalogOptions {
                file: Some(file.with_extension("catalog.json")),
            }),
            connect: Some(recording_connector(seen.clone())),
            ..Default::default()
        }));
        let ctx = crate::agents::testkit::turn_ctx_for(&test_db(), "s1", "t1", 0);
        let host = create_mcp_host_fn(
            &ctx,
            McpDeps {
                manager: Some(manager),
            },
        );
        (host, seen)
    }

    fn test_db() -> crate::types::SharedDb {
        Arc::new(Mutex::new(
            crate::db::SqliteDb::new(":memory:", Default::default()).unwrap(),
        ))
    }

    /// THE POINT OF THE WHOLE FILE: a quote-hostile object reaches the tool
    /// intact, because it was never inside a shell word.
    #[tokio::test]
    async fn call_hands_the_tool_the_object_the_program_passed() {
        let (mcp, seen) = fixture(true);
        let out = mcp(vec![
            "call".into(),
            json!({"server": "notion", "tool": "search", "args": {"query": "it's \"x\" $HOME"}})
                .to_string(),
        ])
        .await
        .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0],
            json!({"query": "it's \"x\" $HOME"})
        );
        assert!(out.contains("ok"), "{out}");
    }

    /// A tool that takes nothing should not need `{}` spelled out.
    #[tokio::test]
    async fn absent_arguments_reach_the_tool_as_an_empty_object() {
        let (mcp, seen) = fixture(true);
        mcp(vec![
            "call".into(),
            json!({"server": "notion", "tool": "search"}).to_string(),
        ])
        .await
        .unwrap();
        assert_eq!(seen.lock().unwrap()[0], json!({}));
    }

    /// Not a way around the grant: the same refusal the CLI door gives.
    #[tokio::test]
    async fn an_ungranted_server_is_refused_and_the_tool_is_never_reached() {
        let (mcp, seen) = fixture(false);
        let err = mcp(vec![
            "call".into(),
            json!({"server": "notion", "tool": "search"}).to_string(),
        ])
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("not granted to this turn"), "{err}");
        assert!(seen.lock().unwrap().is_empty());
    }

    /// Arguments that are not an object are named, with the shape to use.
    #[tokio::test]
    async fn a_non_object_arguments_value_says_what_it_got() {
        let (mcp, _) = fixture(true);
        let err = mcp(vec![
            "call".into(),
            json!({"server": "notion", "tool": "search", "args": "query=x"}).to_string(),
        ])
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("got a string"), "{err}");
    }

    /// A missing field is answered with the call shape, not a type name.
    #[tokio::test]
    async fn a_missing_server_is_told_how_the_call_is_written() {
        let (mcp, _) = fixture(true);
        let err = mcp(vec!["call".into(), json!({"tool": "search"}).to_string()])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("await mcp.call(\"notion\", \"search\""),
            "{err}"
        );
    }

    /// `list` answers live, and it is the door that replaces `bough mcp test`.
    #[tokio::test]
    async fn list_reports_the_granted_servers_and_their_tools() {
        let (mcp, _) = fixture(true);
        let out = mcp(vec!["list".into(), "null".into()]).await.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["server"], "notion");
        assert_eq!(parsed[0]["tools"][0], "search");
    }

    #[tokio::test]
    async fn an_unknown_verb_names_the_two_that_exist() {
        let (mcp, _) = fixture(true);
        let err = mcp(vec!["probe".into(), "null".into()])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("There are two: call, list"), "{err}");
    }
}
