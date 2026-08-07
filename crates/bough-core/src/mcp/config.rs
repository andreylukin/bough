//! The MCP server REGISTRY and the per-session GRANTS over it — one JSON
//! document under `~/.bough`, and every rule about what may be in it (port of
//! `src/mcp/config.ts`).
//!
//! THE INVARIANT THIS HOLDS: **being registered grants nothing.** A registry
//! entry is a definition — this is what `linear` means, this is how you start
//! it. A turn only gets a server's tools when something *granted* it. Two
//! separate questions, two separate reads, which is why [`load_registry`] and
//! [`activations_for`] are different functions returning different things
//! rather than one convenient "servers for this session" call that would let a
//! definition silently become a grant.
//!
//! Three properties follow, and each is load-bearing:
//!
//! **Grants expire, and a lapsed one fails CLOSED.** An activation may carry a
//! TTL ("2h" → an absolute ISO expiry). [`activations_for`] filters expired
//! entries at read time against an injected `now`, so a grant that lapsed while
//! the server was down is gone on the next read — it never has to be swept.
//!
//! **Secrets live in the environment, not in this file.** An `env` value may be
//! `${VAR}`, expanded from bough's own environment when the child is spawned.
//! The expansion is deliberately NOT done at load: the registry is served over
//! HTTP and rendered in the `/mcp` UI, and an expanded token would then be
//! sitting in a response body and, worse, in the model's context. A missing
//! variable THROWS rather than expanding to empty.
//!
//! **MCP state is never cached.** Nothing here memoizes: every call re-reads
//! the file, because grants and connections change between turns and a cached
//! catalog is how the model ends up confidently calling a tool that was revoked
//! two turns ago.
//!
//! Rust delta: TS's `McpConfigOptions` and `ActivationOptions` are ONE struct
//! here ([`McpConfigOptions`], carrying `file`, `env` and `now`) — the TS split
//! existed only because `ActivationOptions extends McpConfigOptions`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{BoughError, ErrorKind};
use crate::mcp::keychain::{parse_keychain_ref, read_keychain_ref, KeychainOptions};
use crate::paths::mcp_registry_path;

/// Every failure in this subsystem is an `McpError` — a status plus a sentence
/// that names the server, what failed, and the move that resolves it.
pub fn mcp_error(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Mcp, message)
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// Server names are lowercase slugs. Not cosmetic: the name is what a program
/// passes to `bough mcp call` and what the prompt catalog prints, so it has to
/// be typeable by a model without quoting rules, and it must never be
/// mistakable for a path segment.
pub const NAME_MESSAGE: &str =
    "server names are lowercase slugs (a-z, 0-9, - and _, starting with a letter or digit)";

/// `^[a-z0-9][a-z0-9_-]*$`, hand-rolled so the hot path allocates nothing.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// One registry entry — a local stdio subprocess or a remote Streamable HTTP
/// endpoint.
///
/// Kept as ONE struct with cross-field rules rather than a discriminated union
/// so the failure a user actually hits — an entry with neither `command` nor
/// `url`, or with both — reports as that sentence instead of as two parallel
/// union failures neither of which names the real problem.
///
/// An old `allowWrite` key still loads and is ignored (dropped field).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    /// stdio transport: the executable to spawn. Mutually exclusive with `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra child env; a value may reference `${VAR}` from bough's environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory for the child. Absent = the session's checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Remote transport: the Streamable HTTP endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Static headers for a remote server, resolved at connect time and never
    /// at load: `${VAR}` or `${keychain:<item>#<a.b.c>}`. Write the reference,
    /// never the secret — this document is served by `GET /mcp/servers`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// A PRE-REGISTERED OAuth client for this server (Slack publishes
    /// `registration_endpoint: null`, so DCR is not always available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// The pre-registered client's secret, as a `${VAR}` REFERENCE and never a
    /// literal — this file is served over HTTP and rendered in the panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

/// True when this entry is a local stdio server (`client.rs` can connect it).
pub fn is_stdio(server: &ServerConfig) -> bool {
    server.command.as_deref().is_some_and(|c| !c.is_empty())
}

/// One grant: a server name, optionally until an absolute ISO instant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Activation {
    pub name: String,
    /// ISO 8601. Absent = until revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

/// What the registry surface returns: definitions only, never grants.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub servers: BTreeMap<String, ServerConfig>,
}

/// The whole document. `servers` are definitions; `activations` are grants
/// keyed by session id, with `""` as the GLOBAL scope.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    servers: BTreeMap<String, ServerConfig>,
    #[serde(default)]
    activations: BTreeMap<String, Vec<Activation>>,
}

/// Reads one variable from bough's environment. Injected so tests need no real
/// env.
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Where the store is, where `${VAR}` comes from, and what time it is.
///
/// Injected rather than read from the environment at each call site: a test
/// points `file` at a temp path and gets a hermetic registry, with no
/// `BOUGH_HOME` mutation and nothing written under the real `~/.bough`.
#[derive(Clone, Default)]
pub struct McpConfigOptions {
    /// The registry document. Absent = `~/.bough/mcp.json` (`paths.rs`).
    pub file: Option<PathBuf>,
    /// `${VAR}` source. Absent = the process environment.
    pub env: Option<EnvLookup>,
    /// Injected clock, epoch ms — TS's `ActivationOptions.now`. Absent = now.
    pub now: Option<i64>,
}

impl McpConfigOptions {
    /// The common test shape: a hermetic store, real env, real clock.
    pub fn with_file(file: impl Into<PathBuf>) -> McpConfigOptions {
        McpConfigOptions {
            file: Some(file.into()),
            env: None,
            now: None,
        }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        match &self.env {
            Some(f) => f(name),
            None => std::env::var(name).ok(),
        }
    }

    fn now(&self) -> i64 {
        self.now.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        })
    }
}

