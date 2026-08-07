//! One shape for "what MCP state is true right now" (port of the state half of
//! `src/mcp/status.ts`; the HTTP handlers it also held live in
//! `bough-server::mcp_routes`, because this crate never constructs a response).
//!
//! THE INVARIANT THIS HOLDS: **there is exactly one builder, and it never serves
//! a cached answer.** `bough mcp` (which is what a program uses — there is no MCP
//! host function), `GET /mcp/servers` in the TUI's MCP tab, and the response to
//! every mutation all come from [`mcp_status_for`] — so the model and the human
//! cannot be looking at different MCP states, and neither can be looking at a
//! stale one. Every call re-reads the registry file, re-resolves the grant,
//! re-reads the credential store and re-reads the live connections. The prompt
//! tells the model to answer every MCP question from a FRESH call precisely
//! because grants and connections change between turns — a memo here would make
//! that instruction a lie, and the model's confident answer would be wrong in the
//! one way it cannot detect.
//!
//! THE FOUR KEYS ARE FIXED. `{registry, auth, active, connections}` is what
//! `prompt/mcp-status.md` promises the model it will get. Renaming or dropping
//! one is a prompt change, not a refactor.
//!
//! `active` IS THE EFFECTIVE GRANT, NOT THE FILE. For an ordinary session it is
//! that session's activations plus the global ones, expired entries already
//! filtered. For a subagent it is the grant it INHERITED from its spawner, which
//! is the only true answer — a subagent has no activations of its own, and
//! reporting the file would tell it that it may call nothing while `bough mcp
//! call` happily works.
//!
//! SECRETS NEVER APPEAR HERE. `registry` carries `env` values verbatim, which are
//! `${VAR}` references, never expanded — this response is rendered in a UI and
//! read by the model. `auth` is one boolean per remote server and never a token.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::mcp::config::{load_registry, McpConfigOptions, Registry};
use crate::mcp::manager::{
    mcp_manager, resolve_grant, ConnStatus, GrantCtx, McpConnState, McpGrant, McpManager,
};
use crate::mcp::oauth::{has_tokens, TokenStoreOptions};
use crate::prompt::assemble::PromptMcpServer;

/// Whether a remote server has stored credentials — never a token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthFlag {
    pub authorized: bool,
}

/// Exactly the four keys `prompt/mcp-status.md` documents.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpStatus {
    pub registry: Registry,
    /// Remote (`url`) servers only: whether stored credentials exist.
    pub auth: BTreeMap<String, AuthFlag>,
    /// The servers this scope may actually call, right now.
    pub active: Vec<String>,
    pub connections: Vec<ConnStatus>,
}

/// Whether a remote server has stored credentials.
///
/// Injected rather than called directly so a test asserts the whole status shape
/// without writing a credential file, and so this module never grows a second
/// opinion about where tokens live — the default is `oauth.rs`'s own accessor.
pub type AuthLookup = Arc<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Clone, Default)]
pub struct McpStatusOptions {
    /// Where the registry, the grants and `${VAR}` come from.
    pub config: McpConfigOptions,
    /// The scope whose grant and connections are reported. Absent = global scope.
    pub session_id: Option<String>,
    /// An inherited grant (a subagent's). Absent = read this session's
    /// activations.
    pub grant: Option<Vec<String>>,
    /// Absent = the process manager.
    pub manager: Option<Arc<McpManager>>,
    /// Absent = `oauth.rs`'s `has_tokens` against the real token directory.
    pub auth: Option<AuthLookup>,
}

