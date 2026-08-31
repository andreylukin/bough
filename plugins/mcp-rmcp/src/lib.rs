//! Invariant: ONE CHILD ENTRY PER SERVER. The parent row reads `servers:` and mounts one
//! `mcp-server` child per enabled row through `ctx.mount`; children are effects of the parent, so
//! unloading the parent cascades and disabling one server's row removes exactly that server and
//! its tools (§0.3, SWAP).
//!
//! rmcp 3.x wants reqwest 0.13 and `bough-llm` holds 0.12. The dual-version arrangement recorded in
//! Phase 0 STANDS, bridged through `OAuthHttpClient`, and both are pinned to a minor (§13).

pub mod invariant;
pub mod keychain;
pub mod server;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};

/// The catalog name of the parent row.
pub const PLUGIN_NAME: &str = "mcp-rmcp";
/// The catalog name of the per-server CHILD row.
pub const SERVER_PLUGIN_NAME: &str = "mcp-server";

/// The parent row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpRmcpConfig {
    pub servers: Vec<ServerRow>,
    pub connect_timeout_ms: u64,
    pub call_timeout_ms: u64,
}

/// One server, as the bundle spells it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerRow {
    pub name: String,
    pub transport: Transport,
    #[serde(default)]
    pub disabled: bool,
}

/// How to reach a server.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// A row name must be a single, filename-safe token: it becomes half of a child ENTRY ID and half
/// of every tool name `tool-mcp` derives, and a dot there would split the entry id.
pub fn validate_row_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty() {
        return Err(ConfigError::Rejected {
            detail: "a server row needs a non-empty `name`".into(),
        });
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConfigError::Rejected {
            detail: format!("server name `{name}` must be ascii alphanumeric, `-` or `_`"),
        });
    }
    Ok(())
}

/// A transport must be reachable in principle: a non-empty command, or a parseable http(s) url.
/// A header or stdio-env value SHAPED like a `${keychain:…}` reference must parse as one, so a
/// typo'd reference is a compose-time rejection and not a mystery 401 at connect time.
pub fn validate_transport(name: &str, t: &Transport) -> Result<(), ConfigError> {
    let check_ref = |place: &str, value: &str| -> Result<(), ConfigError> {
        if keychain::has_keychain_ref(value) {
            keychain::split_refs(value).map_err(|why| ConfigError::Rejected {
                detail: format!("server `{name}` {place} carries a keychain reference: {why}"),
            })?;
        }
        Ok(())
    };
    match t {
        Transport::Stdio { command, env, .. } => {
            if command.trim().is_empty() {
                return Err(ConfigError::Rejected {
                    detail: format!("server `{name}` has an empty stdio `command`"),
                });
            }
            for (k, v) in env {
                check_ref(&format!("env `{k}`"), v)?;
            }
        }
        Transport::Http { url, headers } => {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ConfigError::Rejected {
                    detail: format!("server `{name}` has a url that is not http(s): `{url}`"),
                });
            }
            for (k, v) in headers {
                check_ref(&format!("header `{k}`"), v)?;
            }
        }
    }
    Ok(())
}

/// PURE: the child entry one enabled [`ServerRow`] mounts as.
pub fn child_entry(
    parent: &bough_kernel::EntryId,
    row: &ServerRow,
    connect_timeout_ms: u64,
    call_timeout_ms: u64,
) -> Result<bough_kernel::Entry, PluginError> {
    let config = serde_yaml::to_value(server::McpServerConfig {
        name: row.name.clone(),
        transport: row.transport.clone(),
        connect_timeout_ms,
        call_timeout_ms,
    })
    .map_err(|e| PluginError::new(parent.clone(), anyhow::Error::new(e)))?;
    Ok(bough_kernel::Entry {
        id: bough_kernel::EntryId::new(format!("{}.{}", parent.as_str(), row.name)),
        plugin: Some(SERVER_PLUGIN_NAME.to_string()),
        config,
        disabled: Default::default(),
        isolate: Default::default(),
        inject: Default::default(),
        group: Vec::new(),
        include: None,
        critical: true,
    })
}

/// The parent row.
pub struct McpRmcpPlugin;