/// The registry document this call reads and writes.
pub fn registry_file(opts: &McpConfigOptions) -> PathBuf {
    opts.file.clone().unwrap_or_else(mcp_registry_path)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// One readable validation failure: `path: message`. These are a product
/// surface, not log text — they come back as the 400 body of
/// `PUT /mcp/servers/:name` and as the inline error under the field.
type Issue = (String, String);

fn issue(path: &str, message: &str) -> Issue {
    (path.to_string(), message.to_string())
}

fn render_issues(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(|(path, message)| {
            format!(
                "{}: {message}",
                if path.is_empty() { "(root)" } else { path }
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn looks_like_url(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-')
        && !rest.is_empty()
        && !rest.starts_with('/')
}

fn as_string(raw: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<String> {
    match raw {
        Value::String(s) => Some(s.clone()),
        other => {
            issues.push(issue(
                path,
                &format!("Expected string, received {}", kind_of(other)),
            ));
            None
        }
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn string_map(raw: &Value, path: &str, issues: &mut Vec<Issue>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    match raw {
        Value::Object(map) => {
            for (key, value) in map {
                if let Some(s) = as_string(value, &format!("{path}.{key}"), issues) {
                    out.insert(key.clone(), s);
                }
            }
        }
        other => issues.push(issue(
            path,
            &format!("Expected object, received {}", kind_of(other)),
        )),
    }
    out
}

/// Parse ONE registry entry, with every cross-field rule. The messages are
/// verbatim from `config.ts` — they are what a user reads in the panel.
fn parse_server(raw: &Value) -> Result<ServerConfig, Vec<Issue>> {
    let mut issues: Vec<Issue> = Vec::new();
    let Value::Object(map) = raw else {
        return Err(vec![issue(
            "",
            &format!("Expected object, received {}", kind_of(raw)),
        )]);
    };
    let mut cfg = ServerConfig::default();

    if let Some(v) = map.get("command") {
        if let Some(s) = as_string(v, "command", &mut issues) {
            if s.is_empty() {
                issues.push(issue(
                    "command",
                    "String must contain at least 1 character(s)",
                ));
            } else {
                cfg.command = Some(s);
            }
        }
    }
    if let Some(v) = map.get("args") {
        match v {
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    if let Some(s) = as_string(item, &format!("args.{i}"), &mut issues) {
                        cfg.args.push(s);
                    }
                }
            }
            other => issues.push(issue(
                "args",
                &format!("Expected array, received {}", kind_of(other)),
            )),
        }
    }
    if let Some(v) = map.get("env") {
        cfg.env = string_map(v, "env", &mut issues);
    }
    if let Some(v) = map.get("cwd") {
        cfg.cwd = as_string(v, "cwd", &mut issues);
    }
    if let Some(v) = map.get("url") {
        if let Some(s) = as_string(v, "url", &mut issues) {
            if looks_like_url(&s) {
                cfg.url = Some(s);
            } else {
                issues.push(issue("url", "Invalid url"));
            }
        }
    }
    if let Some(v) = map.get("headers") {
        cfg.headers = string_map(v, "headers", &mut issues);
    }
    if let Some(v) = map.get("clientId") {
        if let Some(s) = as_string(v, "clientId", &mut issues) {
            if s.is_empty() {
                issues.push(issue(
                    "clientId",
                    "String must contain at least 1 character(s)",
                ));
            } else {
                cfg.client_id = Some(s);
            }
        }
    }
    if let Some(v) = map.get("clientSecret") {
        cfg.client_secret = as_string(v, "clientSecret", &mut issues);
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    // The cross-field rules. The first one returns early, because everything
    // after it is meaningless when the transport is unclear (TS: `return`).
    if cfg.command.is_some() == cfg.url.is_some() {
        return Err(vec![issue(
            "",
            "a server needs exactly one of `command` (stdio) or `url` (remote)",
        )]);
    }
    if cfg.url.is_some() && (!cfg.args.is_empty() || !cfg.env.is_empty() || cfg.cwd.is_some()) {
        issues.push(issue(
            "",
            "a remote server takes `url` and `headers` — `args`, `env` and `cwd` \
             describe a subprocess and there is none",
        ));
    }
    if cfg.command.is_some() && !cfg.headers.is_empty() {
        issues.push(issue(
            "",
            "a stdio server takes `env` — `headers` are sent on an HTTP request \
             and there is none",
        ));
    }
    if cfg.command.is_some() && (cfg.client_id.is_some() || cfg.client_secret.is_some()) {
        issues.push(issue(
            "",
            "a stdio server takes `env` — `clientId`/`clientSecret` are an OAuth \
             client for a remote authorization server and there is none",
        ));
    }
    if cfg.client_secret.is_some() && cfg.client_id.is_none() {
        issues.push(issue(
            "",
            "`clientSecret` needs the `clientId` it belongs to — a secret alone \
             identifies nothing",
        ));
    }
    if let Some(secret) = &cfg.client_secret {
        if !is_var_reference(secret) {
            issues.push(issue(
                "",
                "`clientSecret` must be a `${VAR}` reference, not the secret itself — \
                 this file is served by GET /mcp/servers and rendered in the /mcp panel, so a \
                 literal would sit in a response body and in the model's context",
            ));
        }
    }
    if issues.is_empty() {
        Ok(cfg)
    } else {
        Err(issues)
    }
}

/// `^\$\{\w+\}$` — a reference, never a literal.
fn is_var_reference(value: &str) -> bool {
    let Some(inner) = value.strip_prefix("${").and_then(|v| v.strip_suffix('}')) else {
        return false;
    };
    !inner.is_empty() && inner.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Parse the `servers` map, collecting every issue with its path.
fn parse_servers(raw: &Value) -> Result<BTreeMap<String, ServerConfig>, Vec<Issue>> {
    let mut issues = Vec::new();
    let mut servers = BTreeMap::new();
    match raw {
        Value::Object(map) => {
            for (name, entry) in map {
                if !is_valid_name(name) {
                    issues.push(issue(&format!("servers.{name}"), NAME_MESSAGE));
                    continue;
                }
                match parse_server(entry) {
                    Ok(cfg) => {
                        servers.insert(name.clone(), cfg);
                    }
                    Err(inner) => {
                        for (path, message) in inner {
                            let full = if path.is_empty() {
                                format!("servers.{name}")
                            } else {
                                format!("servers.{name}.{path}")
                            };
                            issues.push((full, message));
                        }
                    }
                }
            }
        }
        other => issues.push(issue(
            "servers",
            &format!("Expected object, received {}", kind_of(other)),
        )),
    }
    if issues.is_empty() {
        Ok(servers)
    } else {
        Err(issues)
    }
}

fn parse_activations(raw: &Value) -> Option<BTreeMap<String, Vec<Activation>>> {
    let Value::Object(map) = raw else { return None };
    let mut out = BTreeMap::new();
    for (scope, list) in map {
        let Value::Array(items) = list else {
            return None;
        };
        let mut grants = Vec::new();
        for item in items {
            let Value::Object(entry) = item else {
                return None;
            };
            let name = entry.get("name")?.as_str()?.to_string();
            let expires = match entry.get("expires") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => return None,
            };
            grants.push(Activation { name, expires });
        }
        out.insert(scope.clone(), grants);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Read the whole document. Absent or corrupt ⇒ fail CLOSED: no servers, no
/// grants. A half-parsed registry that granted some servers and dropped others
/// would be the worst outcome — the model would see a catalog that is wrong
/// rather than empty.
fn read_document(opts: &McpConfigOptions) -> ConfigFile {
    let Ok(text) = std::fs::read_to_string(registry_file(opts)) else {
        return ConfigFile::default();
    };
    let Ok(raw) = serde_json::from_str::<Value>(&text) else {
        return ConfigFile::default();
    };
    let Value::Object(doc) = &raw else {
        return ConfigFile::default();
    };
    let servers = match doc.get("servers") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(v) => match parse_servers(v) {
            Ok(s) => s,
            Err(_) => return ConfigFile::default(),
        },
    };
    let activations = match doc.get("activations") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(v) => match parse_activations(v) {
            Some(a) => a,
            None => return ConfigFile::default(),
        },
    };
    ConfigFile {
        servers,
        activations,
    }
}

/// Pretty-printed 2-space + trailing newline, exactly as the TS writer.
fn write_document(doc: &ConfigFile, opts: &McpConfigOptions) -> Result<(), BoughError> {
    let path = registry_file(opts);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| mcp_error(500, format!("could not create {}: {e}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(doc)
        .map_err(|e| mcp_error(500, format!("could not serialize the MCP registry: {e}")))?;
    std::fs::write(&path, format!("{text}\n"))
        .map_err(|e| mcp_error(500, format!("could not write {}: {e}", path.display())))
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// Every registered server. A missing or corrupt file contributes nothing.
pub fn load_registry(opts: &McpConfigOptions) -> Registry {
    Registry {
        servers: read_document(opts).servers,
    }
}

/// One entry, or `None` when the name is not registered.
pub fn get_server(name: &str, opts: &McpConfigOptions) -> Option<ServerConfig> {
    read_document(opts).servers.remove(name)
}

/// One entry, or a 404 that NAMES the alternatives. The message is what a
/// program catches when it calls a mistyped server, so it has to be enough to
/// fix the call without another round trip.
pub fn require_server(name: &str, opts: &McpConfigOptions) -> Result<ServerConfig, BoughError> {
    let mut servers = read_document(opts).servers;
    if let Some(found) = servers.remove(name) {
        return Ok(found);
    }
    // BTreeMap: the alternatives are already sorted, as the TS `.sort()` makes
    // them.
    let known: Vec<&str> = servers.keys().map(|k| k.as_str()).collect();
    Err(mcp_error(
        404,
        format!(
            "no MCP server named \"{name}\" is registered. {} Register one with PUT /mcp/servers/{name}.",
            if known.is_empty() {
                "No servers are registered yet.".to_string()
            } else {
                format!("Registered servers: {}.", known.join(", "))
            }
        ),
    ))
}

/// Validate and persist the WHOLE registry, preserving activations.
///
/// The activations live in the same document, so a naive whole-file write here
/// would revoke every grant as a side effect of renaming a server. They are
/// merged back deliberately.
pub fn save_registry(raw: &Value, opts: &McpConfigOptions) -> Result<Registry, BoughError> {
    let servers_raw = match raw {
        Value::Object(map) => map
            .get("servers")
            .cloned()
            .unwrap_or(Value::Object(Default::default())),
        other => {
            return Err(mcp_error(
                400,
                format!(
                    "invalid MCP registry: (root): Expected object, received {}",
                    kind_of(other)
                ),
            ))
        }
    };
    let servers = parse_servers(&servers_raw)
        .map_err(|i| mcp_error(400, format!("invalid MCP registry: {}", render_issues(&i))))?;
    let doc = read_document(opts);
    let pruned = prune_activations(ConfigFile {
        servers,
        activations: doc.activations,
    });
    write_document(&pruned, opts)?;
    Ok(Registry {
        servers: pruned.servers,
    })
}

/// Add or replace ONE entry.
///
/// This exists so a caller never has to round-trip the whole registry to change
/// one server: a read-modify-write in shell is exactly where a sibling entry
/// gets dropped and a `${VAR}` reference gets expanded into a literal secret.
pub fn upsert_server(
    name: &str,
    raw: &Value,
    opts: &McpConfigOptions,
) -> Result<Registry, BoughError> {
    if !is_valid_name(name) {
        return Err(mcp_error(
            400,
            format!("invalid server name \"{name}\" — {NAME_MESSAGE}"),
        ));
    }
    let cfg = parse_server(raw).map_err(|i| {
        mcp_error(
            400,
            format!("invalid MCP server \"{name}\": {}", render_issues(&i)),
        )
    })?;
    let mut doc = read_document(opts);
    doc.servers.insert(name.to_string(), cfg);
    write_document(&doc, opts)?;
    Ok(Registry {
        servers: doc.servers,
    })
}

/// Remove one entry. Returns false when the name was not registered.
///
/// Removal also drops the server's ACTIVATIONS: a revoked-then-recreated server
/// should start ungranted.
pub fn remove_server(name: &str, opts: &McpConfigOptions) -> Result<bool, BoughError> {
    let mut doc = read_document(opts);
    if doc.servers.remove(name).is_none() {
        return Ok(false);
    }
    let pruned = prune_activations(doc);
    write_document(&pruned, opts)?;
    Ok(true)
}

/// Drop grants naming a server that no longer exists.
fn prune_activations(doc: ConfigFile) -> ConfigFile {
    let mut activations = BTreeMap::new();
    for (scope, list) in doc.activations {
        let kept: Vec<Activation> = list
            .into_iter()
            .filter(|a| doc.servers.contains_key(&a.name))
            .collect();
        if !kept.is_empty() {
            activations.insert(scope, kept);
        }
    }
    ConfigFile {
        servers: doc.servers,
        activations,
    }
}

// ---------------------------------------------------------------------------
// The child environment
// ---------------------------------------------------------------------------

/// Expand `${VAR}` references in a server's `env` values.
///
/// A missing variable throws. Silently expanding to empty produces a server
/// that starts, connects, advertises its tools, and fails every call with the
/// remote service's "unauthorized" — a failure that looks like the server's
/// fault and costs a turn to diagnose.
pub fn expand_env(
    env: &BTreeMap<String, String>,
    opts: &McpConfigOptions,
) -> Result<BTreeMap<String, String>, BoughError> {
    let mut out = BTreeMap::new();
    for (key, value) in env {
        out.insert(key.clone(), expand_one(key, value, opts)?);
    }
    Ok(out)
}

/// `\$\{(\w+)\}` substituted globally, inside larger strings too.
fn expand_one(key: &str, value: &str, opts: &McpConfigOptions) -> Result<String, BoughError> {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
            if let Some(end) = (i + 2..chars.len()).find(|&j| chars[j] == '}') {
                let name: String = chars[i + 2..end].iter().collect();
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    match opts.lookup(&name) {
                        Some(found) => out.push_str(&found),
                        None => {
                            return Err(mcp_error(
                                400,
                                format!(
                                    "MCP server env {key} references ${{{name}}}, which is not set. \
                                     Export {name} in ~/.bough/env (or the server's launch environment) \
                                     and try again — the value is never stored in the registry."
                                ),
                            ))
                        }
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    Ok(out)
}

/// Expand a remote server's `headers` at the moment they are sent.
///
/// `${VAR}` reads bough's environment exactly as a spawned server's `env` does.
/// `${keychain:<item>#<a.b.c>}` reads whichever credential store this machine
/// keeps the item in (`keychain.rs`, row 3.5) — the whole value, or the whole
/// value after a case-insensitive `Bearer `. NEVER a partial interpolation: a
/// keychain reference spliced into the middle of a larger string is how a secret
/// ends up in a log line that was only ever meant to hold a prefix.
///
/// Async because the store read is a subprocess or a file read; the TS is async
/// here for the same reason.
pub async fn expand_headers(
    headers: &BTreeMap<String, String>,
    opts: &McpConfigOptions,
    keychain: &KeychainOptions,
) -> Result<BTreeMap<String, String>, BoughError> {
    let mut out = BTreeMap::new();
    for (key, value) in headers {
        let trimmed = value.trim();
        if let Some(reference) = parse_keychain_ref(trimmed) {
            out.insert(key.clone(), read_keychain_ref(&reference, keychain).await?);
            continue;
        }
        // `Bearer ${keychain:…}` — the scheme is ours, the secret is the store's.
        if let Some(rest) = strip_bearer(trimmed) {
            if let Some(reference) = parse_keychain_ref(rest) {
                let secret = read_keychain_ref(&reference, keychain).await?;
                out.insert(key.clone(), format!("Bearer {secret}"));
                continue;
            }
        }
        out.insert(key.clone(), expand_one(key, value, opts)?);
    }
    Ok(out)
}

/// The value after a case-insensitive leading `Bearer `, or `None`.
fn strip_bearer(value: &str) -> Option<&str> {
    let (head, rest) = value.split_once(' ')?;
    head.eq_ignore_ascii_case("Bearer")
        .then(|| rest.trim_start())
}

/// Variables inherited by a spawned server, by name.
///
/// The child gets a COMPOSED environment (clear-env at the spawn), not bough's
/// own: a server is a third-party binary reading whatever it likes, and handing
/// it every provider key in the process environment is a leak with no upside.
pub const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "TZ",
    "SHELL",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "DENO_DIR",
];

/// The child's ENTIRE environment: the inherited names above, plus the server's
/// own declared `env` with `${VAR}` expanded. Declared values win on a collision
/// — a server that overrides PATH meant to.
pub fn child_env(
    server: &ServerConfig,
    opts: &McpConfigOptions,
) -> Result<BTreeMap<String, String>, BoughError> {
    let mut out = BTreeMap::new();
    for name in INHERITED_ENV {
        if let Some(value) = opts.lookup(name) {
            out.insert((*name).to_string(), value);
        }
    }
    out.extend(expand_env(&server.env, opts)?);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/// The global scope key: a grant every session sees.
pub const GLOBAL_SCOPE: &str = "";

/// `Date.parse` of an ISO instant, in ms. `None` when it does not parse — and
/// an unparseable expiry is NOT expired (TS compares against `NaN`, which is
/// false either way), so a hand-edited file cannot silently revoke a grant.
fn parse_iso_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn expired(activation: &Activation, now: i64) -> bool {
    match &activation.expires {
        None => false,
        Some(iso) => parse_iso_ms(iso).is_some_and(|at| at <= now),
    }
}

/// Server names manually granted to this session: its own scope plus the global
/// one, with expired grants filtered out.
///
/// This is only half of a turn's grant — a skill's `mcp:` frontmatter is the
/// other half, resolved by the layer that assembles the turn. A subagent
/// inherits its spawner's resolved grant rather than reading this, which is why
/// this takes a session id and not a lineage.
pub fn activations_for(session_id: Option<&str>, opts: &McpConfigOptions) -> Vec<String> {
    let now = opts.now();
    let doc = read_document(opts);
    let mut scopes: Vec<&str> = vec![GLOBAL_SCOPE];
    if let Some(id) = session_id.filter(|s| !s.is_empty()) {
        scopes.push(id);
    }
    let mut names = BTreeSet::new();
    for scope in scopes {
        for activation in doc.activations.get(scope).into_iter().flatten() {
            if expired(activation, now) {
                continue;
            }
            names.insert(activation.name.clone());
        }
    }
    names.into_iter().collect()
}

/// Grant or revoke a server for one scope (a session id, or `None` = global).
///
/// A grant REPLACES any existing one for the same name, so re-enabling with a
/// fresh TTL extends it rather than leaving a lapsed entry beside a live one.
pub fn set_activation(
    session_id: Option<&str>,
    name: &str,
    on: bool,
    expires: Option<&str>,
    opts: &McpConfigOptions,
) -> Result<(), BoughError> {
    let mut doc = read_document(opts);
    let scope = session_id.unwrap_or(GLOBAL_SCOPE).to_string();
    let mut rest: Vec<Activation> = doc
        .activations
        .get(&scope)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.name != name)
        .collect();
    if on {
        rest.push(Activation {
            name: name.to_string(),
            expires: expires.map(|e| e.to_string()),
        });
    }
    if rest.is_empty() {
        doc.activations.remove(&scope);
    } else {
        doc.activations.insert(scope, rest);
    }
    write_document(&doc, opts)
}

/// Revoke `name` in EVERY scope — the global one and every session that holds
/// it. The panel's ⏎ grants globally, so its opposite has to mean what it says:
/// a permission surface may not be approximately right.
pub fn revoke_everywhere(name: &str, opts: &McpConfigOptions) -> Result<(), BoughError> {
    let mut doc = read_document(opts);
    let mut next = BTreeMap::new();
    for (scope, list) in doc.activations {
        let rest: Vec<Activation> = list.into_iter().filter(|a| a.name != name).collect();
        if !rest.is_empty() {
            next.insert(scope, rest);
        }
    }
    doc.activations = next;
    write_document(&doc, opts)
}

/// Promote every per-conversation grant to the global scope, once.
///
/// It BROADENS a permission, which is not a thing to do lightly or twice: it
/// runs only where session-scoped rows exist, empties them, and is therefore a
/// no-op ever after. A TTL is dropped with the row it was on — a grant meant to
/// lapse in two hours must not be promoted into a permanent one.
///
/// Returns the names promoted, for the boot log: a permission change nobody is
/// told about is the kind that is discovered later, in the wrong way.
pub fn promote_session_grants(opts: &McpConfigOptions) -> Result<Vec<String>, BoughError> {
    let mut doc = read_document(opts);
    let scopes: Vec<String> = doc
        .activations
        .keys()
        .filter(|s| s.as_str() != GLOBAL_SCOPE)
        .cloned()
        .collect();
    if scopes.is_empty() {
        return Ok(Vec::new());
    }
    let global: BTreeSet<String> = doc
        .activations
        .get(GLOBAL_SCOPE)
        .into_iter()
        .flatten()
        .map(|a| a.name.clone())
        .collect();
    let mut promoted: BTreeSet<String> = BTreeSet::new();
    for scope in &scopes {
        for a in doc.activations.get(scope).into_iter().flatten() {
            // A lapsed grant is already gone as far as every reader is
            // concerned, and a live TTL was a deliberate limit. Neither
            // becomes permanent here.
            if a.expires.is_some() {
                continue;
            }
            if !global.contains(&a.name) {
                promoted.insert(a.name.clone());
            }
        }
        doc.activations.remove(scope);
    }
    let merged: BTreeSet<String> = global.union(&promoted).cloned().collect();
    if merged.is_empty() {
        doc.activations.remove(GLOBAL_SCOPE);
    } else {
        doc.activations.insert(
            GLOBAL_SCOPE.to_string(),
            merged
                .into_iter()
                .map(|name| Activation {
                    name,
                    expires: None,
                })
                .collect(),
        );
    }
    write_document(&doc, opts)?;
    Ok(promoted.into_iter().collect())
}

/// Parse a `"90m" | "2h" | "7d"` TTL into an absolute ISO expiry.
///
/// Absolute, not a duration stored as-is: a duration would silently restart
/// every time the file was rewritten, and a grant meant to last two hours would
/// outlive the machine.
pub fn ttl_to_expires(ttl: &str, now: i64) -> Result<String, BoughError> {
    let refuse = || {
        mcp_error(
            400,
            format!(
                "invalid ttl \"{ttl}\" — use a whole number of minutes, hours or days, \
                 e.g. \"90m\", \"2h\", \"7d\". Omit it entirely to grant until revoked."
            ),
        )
    };
    let trimmed = ttl.trim();
    let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Err(refuse());
    }
    // `^(\d+)\s*(m|h|d)$` — internal whitespace is allowed, trailing is not
    // (the whole value was trimmed first).
    let unit_ms = match trimmed[digits.len()..].trim_start() {
        "m" => 60_000i64,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return Err(refuse()),
    };
    let amount: i64 = digits.parse().map_err(|_| refuse())?;
    Ok(iso_ms(now + amount * unit_ms))
}

/// `new Date(ms).toISOString()` — always three fractional digits and a `Z`.
pub fn iso_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp_millis(0).expect("epoch"))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn tmp_file() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-mcp-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("mcp.json")
    }

    fn opts(file: &Path) -> McpConfigOptions {
        McpConfigOptions::with_file(file)
    }

    fn env_opts(file: &Path, pairs: &'static [(&'static str, &'static str)]) -> McpConfigOptions {
        McpConfigOptions {
            file: Some(file.to_path_buf()),
            env: Some(Arc::new(move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            })),
            now: None,
        }
    }

    fn at(file: &Path, now: i64) -> McpConfigOptions {
        McpConfigOptions {
            file: Some(file.to_path_buf()),
            env: None,
            now: Some(now),
        }
    }

    #[test]
    fn registry_is_empty_when_absent_round_trips_and_a_definition_is_not_a_grant() {
        let file = tmp_file();
        let o = opts(&file);
        assert_eq!(load_registry(&o), Registry::default());

        save_registry(
            &json!({"servers": {"echo": {"command": "deno", "args": ["run", "srv.ts"]}}}),
            &o,
        )
        .unwrap();
        let registry = load_registry(&o);
        assert_eq!(registry.servers.keys().collect::<Vec<_>>(), vec!["echo"]);
        assert_eq!(registry.servers["echo"].command.as_deref(), Some("deno"));
        assert_eq!(registry.servers["echo"].args, vec!["run", "srv.ts"]);
        assert!(is_stdio(&registry.servers["echo"]));

        // Registering granted nothing: no session sees it until something activates it.
        assert!(activations_for(Some("s1"), &o).is_empty());
    }

    #[test]
    fn a_corrupt_file_contributes_nothing_rather_than_half_a_catalog() {
        let file = tmp_file();
        let o = opts(&file);
        std::fs::write(&file, "{ this is not json").unwrap();
        assert_eq!(load_registry(&o), Registry::default());
        std::fs::write(
            &file,
            json!({"servers": {"echo": {"command": 42}}}).to_string(),
        )
        .unwrap();
        assert_eq!(load_registry(&o), Registry::default());
        // …and no grants either: never half a catalog.
        std::fs::write(
            &file,
            json!({"servers": {"echo": {"command": 42}}, "activations": {"": [{"name": "echo"}]}})
                .to_string(),
        )
        .unwrap();
        assert!(activations_for(Some("s1"), &o).is_empty());
    }

    #[test]
    fn entry_shapes_are_rejected_with_a_sentence_naming_the_fix() {
        let file = tmp_file();
        let o = opts(&file);
        // Neither transport, and both, are the same mistake reported the same way.
        for bad in [
            json!({}),
            json!({"command": "x", "url": "https://y.example"}),
        ] {
            let e = save_registry(&json!({"servers": {"bad": bad}}), &o).unwrap_err();
            assert_eq!(e.status(), 400);
            assert!(e.to_string().contains("exactly one of `command`"), "{e}");
        }
        // Names are slugs — both on the whole-registry path and the per-server one.
        assert_eq!(
            save_registry(&json!({"servers": {"Bad Name": {"command": "x"}}}), &o)
                .unwrap_err()
                .status(),
            400
        );
        assert_eq!(
            upsert_server("Bad Name", &json!({"command": "x"}), &o)
                .unwrap_err()
                .status(),
            400
        );
        // Transport-specific keys on the wrong transport.
        let e = upsert_server(
            "remote",
            &json!({"url": "https://y.example", "args": ["--x"]}),
            &o,
        )
        .unwrap_err();
        assert!(e.to_string().contains("remote server takes"), "{e}");
        let e = upsert_server("local", &json!({"command": "x", "headers": {"a": "b"}}), &o)
            .unwrap_err();
        assert!(e.to_string().contains("stdio server takes"), "{e}");
        // A PRE-REGISTERED OAuth client belongs to a remote server and nothing else.
        let e =
            upsert_server("local", &json!({"command": "x", "clientId": "abc"}), &o).unwrap_err();
        assert!(e.to_string().contains("stdio server takes"), "{e}");
        // A secret identifies nothing on its own.
        let e = upsert_server(
            "remote",
            &json!({"url": "https://y.example", "clientSecret": "${SOME_VAR}"}),
            &o,
        )
        .unwrap_err();
        assert!(e.to_string().contains("needs the `clientId`"), "{e}");
        // THE ONE THAT MATTERS: a literal secret is refused.
        let e = upsert_server(
            "remote",
            &json!({
                "url": "https://y.example",
                "clientId": "abc",
                "clientSecret": "xoxb-the-actual-secret"
            }),
            &o,
        )
        .unwrap_err();
        assert!(
            e.to_string().contains("must be a `${VAR}` reference"),
            "{e}"
        );
        assert_eq!(
            load_registry(&o),
            Registry::default(),
            "nothing was written"
        );
    }

    #[test]
    fn a_pre_registered_oauth_client_round_trips_and_the_secret_stays_a_reference() {
        let file = tmp_file();
        let o = opts(&file);
        upsert_server(
            "slack",
            &json!({
                "url": "https://mcp.slack.com/mcp",
                "clientId": "1234.5678",
                "clientSecret": "${SLACK_MCP_CLIENT_SECRET}"
            }),
            &o,
        )
        .unwrap();
        let entry = load_registry(&o).servers["slack"].clone();
        assert_eq!(entry.client_id.as_deref(), Some("1234.5678"));
        assert_eq!(
            entry.client_secret.as_deref(),
            Some("${SLACK_MCP_CLIENT_SECRET}")
        );
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("${SLACK_MCP_CLIENT_SECRET}"),
            "the reference, not a resolved value, is what is stored"
        );
    }

    #[test]
    fn upsert_replaces_one_entry_without_touching_siblings_and_remove_deletes() {
        let file = tmp_file();
        let o = opts(&file);
        save_registry(
            &json!({"servers": {"exa": {"command": "npx", "args": ["exa-mcp"]}}}),
            &o,
        )
        .unwrap();
        upsert_server(
            "echo",
            &json!({"command": "deno", "args": ["run", "srv.ts"]}),
            &o,
        )
        .unwrap();
        assert_eq!(
            load_registry(&o).servers.keys().collect::<Vec<_>>(),
            vec!["echo", "exa"]
        );
        assert_eq!(load_registry(&o).servers["exa"].args, vec!["exa-mcp"]);

        upsert_server("echo", &json!({"url": "https://mcp.example.com/mcp"}), &o).unwrap();
        assert_eq!(
            get_server("echo", &o).unwrap().url.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
        assert!(!is_stdio(&get_server("echo", &o).unwrap()));

        assert!(remove_server("echo", &o).unwrap());
        assert!(!remove_server("echo", &o).unwrap());
        assert_eq!(
            load_registry(&o).servers.keys().collect::<Vec<_>>(),
            vec!["exa"]
        );
    }

    #[test]
    fn require_server_names_the_alternatives_instead_of_saying_not_found() {
        let file = tmp_file();
        let o = opts(&file);
        save_registry(
            &json!({"servers": {"exa": {"command": "npx"}, "linear": {"url": "https://l.example"}}}),
            &o,
        )
        .unwrap();
        assert_eq!(
            require_server("exa", &o).unwrap().command.as_deref(),
            Some("npx")
        );
        let e = require_server("linaer", &o).unwrap_err();
        assert_eq!(e.status(), 404);
        assert!(
            e.to_string().contains("Registered servers: exa, linear"),
            "{e}"
        );
        assert!(e.to_string().contains("PUT /mcp/servers/linaer"), "{e}");
    }

    #[test]
    fn save_preserves_grants_and_remove_revokes_the_ones_it_orphans() {
        let file = tmp_file();
        let o = opts(&file);
        save_registry(
            &json!({"servers": {"echo": {"command": "deno"}, "exa": {"command": "npx"}}}),
            &o,
        )
        .unwrap();
        set_activation(Some("s1"), "echo", true, None, &o).unwrap();
        set_activation(None, "exa", true, None, &o).unwrap();

        // Rewriting the registry must not revoke every grant as a side effect.
        save_registry(
            &json!({"servers": {
                "echo": {"command": "deno", "args": ["-A"]},
                "exa": {"command": "npx"}
            }}),
            &o,
        )
        .unwrap();
        assert_eq!(activations_for(Some("s1"), &o), vec!["echo", "exa"]);

        // Deleting the server deletes its grant, so re-registering starts ungranted.
        remove_server("echo", &o).unwrap();
        assert_eq!(activations_for(Some("s1"), &o), vec!["exa"]);
        upsert_server("echo", &json!({"command": "deno"}), &o).unwrap();
        assert_eq!(activations_for(Some("s1"), &o), vec!["exa"]);
    }

    #[test]
    fn promote_session_grants_lifts_old_per_conversation_grants_to_the_global_scope() {
        let file = tmp_file();
        let o = opts(&file);
        save_registry(
            &json!({"servers": {
                "echo": {"command": "deno"},
                "exa": {"command": "npx"},
                "linear": {"url": "https://l.example"}
            }}),
            &o,
        )
        .unwrap();
        set_activation(Some("s1"), "echo", true, None, &o).unwrap();
        set_activation(Some("s2"), "echo", true, None, &o).unwrap(); // the same server twice
        set_activation(Some("s2"), "exa", true, None, &o).unwrap();
        set_activation(None, "linear", true, None, &o).unwrap(); // already global
                                                                 // A TTL grant is a deliberate limit and must not become permanent.
        let ttl = ttl_to_expires("2h", o.now()).unwrap();
        set_activation(Some("s1"), "linear", true, Some(&ttl), &o).unwrap();

        assert_eq!(promote_session_grants(&o).unwrap(), vec!["echo", "exa"]);
        assert_eq!(activations_for(None, &o), vec!["echo", "exa", "linear"]);
        // The session rows are gone, so a conversation resolves the global set…
        assert_eq!(
            activations_for(Some("s1"), &o),
            vec!["echo", "exa", "linear"]
        );
        // …and running it again is a no-op, which is what makes it safe at every boot.
        assert!(promote_session_grants(&o).unwrap().is_empty());
    }

    #[test]
    fn revoke_everywhere_clears_the_global_scope_and_every_session_that_holds_it() {
        let file = tmp_file();
        let o = opts(&file);
        save_registry(
            &json!({"servers": {"echo": {"command": "deno"}, "exa": {"command": "npx"}}}),
            &o,
        )
        .unwrap();
        set_activation(Some("s1"), "echo", true, None, &o).unwrap();
        set_activation(Some("s2"), "echo", true, None, &o).unwrap();
        set_activation(None, "echo", true, None, &o).unwrap();
        set_activation(Some("s1"), "exa", true, None, &o).unwrap();

        revoke_everywhere("echo", &o).unwrap();
        assert_eq!(activations_for(Some("s1"), &o), vec!["exa"]);
        assert!(activations_for(Some("s2"), &o).is_empty());
        assert!(activations_for(None, &o).is_empty());
    }

    #[test]
    fn activations_scope_per_session_and_globally_and_a_lapsed_ttl_fails_closed() {
        let file = tmp_file();
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-27T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let o = at(&file, now);
        save_registry(
            &json!({"servers": {"echo": {"command": "deno"}, "linear": {"url": "https://l.example"}}}),
            &o,
        )
        .unwrap();

        assert!(activations_for(Some("s1"), &o).is_empty());
        set_activation(Some("s1"), "echo", true, None, &o).unwrap();
        set_activation(None, "linear", true, None, &o).unwrap(); // global
        assert_eq!(activations_for(Some("s1"), &o), vec!["echo", "linear"]);
        assert_eq!(activations_for(Some("s2"), &o), vec!["linear"]);
        assert_eq!(activations_for(None, &o), vec!["linear"]);

        // A TTL is stored absolute and read against the injected clock.
        let expires = ttl_to_expires("2h", now).unwrap();
        set_activation(Some("s1"), "echo", true, Some(&expires), &o).unwrap();
        assert_eq!(
            activations_for(Some("s1"), &at(&file, now + 3_600_000)),
            vec!["echo", "linear"]
        );
        assert_eq!(
            activations_for(Some("s1"), &at(&file, now + 7_200_001)),
            vec!["linear"]
        );

        // Re-enabling replaces the lapsed grant rather than sitting beside it.
        set_activation(Some("s1"), "echo", true, None, &o).unwrap();
        assert_eq!(
            activations_for(Some("s1"), &at(&file, now + 7_200_001)),
            vec!["echo", "linear"]
        );
        set_activation(Some("s1"), "echo", false, None, &o).unwrap();
        set_activation(None, "linear", false, None, &o).unwrap();
        assert!(activations_for(Some("s1"), &o).is_empty());
    }

    #[test]
    fn ttl_parses_the_three_forms_and_refuses_anything_else() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-27T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            ttl_to_expires("90m", now).unwrap(),
            iso_ms(now + 90 * 60_000)
        );
        assert_eq!(
            ttl_to_expires(" 2h ", now).unwrap(),
            iso_ms(now + 2 * 3_600_000)
        );
        assert_eq!(
            ttl_to_expires("7d", now).unwrap(),
            iso_ms(now + 7 * 86_400_000)
        );
        assert_eq!(
            ttl_to_expires("2 h", now).unwrap(),
            iso_ms(now + 2 * 3_600_000)
        );
        let e = ttl_to_expires("forever", now).unwrap_err();
        assert!(e.to_string().contains("\"90m\", \"2h\", \"7d\""), "{e}");
        assert!(ttl_to_expires("2w", now).is_err());
        assert!(ttl_to_expires("h", now).is_err());
    }

    #[test]
    fn expand_env_substitutes_var_and_refuses_to_start_on_a_missing_one() {
        let file = tmp_file();
        let o = env_opts(&file, &[("TOK", "s3cr3t")]);
        let env: BTreeMap<String, String> = [
            ("TOKEN", "${TOK}"),
            ("MIXED", "Bearer ${TOK}"),
            ("PLAIN", "as-is"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let expanded = expand_env(&env, &o).unwrap();
        assert_eq!(expanded["TOKEN"], "s3cr3t");
        assert_eq!(expanded["MIXED"], "Bearer s3cr3t");
        assert_eq!(expanded["PLAIN"], "as-is");

        let missing: BTreeMap<String, String> = [("TOKEN".to_string(), "${NOPE}".to_string())]
            .into_iter()
            .collect();
        let e = expand_env(&missing, &o).unwrap_err();
        assert_eq!(e.status(), 400);
        assert!(e.to_string().contains("${NOPE}"), "{e}");
        assert!(
            e.to_string().contains("never stored in the registry"),
            "{e}"
        );
    }

    #[test]
    fn the_secret_reference_is_stored_never_the_secret() {
        let file = tmp_file();
        let o = opts(&file);
        upsert_server(
            "linear",
            &json!({"url": "https://l.example", "headers": {}}),
            &o,
        )
        .unwrap();
        upsert_server(
            "gh",
            &json!({"command": "gh-mcp", "env": {"TOKEN": "${GH_TOKEN}"}}),
            &o,
        )
        .unwrap();
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(on_disk.contains("${GH_TOKEN}"));
        assert!(!on_disk.contains("s3cr3t"));
        // The value only appears when a child is about to be spawned.
        let composed = child_env(
            &get_server("gh", &o).unwrap(),
            &env_opts(&file, &[("GH_TOKEN", "s3cr3t"), ("PATH", "/usr/bin")]),
        )
        .unwrap();
        assert_eq!(composed["TOKEN"], "s3cr3t");
    }

    #[test]
    fn child_env_composes_the_childs_whole_environment() {
        let file = tmp_file();
        let host = env_opts(
            &file,
            &[
                ("PATH", "/usr/bin"),
                ("HOME", "/home/u"),
                ("HTTPS_PROXY", "http://proxy:8080"),
                ("ANTHROPIC_API_KEY", "sk-do-not-leak"),
                ("RANDOM_THING", "no"),
            ],
        );
        let server =
            parse_server(&json!({"command": "srv", "env": {"PATH": "/opt/bin", "X": "1"}}))
                .unwrap();
        let composed = child_env(&server, &host).unwrap();
        assert_eq!(composed["HOME"], "/home/u");
        assert_eq!(composed["HTTPS_PROXY"], "http://proxy:8080");
        assert_eq!(composed["X"], "1");
        // Declared values win on a collision — a server that overrides PATH meant to.
        assert_eq!(composed["PATH"], "/opt/bin");
        // Everything else stays behind: a third-party binary gets no provider keys.
        assert!(!composed.contains_key("ANTHROPIC_API_KEY"));
        assert!(!composed.contains_key("RANDOM_THING"));
    }

    #[test]
    fn nothing_is_cached_every_call_re_reads_the_file() {
        let file = tmp_file();
        let o = opts(&file);
        save_registry(&json!({"servers": {"echo": {"command": "deno"}}}), &o).unwrap();
        assert_eq!(load_registry(&o).servers.len(), 1);
        // Edited by something that is not this process.
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&json!({
                "servers": {"echo": {"command": "deno"}, "exa": {"command": "npx"}},
                "activations": {"": [{"name": "exa"}]}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            load_registry(&o).servers.len(),
            2,
            "the very next read sees the edit"
        );
        assert_eq!(activations_for(Some("s1"), &o), vec!["exa"]);
    }

    #[tokio::test]
    async fn a_keychain_header_reference_reaches_the_store_and_reports_its_failures() {
        // Row 3.5 replaced the "not ported" arm with a real keychain read. What is
        // asserted here is the CONFIG half: the reference reaches the store rather
        // than being sent as a literal, and a store failure surfaces as config's 400
        // naming the item. The resolution itself is pinned in `keychain.rs`.
        use crate::mcp::keychain::{reader_fn, KeychainResult};

        let file = tmp_file();
        let headers: BTreeMap<String, String> = [(
            "Authorization".to_string(),
            "Bearer ${keychain:Claude Code-credentials#claudeAiOauth.accessToken}".to_string(),
        )]
        .into_iter()
        .collect();
        let absent = KeychainOptions {
            keychain: Some(reader_fn(|_| async { KeychainResult::miss(44, "", None) })),
        };
        let e = expand_headers(&headers, &opts(&file), &absent)
            .await
            .unwrap_err();
        assert_eq!(e.status(), 400);
        assert!(e.to_string().contains("Claude Code-credentials"), "{e}");
    }
}