/// Build the whole MCP state for one scope. Read-only: it never connects, never
/// spawns, and never fails — status must be answerable while everything is
/// broken, because that is exactly when it is asked.
pub fn mcp_status_for(opts: &McpStatusOptions) -> McpStatus {
    let registry = load_registry(&opts.config);
    let manager = opts.manager.clone().unwrap_or_else(mcp_manager);
    let grant_ctx = GrantCtx {
        session_id: opts.session_id.clone().unwrap_or_default(),
        grant: opts.grant.clone().map(McpGrant::Inherited),
    };
    let auth: BTreeMap<String, AuthFlag> = registry
        .servers
        .iter()
        .filter(|(_, cfg)| cfg.url.is_some())
        .map(|(name, _)| {
            let authorized = match &opts.auth {
                Some(lookup) => lookup(name),
                None => has_tokens(name, &TokenStoreOptions::default()),
            };
            (name.clone(), AuthFlag { authorized })
        })
        .collect();
    McpStatus {
        registry,
        auth,
        // `resolve_grant` reads the file when nothing was inherited, and an empty
        // `sessionId` is the global scope — so a status asked for no session
        // reports exactly the grants every session has.
        active: resolve_grant(&grant_ctx, &opts.config),
        connections: manager.statuses(opts.session_id.as_deref()),
    }
}