#[async_trait::async_trait]
impl Plugin for McpRmcpPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = McpRmcpConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["mcp"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.connect_timeout_ms == 0 || cfg.call_timeout_ms == 0 {
            return Err(ConfigError::Rejected {
                detail: "timeouts must be greater than zero".into(),
            });
        }
        let mut seen: Vec<&str> = Vec::new();
        for row in &cfg.servers {
            validate_row_name(&row.name)?;
            validate_transport(&row.name, &row.transport)?;
            if seen.contains(&row.name.as_str()) {
                return Err(ConfigError::Rejected {
                    detail: format!("two server rows are both named `{}`", row.name),
                });
            }
            seen.push(&row.name);
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        for row in cfg.servers.iter().filter(|r| !r.disabled) {
            let entry = child_entry(
                ctx.entry_id(),
                row,
                cfg.connect_timeout_ms,
                cfg.call_timeout_ms,
            )?;
            ctx.mount(entry)
                .await
                .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(McpRmcpPlugin);
bough_kernel::register_plugin!(server::McpServerPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio_row(name: &str) -> ServerRow {
        ServerRow {
            name: name.to_string(),
            transport: Transport::Stdio {
                command: "python3".into(),
                args: vec!["server.py".into()],
                env: BTreeMap::new(),
                cwd: None,
            },
            disabled: false,
        }
    }

    #[test]
    fn one_enabled_row_becomes_one_child_entry_named_after_the_parent() {
        let parent = bough_kernel::EntryId::new("mcp.rmcp");
        let entry = child_entry(&parent, &stdio_row("fixture"), 5_000, 9_000).unwrap();
        assert_eq!(entry.id.as_str(), "mcp.rmcp.fixture");
        assert_eq!(entry.plugin.as_deref(), Some(SERVER_PLUGIN_NAME));
        let cfg: server::McpServerConfig = serde_yaml::from_value(entry.config).unwrap();
        assert_eq!(cfg.name, "fixture");
        assert_eq!(cfg.connect_timeout_ms, 5_000);
        assert_eq!(cfg.call_timeout_ms, 9_000);
    }

    #[test]
    fn validate_refuses_duplicate_names_zero_timeouts_and_unreachable_transports() {
        let two = McpRmcpConfig {
            servers: vec![stdio_row("fixture"), stdio_row("fixture")],
            connect_timeout_ms: 1,
            call_timeout_ms: 1,
        };
        assert!(McpRmcpPlugin::validate(&two)
            .unwrap_err()
            .to_string()
            .contains("both named"));

        let zero = McpRmcpConfig {
            servers: vec![],
            connect_timeout_ms: 0,
            call_timeout_ms: 1,
        };
        assert!(McpRmcpPlugin::validate(&zero)
            .unwrap_err()
            .to_string()
            .contains("greater than zero"));

        let mut bad = stdio_row("fixture");
        bad.transport = Transport::Http {
            url: "ftp://nope".into(),
            headers: BTreeMap::new(),
        };
        let cfg = McpRmcpConfig {
            servers: vec![bad],
            connect_timeout_ms: 1,
            call_timeout_ms: 1,
        };
        assert!(McpRmcpPlugin::validate(&cfg)
            .unwrap_err()
            .to_string()
            .contains("not http(s)"));

        let dotted = ServerRow {
            name: "one.two".into(),
            ..stdio_row("x")
        };
        assert!(validate_row_name(&dotted.name).is_err());
    }

    #[test]
    fn a_header_shaped_like_a_keychain_reference_must_parse_as_one() {
        let mut row = stdio_row("fixture");
        row.transport = Transport::Http {
            url: "https://mcp.example.com/mcp".into(),
            headers: BTreeMap::from([("Authorization".to_string(), "${keychain:}".to_string())]),
        };
        assert!(validate_transport("fixture", &row.transport)
            .unwrap_err()
            .to_string()
            .contains("keychain reference"));

        // A well-formed reference and a plain bearer value both pass.
        row.transport = Transport::Http {
            url: "https://mcp.example.com/mcp".into(),
            headers: BTreeMap::from([(
                "Authorization".to_string(),
                "${keychain:Claude Code-credentials#mcpOAuth.x.accessToken}".to_string(),
            )]),
        };
        assert!(validate_transport("fixture", &row.transport).is_ok());

        let mut env_row = stdio_row("fixture");
        if let Transport::Stdio { env, .. } = &mut env_row.transport {
            env.insert("API_TOKEN".into(), "${keychain:a#b#c}".into());
        }
        assert!(validate_transport("fixture", &env_row.transport)
            .unwrap_err()
            .to_string()
            .contains("keychain reference"));
    }

    #[test]
    fn a_disabled_row_mounts_no_child() {
        let mut row = stdio_row("fixture");
        row.disabled = true;
        let cfg = McpRmcpConfig {
            servers: vec![row],
            connect_timeout_ms: 1,
            call_timeout_ms: 1,
        };
        assert_eq!(cfg.servers.iter().filter(|r| !r.disabled).count(), 0);
    }
}