/// The turn-start catalog: what `prompt/assemble.rs` renders as the MCP tools
/// section, derived from the same status document the panel and `bough mcp` read.
///
/// WHY IT IS BUILT FROM `active` RATHER THAN FROM THE CONNECTIONS. `active` is
/// the grant — precisely the set `bough mcp call` will let this session reach. A
/// catalog built from live connections instead would list nothing on the first
/// turn (nothing has connected yet) and would silently omit a granted server
/// whose child had exited, which is exactly the "the model never knew it could"
/// failure this section exists to stop.
///
/// A granted server with no live connection is therefore NAMED, with a note
/// saying how to see its tools, rather than dropped or rendered as an empty
/// catalog. Pure, and it never connects: the prompt is assembled on the turn's
/// critical path.
pub fn prompt_mcp_servers(status: &McpStatus) -> Vec<PromptMcpServer> {
    status
        .active
        .iter()
        .map(|name| {
            // The connection for THIS scope; a remote server shared with other
            // sessions reports under the shared scope, which is still the right
            // catalog for it.
            let conn = status.connections.iter().find(|c| &c.server == name);
            match conn {
                Some(c) if c.state == McpConnState::Failed => PromptMcpServer {
                    name: name.clone(),
                    error: Some(
                        c.error
                            .clone()
                            .unwrap_or_else(|| "failed to connect".to_string()),
                    ),
                    ..Default::default()
                },
                Some(c) if !c.tools.is_empty() => PromptMcpServer {
                    name: name.clone(),
                    tools: c
                        .tools
                        .iter()
                        .map(|tool| crate::prompt::assemble::PromptMcpTool {
                            name: tool.clone(),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                },
                _ => PromptMcpServer {
                    name: name.clone(),
                    note: Some(format!(
                        "granted, not connected yet — the first `bough mcp call` connects it, \
                         and `bough mcp test {name}` lists its tools without calling one"
                    )),
                    ..Default::default()
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::{save_registry, set_activation};
    use crate::mcp::manager::{McpManager, McpManagerOptions};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn tmp_registry() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-mcp-status-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("mcp.json")
    }

    fn options(file: &Path, session_id: Option<&str>) -> McpStatusOptions {
        McpStatusOptions {
            config: McpConfigOptions::with_file(file),
            session_id: session_id.map(|s| s.to_string()),
            manager: Some(Arc::new(McpManager::new(McpManagerOptions {
                config: Some(McpConfigOptions::with_file(file)),
                ..Default::default()
            }))),
            // No token files are written by these tests, and the real store must
            // not be read by one.
            auth: Some(Arc::new(|_| false)),
            ..Default::default()
        }
    }

    #[test]
    fn the_document_carries_exactly_four_keys_and_never_a_secret() {
        let file = tmp_registry();
        save_registry(
            &json!({"servers": {
                "gh": {"command": "gh-mcp", "env": {"TOKEN": "${GH_TOKEN}"}},
                "linear": {"url": "https://mcp.linear.app/mcp"}
            }}),
            &McpConfigOptions::with_file(file.clone()),
        )
        .unwrap();
        let status = mcp_status_for(&options(&file, Some("s1")));
        let body = serde_json::to_value(&status).unwrap();
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort();
        assert_eq!(keys, vec!["active", "auth", "connections", "registry"]);
        // Remote servers, and only remote servers, carry an auth flag.
        assert_eq!(status.auth.keys().collect::<Vec<_>>(), vec!["linear"]);
        assert!(!status.auth["linear"].authorized);
        // The `${VAR}` reference is served verbatim — never the value.
        assert_eq!(
            body["registry"]["servers"]["gh"]["env"]["TOKEN"],
            json!("${GH_TOKEN}"),
            "the reference is what is served"
        );
        assert!(
            !body.to_string().contains("token\":"),
            "no token ever appears: {body}"
        );
    }

    #[test]
    fn active_is_the_effective_grant_and_a_subagents_is_the_one_it_inherited() {
        let file = tmp_registry();
        save_registry(
            &json!({"servers": {"echo": {"command": "deno"}, "exa": {"command": "npx"}}}),
            &McpConfigOptions::with_file(file.clone()),
        )
        .unwrap();
        set_activation(
            Some("s1"),
            "echo",
            true,
            None,
            &McpConfigOptions::with_file(file.clone()),
        )
        .unwrap();

        assert_eq!(
            mcp_status_for(&options(&file, Some("s1"))).active,
            vec!["echo"]
        );
        // A session with no grant of its own sees the global scope only (empty).
        assert!(mcp_status_for(&options(&file, Some("s2")))
            .active
            .is_empty());
        // No session at all = the global scope: exactly what every session has.
        assert!(mcp_status_for(&options(&file, None)).active.is_empty());

        // A subagent reports what it inherited, not what the file says about it.
        let inherited = McpStatusOptions {
            grant: Some(vec!["exa".to_string()]),
            ..options(&file, Some("child"))
        };
        assert_eq!(mcp_status_for(&inherited).active, vec!["exa"]);
    }

    #[test]
    fn the_turns_catalog_is_the_grant_not_the_live_connections() {
        // Built from `active` on purpose. A catalog derived from connections
        // would list nothing on the first turn — nothing has connected yet — so
        // the model would never learn the server exists.
        let status = McpStatus {
            registry: Registry::default(),
            auth: BTreeMap::new(),
            active: vec!["files".into(), "notion".into(), "broken".into()],
            connections: vec![
                ConnStatus {
                    server: "files".into(),
                    session_id: "s1".into(),
                    state: McpConnState::Connected,
                    alive: true,
                    tool_count: 2,
                    tools: vec!["read_file".into(), "write_file".into()],
                    last_used: 0,
                    error: None,
                    stderr_tail: None,
                },
                ConnStatus {
                    server: "broken".into(),
                    session_id: "s1".into(),
                    state: McpConnState::Failed,
                    alive: false,
                    tool_count: 0,
                    tools: vec![],
                    last_used: 0,
                    error: Some("exited before handshake".into()),
                    stderr_tail: None,
                },
            ],
        };
        let catalog = prompt_mcp_servers(&status);
        assert_eq!(
            catalog.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["files", "notion", "broken"]
        );
        // Connected: the live tool names, which is what a call can name.
        assert_eq!(
            catalog[0]
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["read_file", "write_file"]
        );
        // Granted but never connected: NAMED, with the way to see its tools.
        assert!(catalog[1].tools.is_empty());
        let note = catalog[1].note.as_deref().unwrap_or("");
        assert!(note.contains("not connected yet"), "{note}");
        assert!(note.contains("bough mcp test notion"), "{note}");
        // Broken: its own error, so the model stops rather than inventing a
        // workaround.
        assert_eq!(catalog[2].error.as_deref(), Some("exited before handshake"));
        // A server nobody granted is not in the catalog at all — the catalog IS
        // the grant.
        assert!(!catalog.iter().any(|c| c.name == "ungranted"));
    }

    #[test]
    fn a_corrupt_registry_still_answers_with_the_four_keys() {
        let file = tmp_registry();
        std::fs::write(&file, "{ not json").unwrap();
        let status = mcp_status_for(&options(&file, Some("s1")));
        assert!(status.registry.servers.is_empty());
        assert!(status.active.is_empty());
        assert!(status.connections.is_empty());
        assert!(status.auth.is_empty());
    }
}
