//! `bough sync-mcp` — adopt Claude Code's MCP servers into bough's registry (port of
//! `src/cli/sync_mcp.ts`).
//!
//! THE INVARIANT THIS HOLDS, inherited from `mcp/config.rs` and non-negotiable:
//! **what gets written down is a REFERENCE, never a secret.** bough's registry is
//! served by `GET /mcp/servers` and rendered in the `/mcp` panel, so a token copied
//! into it would sit in a response body and, from there, in the model's context.
//! "Pull the tokens from the Mac secrets" is therefore implemented as
//! `${keychain:Claude Code-credentials#claudeAiOauth.accessToken}` — the item's NAME
//! is stored, the read happens at connect time in `mcp/keychain.rs`, and the value
//! goes into one request header and nowhere else.
//!
//! EVERY SERVER GETS THE CREDENTIAL THAT IS ACTUALLY ITS OWN. The login item holds
//! TWO things: the account token above, and `mcpOAuth` — one OAuth grant per remote
//! server Claude Code has authorized, keyed `<serverName>|<hash>`. A server with a
//! grant is referenced to THAT grant. Only a host the account token belongs to
//! (`claude.ai`, `*.anthropic.com`) is referenced to the account token, and
//! everything else is registered unauthenticated. The generalization this refuses —
//! "it is remote, so give it the bearer token" — would post the user's Anthropic
//! credential to whatever third party a config file names, which is a credential leak
//! with a helpful tone of voice.
//!
//! WHAT IT READS, in Claude Code's own order of scope:
//!
//!   ~/.claude.json                    → `mcpServers`                 (user scope)
//!   $CLAUDE_CONFIG_DIR/.claude.json   → `mcpServers`                 (user scope, current)
//!   either of those                   → `projects[<dir>].mcpServers` (that project)
//!   <dir>/.mcp.json                   → `mcpServers`                 (checked in)
//!   installed plugins                 → `.mcp.json`, `plugin.json`   (plugin servers)
//!   the credential store              → `mcpOAuth`                   (authorized remotes)
//!
//! WHAT IT NEVER DOES: overwrite. A name already in bough's registry is left exactly
//! as it is and reported — `--force` is how you say otherwise. Nothing here grants
//! anything either.
//!
//! Exit codes: 0 synced (or nothing to do), 1 a source was unreadable, 2 usage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Map, Value};

use bough_core::mcp::config::{load_registry, upsert_server, McpConfigOptions, ServerConfig};
use bough_core::mcp::keychain::{
    claude_config_dir_from, credential_reader_for, KeychainReader, CLAUDE_CODE_ITEM,
};

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/// A remote server Claude Code has authorized, with the key its grant is under.
#[derive(Debug, Clone, PartialEq)]
pub struct KeychainGrant {
    pub key: String,
    pub name: String,
    pub url: String,
    /// Epoch ms, when the entry carried one.
    pub expires_at: Option<i64>,
    /// The entry records a grant but holds no token. Claude Code leaves these behind
    /// for a connector it no longer has access to, and the reference written for one
    /// can never resolve — so it is worth SAYING at sync time rather than discovering
    /// as "has no string at #mcpOAuth…" per server, later, in a panel.
    ///
    /// A boolean, never the token: nothing in this process needs the value, and a
    /// secret that is never loaded cannot be logged by accident.
    pub empty: bool,
}

/// Already past its expiry at sync time.
pub fn is_stale(g: &KeychainGrant, now: i64) -> bool {
    g.expires_at.is_some_and(|at| at <= now)
}

/// Does this item hold any `mcpOAuth` grants? The store-selection predicate.
///
/// An item parsed but EMPTY of grants is a miss rather than an answer, which is the
/// whole point: it is exactly the keychain blob that was winning the read.
pub fn holds_grants(value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<Value>(value) else {
        return false;
    };
    parsed
        .get("mcpOAuth")
        .and_then(|m| m.as_object())
        .is_some_and(|m| !m.is_empty())
}

/// Every server in the login item's `mcpOAuth` map.
///
/// A read failure is NOT an error here: no store on this machine holding it, a denied
/// dialog, or simply no such item all mean "there are no grants to adopt", and the
/// config-file half of this command must still work. The one thing worth saying out
/// loud is a denied prompt, since that is a decision the user just made.
pub async fn read_grants(read: &KeychainReader) -> (Vec<KeychainGrant>, Option<String>) {
    let result = read(CLAUDE_CODE_ITEM.to_string()).await;
    if result.code != 0 || result.value.is_empty() {
        // 128 is the macOS "allow access?" dialog being dismissed, or the credentials
        // file being unreadable. Either way it is access being withheld, not absent.
        if result.code == 128 {
            let tail = if result.error.is_empty() {
                String::new()
            } else {
                format!(": {}", result.error)
            };
            return (
                vec![],
                Some(format!(
                    "access to \"{CLAUDE_CODE_ITEM}\" was denied, so no authorized remote \
                     servers were adopted{tail}. On macOS, re-run and choose Allow."
                )),
            );
        }
        let note = (!result.error.is_empty() && result.code != 44).then(|| result.error.clone());
        return (vec![], note);
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&result.value) else {
        return (
            vec![],
            Some(format!(
                "the \"{CLAUDE_CODE_ITEM}\" credential item is not JSON"
            )),
        );
    };
    let Some(map) = parsed.get("mcpOAuth").and_then(|m| m.as_object()) else {
        return (vec![], None);
    };
    let mut grants = Vec::new();
    for (key, raw) in map {
        // A shape we do not recognize is not a server.
        let (Some(name), Some(url)) = (
            raw.get("serverName")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty()),
            raw.get("serverUrl")
                .and_then(|v| v.as_str())
                .filter(|s| looks_like_url(s)),
        ) else {
            continue;
        };
        let token = raw.get("accessToken").and_then(|v| v.as_str());
        grants.push(KeychainGrant {
            key: key.clone(),
            name: name.to_string(),
            url: url.to_string(),
            expires_at: raw
                .get("expiresAt")
                .and_then(|v| v.as_f64())
                .map(|f| f as i64),
            empty: token.is_none_or(|t| t.is_empty()),
        });
    }
    (grants, None)
}

fn looks_like_url(v: &str) -> bool {
    v.split_once("://")
        .is_some_and(|(scheme, rest)| !scheme.is_empty() && !rest.is_empty())
}

/// The keychain item Claude Code keeps its claude.ai OAuth blob in, and the path to
/// the access token inside it.
fn claude_token_ref() -> String {
    format!("${{keychain:{CLAUDE_CODE_ITEM}#claudeAiOauth.accessToken}}")
}

/// A per-server grant Claude Code obtained, as a reference to it.
///
/// The SAME keychain item holds a second map — `mcpOAuth`, keyed by
/// `<serverName>|<hash>` — with one OAuth grant per remote server it has authorized.
/// That map is why the first cut of this command could not sync Slack at all.
fn grant_ref(key: &str) -> String {
    format!("${{keychain:{CLAUDE_CODE_ITEM}#mcpOAuth.{key}.accessToken}}")
}

/// Hosts the claude.ai credential belongs to.
///
/// Matched on the host's SUFFIX after a dot, never with a substring test — `claude.ai`
/// as a substring also matches `claude.ai.evil.example`, which is precisely the case
/// that must not receive the token.
const ANTHROPIC_HOSTS: [&str; 2] = ["claude.ai", "anthropic.com"];

fn is_anthropic_host(url: &str) -> bool {
    let Some(host) = hostname(url) else {
        return false;
    };
    ANTHROPIC_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

fn hostname(url: &str) -> Option<String> {
    let i = url.find("://")?;
    if url[..i].is_empty() {
        return None;
    }
    let rest = &url[i + 3..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = match authority.rfind('@') {
        Some(j) => &authority[j + 1..],
        None => authority,
    };
    let host = authority.split(':').next().unwrap_or("");
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// Claude Code's server entries
// ---------------------------------------------------------------------------

/// Claude Code's server entry, read permissively.
///
/// Every field optional and unknown keys ignored, because this is ANOTHER tool's
/// file: a field bough does not know about is not an error, and a strict schema here
/// would turn "Claude Code added a key" into "sync-mcp is broken".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeServer {
    pub r#type: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    /// A PRE-REGISTERED OAuth client, which a plugin ships when its provider does not
    /// do dynamic registration. Slack is the case: it publishes
    /// `registration_endpoint: null`, so bough's own `a`-to-authorize cannot get in
    /// without a `client_id` it is told. Dropping it made an adopted Slack entry
    /// un-reauthorizable the moment its copied grant expired.
    pub client_id: Option<String>,
}

/// `None` when the value is not a server definition at all.
pub fn parse_claude_server(raw: &Value) -> Option<ClaudeServer> {
    let obj = raw.as_object()?;
    let string = |k: &str| -> Result<Option<String>, ()> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(()),
        }
    };
    let string_map = |k: &str| -> Result<Option<BTreeMap<String, String>>, ()> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(m)) => {
                let mut out = BTreeMap::new();
                for (key, v) in m {
                    out.insert(key.clone(), v.as_str().ok_or(())?.to_string());
                }
                Ok(Some(out))
            }
            Some(_) => Err(()),
        }
    };
    let args = match obj.get("args") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for v in a {
                out.push(v.as_str()?.to_string());
            }
            Some(out)
        }
        Some(_) => return None,
    };
    Some(ClaudeServer {
        r#type: string("type").ok()?,
        command: string("command").ok()?,
        args,
        env: string_map("env").ok()?,
        cwd: string("cwd").ok()?,
        url: string("url").ok()?,
        headers: string_map("headers").ok()?,
        client_id: obj
            .get("oauth")
            .and_then(|o| o.get("clientId"))
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
    })
}

/// One server found in one of Claude Code's files, with where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub name: String,
    pub server: ClaudeServer,
    /// Human-readable origin, for the report: a scope is why two entries disagree.
    pub source: String,
}

/// What one name's sync did. `reason` is filled for `skipped` and `failed`.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncResult {
    pub name: String,
    pub source: String,
    pub action: &'static str,
    /// True when the entry carries the keychain reference.
    pub authed: bool,
    /// Claude Code's name for it, when bough had to rename it to a valid slug.
    pub renamed_from: Option<String>,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct SyncArgs {
    /// Directories whose project-scope and `.mcp.json` entries are included.
    pub dirs: Vec<String>,
    pub force: bool,
    pub dry_run: bool,
    pub help: bool,
    /// Installed plugins' own servers. On by default; `--no-plugins` opts out.
    pub plugins: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncParse {
    Args(SyncArgs),
    Usage(String),
}

/// Pure, total, and it never throws — the same contract `parse_exec_args` holds.
pub fn parse_sync_args(argv: &[String]) -> SyncParse {
    let mut args = SyncArgs {
        dirs: vec![],
        force: false,
        dry_run: false,
        help: false,
        plugins: true,
    };
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "-h" | "--help" => args.help = true,
            "--force" => args.force = true,
            "--dry-run" | "-n" => args.dry_run = true,
            "--no-plugins" => args.plugins = false,
            "--plugins" => args.plugins = true,
            "--from" | "-C" => {
                let Some(dir) = argv.get(i + 1).filter(|d| !d.is_empty()) else {
                    return SyncParse::Usage(format!("{a} needs a directory"));
                };
                args.dirs.push(dir.clone());
                i += 1;
            }
            _ if a.starts_with('-') => return SyncParse::Usage(format!("unknown flag {a}")),
            // A typo'd flag looks like it worked, otherwise.
            _ => {
                return SyncParse::Usage(format!(
                    "unexpected argument \"{a}\" — sync-mcp takes flags only"
                ))
            }
        }
        i += 1;
    }
    SyncParse::Args(args)
}

pub const USAGE: &str = "usage: bough sync-mcp [--from DIR]... [--dry-run] [--force] [--no-plugins]

  Adopt Claude Code's MCP servers into bough's registry. Existing entries are
  kept unless --force is given. Four sources are read:

    ~/.claude.json and $CLAUDE_CONFIG_DIR/.claude.json   user scope
    <dir>/.mcp.json                                      checked in
    installed plugins' .mcp.json / plugin.json           plugin servers
    Claude Code's credential store (mcpOAuth grants)     authorized remotes

  -C, --from DIR   also read that project's scope and its .mcp.json
                   (default: the current directory)
  -n, --dry-run    report what would change and write nothing
      --force      replace entries bough already has under the same name
      --no-plugins skip installed plugins' own servers

  Servers Claude Code has authorized are synced even when no config file defines
  them, which is how a Slack connector gets here. Tokens are never copied: what
  is written is a REFERENCE to the credential store (the login keychain on macOS,
  ~/.claude/.credentials.json elsewhere), resolved at connect time. Registering
  grants nothing — activate a server in the /mcp panel before a turn can use it.";

/// Reads one JSON file. `Ok(None)` for absent; `Err` only on unreadable/malformed.
pub type ReadJson = Arc<dyn Fn(&Path) -> Result<Option<Value>, String> + Send + Sync>;

pub fn real_read_json() -> ReadJson {
    Arc::new(|path: &Path| {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            // Absent is the normal case — most directories have no `.mcp.json` — and
            // it is not news. Anything else is worth saying out loud.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        serde_json::from_str::<Value>(&text)
            .map(Some)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))
    })
}

/// `mcpServers` out of an arbitrary blob, ignoring everything malformed in it.
fn servers_in(blob: &Value) -> Map<String, Value> {
    blob.get("mcpServers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default()
}

/// Every server Claude Code would offer, later sources winning on name.
///
/// TWO CANDIDATES FOR THE USER-SCOPE FILE, and the second one is why this command
/// reported "nothing to sync" on a machine with servers configured. `~/.claude.json`
/// is where Claude Code used to keep it; a current install keeps it inside the config
/// directory. Both are read, the config-directory one last because when it exists it
/// is the live one.
pub fn collect_claude_servers(
    dirs: &[String],
    read_json: &ReadJson,
    home: &Path,
    config_dir: &Path,
) -> (Vec<Found>, Vec<String>) {
    let mut errors: Vec<String> = Vec::new();
    // Insertion-ordered by name so "later wins" replaces in place, as the TS Map does.
    let mut by_name: Vec<Found> = Vec::new();

    let read = |path: &Path, errors: &mut Vec<String>| -> Option<Value> {
        match read_json(path) {
            Ok(v) => v,
            Err(e) => {
                errors.push(e);
                None
            }
        }
    };

    // Labelled by the path, not by a fixed string: a person looking at two entries
    // that disagree needs to know WHICH of the two files won.
    let label = |path: &Path| -> String {
        let p = path.display().to_string();
        let h = home.display().to_string();
        if p.starts_with(&h) {
            format!("~{}", &p[h.len()..])
        } else {
            p
        }
    };

    let candidates = [home.join(".claude.json"), config_dir.join(".claude.json")];
    let mut docs: Vec<(Value, String)> = Vec::new();
    for path in &candidates {
        if let Some(doc) = read(path, &mut errors) {
            docs.push((doc, label(path)));
        }
    }

    fn take(
        into: &mut Vec<Found>,
        errors: &mut Vec<String>,
        raw: &Map<String, Value>,
        source: &str,
    ) {
        for (name, value) in raw {
            let Some(parsed) = parse_claude_server(value) else {
                errors.push(format!(
                    "{source}: {name} is not a server definition — skipped"
                ));
                continue;
            };
            let found = Found {
                name: name.clone(),
                server: parsed,
                source: source.to_string(),
            };
            match into.iter_mut().find(|f| &f.name == name) {
                Some(slot) => *slot = found,
                None => into.push(found),
            }
        }
    }

    for (doc, source) in &docs {
        take(&mut by_name, &mut errors, &servers_in(doc), source);
    }
    for dir in dirs {
        for (doc, source) in &docs {
            if let Some(project) = doc.get("projects").and_then(|p| p.get(dir)) {
                take(
                    &mut by_name,
                    &mut errors,
                    &servers_in(project),
                    &format!("{source} projects[{dir}]"),
                );
            }
        }
        let local_path = PathBuf::from(dir).join(".mcp.json");
        if let Some(local) = read(&local_path, &mut errors) {
            let source = local_path.display().to_string();
            take(&mut by_name, &mut errors, &servers_in(&local), &source);
        }
    }
    (by_name, errors)
}

// ---------------------------------------------------------------------------
// Installed plugins
// ---------------------------------------------------------------------------

/// Every MCP server an INSTALLED Claude Code plugin defines.
///
/// WHY THIS IS A SEPARATE SOURCE. A plugin's servers are not in `~/.claude.json` and
/// not in any `.mcp.json` under a project. They live inside the plugin's own install
/// directory, which is how Slack, chrome-devtools and claude-mem can all be working in
/// Claude Code while every file this command used to read is empty.
///
/// INSTALLED, not merely available. The marketplace cache holds a `.mcp.json` for
/// every plugin ever indexed, and adopting those would fill the registry with servers
/// the user never chose.
///
/// Names are `plugin:<plugin>:<server>`, Claude Code's own namespacing, kept verbatim
/// because that is the key its OAuth grants are stored under (`plugin:slack:slack`)
/// and matching them is the entire point.
pub fn collect_plugin_servers(
    config_dir: &Path,
    dirs: &[String],
    read_json: &ReadJson,
) -> (Vec<Found>, Vec<String>) {
    let mut errors: Vec<String> = Vec::new();
    let mut found: Vec<Found> = Vec::new();
    let read = |path: &Path, errors: &mut Vec<String>| -> Option<Value> {
        match read_json(path) {
            Ok(v) => v,
            Err(e) => {
                errors.push(e);
                None
            }
        }
    };

    let registry_path = config_dir.join("plugins").join("installed_plugins.json");
    let Some(registry) = read(&registry_path, &mut errors) else {
        return (found, errors);
    };
    let Some(plugins) = registry.get("plugins").and_then(|p| p.as_object()).cloned() else {
        return (found, errors);
    };

    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for (key, raw) in &plugins {
        // `<plugin>@<marketplace>`; the marketplace is not part of the server's name.
        let fallback_name = key.split('@').next().unwrap_or(key).to_string();
        let installs: Vec<Value> = match raw {
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        for raw_install in installs {
            let Some(install_path) = raw_install
                .get("installPath")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let scope = raw_install.get("scope").and_then(|v| v.as_str());
            let project_path = raw_install.get("projectPath").and_then(|v| v.as_str());
            // A project-scoped install is only taken when its project is one of
            // `dirs`, the same rule Claude Code applies.
            if scope == Some("project")
                && !project_path.is_some_and(|p| dirs.iter().any(|d| d == p))
            {
                continue;
            }
            let install_dir = PathBuf::from(install_path);

            let manifest = read(
                &install_dir.join(".claude-plugin").join("plugin.json"),
                &mut errors,
            )
            .unwrap_or(json!({}));
            let plugin_name = manifest
                .get("name")
                .and_then(|n| n.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| fallback_name.clone());

            // Two shapes in the wild, and both are live: chrome-devtools declares its
            // server in the manifest, slack and claude-mem in a `.mcp.json` beside it.
            // The file wins, being the more specific of the two.
            let mut declared: Map<String, Value> = manifest
                .get("mcpServers")
                .and_then(|m| m.as_object())
                .cloned()
                .unwrap_or_default();
            let from_file = read(&install_dir.join(".mcp.json"), &mut errors)
                .map(|b| plugin_servers_in(&b))
                .unwrap_or_default();
            for (k, v) in from_file {
                declared.insert(k, v);
            }

            for (server_name, value) in &declared {
                let name = format!("plugin:{plugin_name}:{server_name}");
                // First install wins. A plugin present at two versions or two scopes
                // is one server, and the alternative is the same endpoint twice.
                if claimed.contains(&name) {
                    continue;
                }
                let Some(parsed) = parse_claude_server(value) else {
                    errors.push(format!(
                        "plugin {plugin_name}: {server_name} is not a server definition, skipped"
                    ));
                    continue;
                };
                claimed.insert(name.clone());
                found.push(Found {
                    name,
                    server: expand_plugin_root(&parsed, install_path),
                    source: format!("plugin {plugin_name}"),
                });
            }
        }
    }
    (found, errors)
}

/// A plugin's server map, out of either shape a `.mcp.json` comes in.
///
/// `{ "mcpServers": { … } }` is the documented one; a bare `{ "<name>": { … } }` map
/// is what several official plugins actually ship (terraform, linear, github), and
/// reading only the wrapper form silently found nothing in them.
fn plugin_servers_in(blob: &Value) -> Map<String, Value> {
    match blob.get("mcpServers").and_then(|v| v.as_object()) {
        Some(wrapped) => wrapped.clone(),
        None => blob.as_object().cloned().unwrap_or_default(),
    }
}

/// `${CLAUDE_PLUGIN_ROOT}` resolved to the directory the plugin is installed in.
///
/// Claude Code sets that variable when it spawns a plugin's server, so a definition
/// carrying it is complete THERE and broken here. Substituted at sync time rather than
/// left as a bough `${VAR}` reference because the value is not a secret and not a
/// setting: it is where this install happens to be.
fn expand_plugin_root(s: &ClaudeServer, install_path: &str) -> ClaudeServer {
    let sub = |v: &String| v.replace("${CLAUDE_PLUGIN_ROOT}", install_path);
    ClaudeServer {
        r#type: s.r#type.clone(),
        command: s.command.as_ref().map(&sub),
        args: s.args.as_ref().map(|a| a.iter().map(&sub).collect()),
        env: s
            .env
            .as_ref()
            .map(|e| e.iter().map(|(k, v)| (k.clone(), sub(v))).collect()),
        cwd: s.cwd.as_ref().map(&sub),
        url: s.url.as_ref().map(&sub),
        headers: s.headers.clone(),
        client_id: s.client_id.clone(),
    }
}

// ---------------------------------------------------------------------------
// Mapping
// ---------------------------------------------------------------------------

pub enum Mapped {
    Server { server: Value, authed: bool },
    Refused(String),
}

/// One Claude Code entry as a bough registry entry, or a reason it cannot be.
///
/// The two transports are kept strictly apart because bough's schema refuses a
/// mixture — a remote server with `args` is rejected as "describes a subprocess and
/// there is none" — and a config in the wild may carry both keys.
pub fn to_bough_server(s: &ClaudeServer, grant: Option<&KeychainGrant>) -> Mapped {
    let remote = s.url.is_some()
        && (s.command.is_none()
            || s.r#type.as_deref() == Some("http")
            || s.r#type.as_deref() == Some("sse"));
    if remote {
        let url = s.url.clone().unwrap_or_default();
        let mut headers: BTreeMap<String, String> = s.headers.clone().unwrap_or_default();
        let has_auth = headers
            .keys()
            .any(|h| h.eq_ignore_ascii_case("authorization"));
        // Carried so the entry stays usable AFTER the adopted grant expires: without
        // it, reauthorizing a provider that has no dynamic registration is impossible.
        let client = s.client_id.clone();
        // THE SERVER'S OWN GRANT FIRST. When Claude Code has authorized this server,
        // the right credential is the one it obtained FOR it — not the account token,
        // which that server would reject anyway. This is what makes Slack work.
        if !has_auth {
            if let Some(g) = grant {
                headers.insert(
                    "Authorization".into(),
                    format!("Bearer {}", grant_ref(&g.key)),
                );
                return Mapped::Server {
                    server: remote_entry(&url, &headers, &client),
                    authed: true,
                };
            }
        }
        // Otherwise the account token, and only to hosts it belongs to.
        let authed = !has_auth && is_anthropic_host(&url);
        if authed {
            headers.insert(
                "Authorization".into(),
                format!("Bearer {}", claude_token_ref()),
            );
        }
        return Mapped::Server {
            server: remote_entry(&url, &headers, &client),
            authed,
        };
    }
    let Some(command) = s.command.clone() else {
        return Mapped::Refused("has neither a `command` nor a `url` bough can use".into());
    };
    let mut entry = Map::new();
    entry.insert("command".into(), json!(command));
    entry.insert("args".into(), json!(s.args.clone().unwrap_or_default()));
    entry.insert("env".into(), json!(s.env.clone().unwrap_or_default()));
    if let Some(cwd) = &s.cwd {
        entry.insert("cwd".into(), json!(cwd));
    }
    Mapped::Server {
        server: Value::Object(entry),
        authed: false,
    }
}

fn remote_entry(
    url: &str,
    headers: &BTreeMap<String, String>,
    client_id: &Option<String>,
) -> Value {
    let mut entry = Map::new();
    entry.insert("url".into(), json!(url));
    entry.insert("headers".into(), json!(headers));
    if let Some(c) = client_id {
        entry.insert("clientId".into(), json!(c));
    }
    Value::Object(entry)
}

/// A name bough's registry will accept, derived from whatever Claude Code called it.
///
/// Claude Code namespaces a plugin's server — the Slack connector arrives as
/// `plugin:slack:slack` — and bough's registry takes lowercase slugs. Renaming is the
/// right answer rather than loosening the registry: the name is what a person types in
/// `/mcp` and what a skill's `mcp:` frontmatter names.
///
/// The LAST segment is preferred (`plugin:slack:slack` → `slack`). When that is taken
/// by something else, or is not a usable slug on its own, the whole name is slugified
/// instead (`plugin-slack-slack`) — ugly, but unambiguous, and reported either way.
pub fn bough_name(raw: &str, taken: &BTreeSet<String>) -> Option<String> {
    fn valid(s: &str) -> bool {
        let mut c = s.chars();
        match c.next() {
            Some(f) if f.is_ascii_lowercase() || f.is_ascii_digit() => {}
            _ => return false,
        }
        c.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    }
    fn slug(s: &str) -> String {
        let lowered = s.to_lowercase();
        let mut out = String::with_capacity(lowered.len());
        let mut pending_dash = false;
        for ch in lowered.chars() {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-' {
                if pending_dash && !out.is_empty() {
                    out.push('-');
                }
                pending_dash = false;
                out.push(ch);
            } else {
                pending_dash = true;
            }
        }
        out.trim_matches('-').to_string()
    }
    if valid(raw) {
        return Some(raw.to_string());
    }
    let last = slug(raw.rsplit(':').next().unwrap_or(""));
    if valid(&last) && !taken.contains(&last) {
        return Some(last);
    }
    let whole = slug(raw);
    if valid(&whole) && !taken.contains(&whole) {
        return Some(whole);
    }
    None
}

/// An env value that looks like a pasted credential rather than a setting.
///
/// A heuristic, and it is allowed to be: it drives a WARNING, never a refusal. The
/// point is that `bough sync-mcp` can move a literal secret out of one file and into
/// one that is served over HTTP, and the person doing it should hear about it once
/// rather than discover it in a response body.
pub fn looks_secret(key: &str, value: &str) -> bool {
    // Already a reference.
    if value.starts_with("${") && value.ends_with('}') && !value[2..].contains('{') {
        return false;
    }
    let k = key.to_lowercase();
    let named = ["token", "secret", "key", "password", "passwd", "credential"]
        .iter()
        .any(|n| k.contains(n));
    named && value.len() >= 12
}

/// The entry already carries a credential of its own.
fn has_auth_header(entry: &ServerConfig) -> bool {
    entry
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("authorization") && !v.trim().is_empty())
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SyncDeps {
    pub read_json: Option<ReadJson>,
    /// Injected so tests never touch a real credential store.
    pub keychain: Option<KeychainReader>,
    /// Injected so tests write to a temp file rather than `~/.bough`.
    pub config: McpConfigOptions,
    pub home: Option<PathBuf>,
    /// Claude Code's config directory. Absent = `CLAUDE_CONFIG_DIR`, else
    /// `<home>/.claude`.
    pub config_dir: Option<PathBuf>,
    pub cwd: Option<String>,
    pub out: Arc<dyn Fn(&str) + Send + Sync>,
    pub err: Arc<dyn Fn(&str) + Send + Sync>,
    /// Injected clock, epoch ms — the staleness assertions need no sleeping.
    pub now: Option<i64>,
}

impl Default for SyncDeps {
    fn default() -> Self {
        SyncDeps {
            read_json: None,
            keychain: None,
            config: McpConfigOptions::default(),
            home: None,
            config_dir: None,
            cwd: None,
            out: Arc::new(|l| println!("{l}")),
            err: Arc::new(|l| eprintln!("{l}")),
            now: None,
        }
    }
}

/// Trailing slashes are not an identity.
fn norm(u: &str) -> String {
    u.trim_end_matches('/').to_string()
}

/// What makes two entries the same server. `None` when the entry describes neither.
fn identity_of(url: Option<&str>, command: Option<&str>, args: &[String]) -> Option<String> {
    if let Some(u) = url.filter(|u| !u.is_empty()) {
        return Some(norm(u));
    }
    command
        .filter(|c| !c.is_empty())
        .map(|c| format!("{c} {}", args.join(" ")).trim().to_string())
}

pub async fn run_sync_mcp(argv: &[String], deps: &SyncDeps) -> i32 {
    let out = &deps.out;
    let err = &deps.err;
    let read_json = deps.read_json.clone().unwrap_or_else(real_read_json);
    let home = deps
        .home
        .clone()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    let now = deps.now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    });

    let args = match parse_sync_args(argv) {
        SyncParse::Usage(message) => {
            err(&format!("error: {message}"));
            err(USAGE);
            return 2;
        }
        SyncParse::Args(a) => a,
    };
    if args.help {
        out(USAGE);
        return 0;
    }
    let dirs: Vec<String> = if args.dirs.is_empty() {
        vec![deps.cwd.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        })]
    } else {
        args.dirs.clone()
    };
    let config_dir = deps
        .config_dir
        .clone()
        .unwrap_or_else(|| claude_config_dir_from(&|n| std::env::var(n).ok(), &home));

    let (mut found, mut errors) = collect_claude_servers(&dirs, &read_json, &home, &config_dir);
    if args.plugins {
        // AFTER the config files, so a user's own entry under the same name still
        // wins: a plugin's definition is the default, and someone who has written
        // their own is saying they want theirs.
        let (plugin_found, plugin_errors) = collect_plugin_servers(&config_dir, &dirs, &read_json);
        errors.extend(plugin_errors);
        let claimed_by_config: BTreeSet<String> = found.iter().map(|f| f.name.clone()).collect();
        for p in plugin_found {
            if !claimed_by_config.contains(&p.name) {
                found.push(p);
            }
        }
    }
    // NOT the default reader: the grants live in whichever store has an `mcpOAuth`
    // map, and on a machine where Claude Code left `claudeAiOauth` in the keychain and
    // moved the grants to `.credentials.json` those are different stores.
    let reader = deps
        .keychain
        .clone()
        .unwrap_or_else(|| credential_reader_for(Arc::new(holds_grants)));
    let (grants, note) = read_grants(&reader).await;
    if let Some(note) = note {
        err(&format!("warning: {note}"));
    }
    // SAY WHAT WAS ADOPTED IN WHAT CONDITION. A reference is written for a grant
    // whether or not the grant currently works, and that is the right behaviour. What
    // was wrong was doing it SILENTLY: an adopted-but-dead grant surfaced later, one
    // server at a time, as a connect error in a panel.
    for g in grants.iter().filter(|g| g.empty) {
        err(&format!(
            "warning: Claude Code's grant for \"{}\" holds no token — the entry exists but \
             is empty, so its reference cannot resolve. Re-authorize it in Claude Code, or \
             remove the server from bough's registry.",
            g.name
        ));
    }
    let stale: Vec<&KeychainGrant> = grants
        .iter()
        .filter(|g| !g.empty && is_stale(g, now))
        .collect();
    if !stale.is_empty() {
        let names = stale
            .iter()
            .map(|g| format!("\"{}\"", g.name))
            .collect::<Vec<_>>()
            .join(", ");
        let one = stale.len() == 1;
        err(&format!(
            "note: {names} {} already expired. Adopted anyway — Claude Code refreshes its \
             own tokens, so using {} there makes {} work here. bough does not refresh them.",
            if one {
                "has a grant that is"
            } else {
                "have grants that are"
            },
            if one { "that server" } else { "those servers" },
            if one { "it" } else { "them" },
        ));
    }
    // Matched by NAME first, then by URL: the name is what Claude Code keys the grant
    // under, and the URL catches the case where the two disagree about spelling but
    // plainly mean the same endpoint.
    let same_url = |a: &str, b: &str| norm(a) == norm(b);
    let grant_for = |name: &str, url: Option<&str>| -> Option<KeychainGrant> {
        grants
            .iter()
            .find(|g| g.name == name)
            .or_else(|| url.and_then(|u| grants.iter().find(|g| same_url(&g.url, u))))
            .cloned()
    };

    // A grant with no definition anywhere is STILL a server — and it is the whole
    // reason Slack could not be synced before.
    let claimed: BTreeSet<String> = found.iter().map(|f| f.name.clone()).collect();
    for g in &grants {
        if claimed.contains(&g.name)
            || found
                .iter()
                .any(|f| f.server.url.as_deref().is_some_and(|u| same_url(u, &g.url)))
        {
            continue;
        }
        found.push(Found {
            name: g.name.clone(),
            server: ClaudeServer {
                r#type: Some("http".into()),
                url: Some(g.url.clone()),
                ..Default::default()
            },
            source: "Claude Code's keychain grants".into(),
        });
    }
    for e in &errors {
        err(&format!("warning: {e}"));
    }
    if found.is_empty() {
        out("no MCP servers found in Claude Code's config — nothing to sync.");
        // Not a failure: a person with no servers configured ran a command and got a
        // true answer. The exit code is for a source that could not be READ.
        return i32::from(!errors.is_empty());
    }

    let existing = load_registry(&deps.config).servers;
    let mut results: Vec<SyncResult> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Names claimed so far, so a rename cannot land on top of another server.
    let mut taken: BTreeSet<String> = existing.keys().cloned().collect();
    taken.extend(found.iter().map(|f| f.name.clone()));

    // AN ENDPOINT IS A SERVER, whatever either tool calls it. Without this, adopting a
    // server bough already had under a different name minted a SECOND entry beside it.
    // A SUBPROCESS HAS AN IDENTITY TOO, and leaving it out made this command
    // non-idempotent the moment plugin servers arrived: every plugin server needs a
    // rename, so a second run found the first run's name "taken" and added
    // `plugin-claude-mem-mcp-search` beside `mcp-search`. Running a sync twice must be
    // the same as running it once.
    let mut by_identity: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (n, cfg) in &existing {
        if let Some(id) = identity_of(cfg.url.as_deref(), cfg.command.as_deref(), &cfg.args) {
            by_identity.entry(id).or_default().push(n.clone());
        }
    }
    // A duplicate that is ALREADY there is not this run's doing and not this command's
    // to delete — but silence about it is how it survives.
    for (id, names) in &by_identity {
        if names.len() > 1 {
            warnings.push(format!(
                "{} are the same server ({id}). Only one is needed. Open /mcp and press F \
                 on the one you do not want.",
                names.join(" and ")
            ));
        }
    }

    for Found {
        name: claude_name,
        server,
        source,
    } in &found
    {
        // THE ENDPOINT DECIDES FIRST, and it has to: `bough_name` refuses a name that
        // is taken, and when the taker IS this same server the refusal renames a
        // server into a duplicate of itself. Among entries sharing the URL, the one a
        // person would have named wins (`slack`, not `plugin-slack-slack`).
        let id = identity_of(
            server.url.as_deref(),
            server.command.as_deref(),
            server.args.as_deref().unwrap_or(&[]),
        );
        let same_names: Vec<String> = id
            .as_ref()
            .and_then(|i| by_identity.get(i))
            .cloned()
            .unwrap_or_default();
        let natural = bough_name(claude_name, &BTreeSet::new());
        let same_endpoint = match &natural {
            Some(n) if same_names.contains(n) => Some(n.clone()),
            _ => same_names.first().cloned(),
        };
        let Some(name) = same_endpoint.or_else(|| bough_name(claude_name, &taken)) else {
            results.push(SyncResult {
                name: claude_name.clone(),
                source: source.clone(),
                action: "failed",
                authed: false,
                renamed_from: None,
                reason: Some(format!(
                    "no free name could be derived from \"{claude_name}\""
                )),
            });
            continue;
        };
        if &name != claude_name {
            taken.insert(name.clone());
        }
        let already = existing.get(&name);
        if let Some(already) = already {
            if !args.force {
                // ONE THING IS STILL WORTH DOING TO AN ENTRY WE ARE NOT REPLACING:
                // giving it the credential it is missing. Adding a header where there
                // was none is not the clobber `--force` guards against — nothing is
                // overwritten, and every other field is left exactly as found.
                let grant = grant_for(claude_name, server.url.as_deref());
                if let Some(g) = &grant {
                    if already.url.is_some() && !has_auth_header(already) {
                        let mut entry = serde_json::to_value(already).unwrap_or(json!({}));
                        let mut headers = already.headers.clone();
                        headers.insert(
                            "Authorization".into(),
                            format!("Bearer {}", grant_ref(&g.key)),
                        );
                        entry["headers"] = json!(headers);
                        if !args.dry_run {
                            let _ = upsert_server(&name, &entry, &deps.config);
                        }
                        results.push(SyncResult {
                            name: name.clone(),
                            source: source.clone(),
                            action: "updated",
                            authed: true,
                            renamed_from: None,
                            reason: Some("added the missing credential".into()),
                        });
                        continue;
                    }
                }
                let renamed = same_names.contains(&name) && &name != claude_name;
                results.push(SyncResult {
                    name: name.clone(),
                    source: source.clone(),
                    action: "skipped",
                    authed: false,
                    renamed_from: None,
                    reason: Some(if renamed {
                        format!("already registered as \"{name}\", the same server")
                    } else {
                        "already registered here — --force replaces it".into()
                    }),
                });
                continue;
            }
        }
        // Matched on what CLAUDE CODE calls it — the grant is keyed by that name, and
        // the rename above is bough's business, not the keychain's.
        let grant = grant_for(claude_name, server.url.as_deref());
        let mapped = to_bough_server(server, grant.as_ref());
        let (entry, authed) = match mapped {
            Mapped::Refused(reason) => {
                results.push(SyncResult {
                    name: name.clone(),
                    source: source.clone(),
                    action: "failed",
                    authed: false,
                    renamed_from: None,
                    reason: Some(reason),
                });
                continue;
            }
            Mapped::Server { server, authed } => (server, authed),
        };
        // Said at sync time as well as at connect time. `keychain.rs` refuses an
        // expired token when the request is about to go out, which is correct but
        // arrives much later and looks like the server's fault.
        if let Some(g) = &grant {
            if g.expires_at.is_some_and(|at| at <= now) {
                warnings.push(format!(
                    "{name}: Claude Code's grant for this server expired {}. bough does not \
                     refresh a credential it did not obtain — run `claude` once to refresh \
                     it in place.",
                    iso_ms(g.expires_at.unwrap_or(0))
                ));
            }
        }
        for (k, v) in server.env.as_ref().unwrap_or(&BTreeMap::new()) {
            if looks_secret(k, v) {
                warnings.push(format!(
                    "{name}: env {k} looks like a literal secret. bough's registry is served \
                     by GET /mcp/servers — prefer ${{{k}}} and put the value in ~/.bough/env."
                ));
            }
        }
        let action = if existing.contains_key(&name) {
            "updated"
        } else {
            "added"
        };
        if !args.dry_run {
            if let Err(e) = upsert_server(&name, &entry, &deps.config) {
                results.push(SyncResult {
                    name: name.clone(),
                    source: source.clone(),
                    action: "failed",
                    authed: false,
                    renamed_from: None,
                    reason: Some(e.to_string()),
                });
                continue;
            }
        }
        results.push(SyncResult {
            name: name.clone(),
            source: source.clone(),
            action,
            authed,
            // A rename is not a detail to swallow: this name is what you type in
            // `/mcp` and what a skill's `mcp:` frontmatter has to say.
            renamed_from: (&name != claude_name).then(|| claude_name.clone()),
            reason: None,
        });
    }

    for r in &results {
        let mark = if r.action == "added" || r.action == "updated" {
            "✓"
        } else {
            "·"
        };
        let note = match &r.reason {
            Some(reason) => format!(" — {reason}"),
            None if r.authed => " (using the token Claude Code already holds)".to_string(),
            None => String::new(),
        };
        let renamed = match &r.renamed_from {
            Some(from) => format!(" (renamed from {from})"),
            None => String::new(),
        };
        out(&format!(
            "{mark} {}{renamed}  {}{note}   ({})",
            r.name, r.action, r.source
        ));
    }
    for w in &warnings {
        err(&format!("warning: {w}"));
    }

    let wrote = results
        .iter()
        .filter(|r| r.action == "added" || r.action == "updated")
        .count();
    if args.dry_run {
        out(&format!(
            "\n--dry-run: {wrote} entr{} would change, nothing written.",
            if wrote == 1 { "y" } else { "ies" }
        ));
        return 0;
    }
    if wrote > 0 {
        // Said every time, because this is the step whose absence looks like a bug:
        // the servers are registered, the agent still cannot see them, and nothing
        // else on the path says why.
        out(&format!(
            "\n{wrote} server{} registered. Registering grants nothing — open the /mcp panel \
             and enable the ones a turn should be able to use.",
            if wrote == 1 { "" } else { "s" }
        ));
    }
    i32::from(results.iter().any(|r| r.action == "failed") || !errors.is_empty())
}

fn iso_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| format!("{ms}"))
}

#[cfg(test)]
mod tests {
    //! Nothing here reads `~/.claude.json`, `~/.bough` or the login keychain: the
    //! JSON reads and the registry file are both injected, which is what lets the
    //! security claims below be tested at all — "the Anthropic token is not sent to a
    //! stranger" is a statement about what gets WRITTEN, and this asserts on the
    //! written file.

    use super::*;
    use bough_core::mcp::keychain::{reader_fn, KeychainResult};
    use std::sync::Mutex;

    const HOME: &str = "/home/t";
    const CONFIG_DIR: &str = "/home/t/.claude";

    /// A fake filesystem of JSON documents, keyed by absolute path.
    fn reader(files: Vec<(String, Value)>) -> ReadJson {
        Arc::new(move |path: &Path| {
            let key = path.display().to_string();
            Ok(files
                .iter()
                .find(|(p, _)| *p == key)
                .map(|(_, v)| v.clone()))
        })
    }

    fn no_files() -> ReadJson {
        Arc::new(|_| Ok(None))
    }

    fn registry_file() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bough-syncmcp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("mcp.json")
    }

    /// `security` reporting "no such item" (44).
    ///
    /// The DEFAULT for every test that is not about grants, and not an incidental
    /// one: without it the command falls through to the real store and a test run
    /// reads the developer's login keychain — which can raise the system's "allow
    /// access?" dialog and hang a suite on a machine nobody is watching.
    fn no_keychain() -> KeychainReader {
        reader_fn(|_| async { KeychainResult::miss(44, "", None) })
    }

    /// A `Claude Code-credentials` item, shaped like the real one.
    fn keychain(mcp_oauth: Value) -> KeychainReader {
        let value = json!({
            "mcpOAuth": mcp_oauth,
            "claudeAiOauth": { "accessToken": "account-token", "expiresAt": 4_000_000_000_000i64 },
        })
        .to_string();
        reader_fn(move |_| {
            let value = value.clone();
            async move { KeychainResult::ok(value, None) }
        })
    }

    fn slack_grant() -> Value {
        json!({
            "slack|a1b2c3": {
                "serverName": "slack",
                "serverUrl": "https://slack.example.com/mcp",
                "accessToken": "slack-secret-token",
                "refreshToken": "slack-refresh",
                "redirectUri": "http://localhost:1/callback",
                "expiresAt": 4_000_000_000_000i64,
                "scope": "read",
            }
        })
    }

    struct Run {
        deps: SyncDeps,
        lines: Arc<Mutex<String>>,
        file: PathBuf,
    }

    impl Run {
        fn new(read_json: ReadJson, keychain: KeychainReader, file: PathBuf) -> Run {
            let lines = Arc::new(Mutex::new(String::new()));
            let (o, e) = (lines.clone(), lines.clone());
            Run {
                deps: SyncDeps {
                    read_json: Some(read_json),
                    keychain: Some(keychain),
                    config: McpConfigOptions::with_file(&file),
                    home: Some(PathBuf::from(HOME)),
                    config_dir: Some(PathBuf::from(CONFIG_DIR)),
                    cwd: Some("/w".into()),
                    out: Arc::new(move |l| {
                        let mut s = o.lock().unwrap();
                        s.push_str(l);
                        s.push('\n');
                    }),
                    err: Arc::new(move |l| {
                        let mut s = e.lock().unwrap();
                        s.push_str(l);
                        s.push('\n');
                    }),
                    // A fixed clock, so "expired" is a property of the fixture and
                    // never of how long the suite took.
                    now: Some(1_700_000_000_000),
                },
                lines,
                file,
            }
        }
        fn text(&self) -> String {
            self.lines.lock().unwrap().clone()
        }
        fn registry(&self) -> Value {
            let raw = std::fs::read_to_string(&self.file).unwrap_or_else(|_| "{}".into());
            serde_json::from_str(&raw).unwrap()
        }
        fn raw(&self) -> String {
            std::fs::read_to_string(&self.file).unwrap_or_default()
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    // ---- parsing -----------------------------------------------------------

    #[test]
    fn parse_sync_args_is_flags_only_and_never_throws() {
        assert_eq!(
            parse_sync_args(&[]),
            SyncParse::Args(SyncArgs {
                dirs: vec![],
                force: false,
                dry_run: false,
                help: false,
                plugins: true
            })
        );
        match parse_sync_args(&argv(&[
            "--from",
            "/a",
            "-C",
            "/b",
            "-n",
            "--force",
            "--no-plugins",
        ])) {
            SyncParse::Args(a) => {
                assert_eq!(a.dirs, vec!["/a".to_string(), "/b".to_string()]);
                assert!(a.force && a.dry_run && !a.plugins);
            }
            other => panic!("{other:?}"),
        }
        // A typo'd flag looks like it worked, otherwise.
        match parse_sync_args(&argv(&["notion"])) {
            SyncParse::Usage(m) => assert!(m.contains("sync-mcp takes flags only"), "{m}"),
            other => panic!("{other:?}"),
        }
        match parse_sync_args(&argv(&["--nope"])) {
            SyncParse::Usage(m) => assert!(m.contains("unknown flag --nope"), "{m}"),
            other => panic!("{other:?}"),
        }
        match parse_sync_args(&argv(&["--from"])) {
            SyncParse::Usage(m) => assert!(m.contains("needs a directory"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    // ---- collection --------------------------------------------------------

    #[test]
    fn collect_user_scope_project_scope_and_a_checked_in_mcp_json_later_wins() {
        let files = reader(vec![
            (
                format!("{HOME}/.claude.json"),
                json!({
                    "mcpServers": { "a": { "command": "user" } },
                    "projects": { "/w": { "mcpServers": { "a": { "command": "project" } } } },
                }),
            ),
            (
                "/w/.mcp.json".into(),
                json!({ "mcpServers": { "a": { "command": "checked-in" } } }),
            ),
        ]);
        let (found, errors) = collect_claude_servers(
            &["/w".to_string()],
            &files,
            Path::new(HOME),
            Path::new(CONFIG_DIR),
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].server.command.as_deref(), Some("checked-in"));
        assert_eq!(found[0].source, "/w/.mcp.json");
    }

    #[test]
    fn collect_the_config_directorys_claude_json_is_read_and_wins() {
        // A current install keeps the user-scope file INSIDE the config directory.
        // Reading only `~/.claude.json` took an ENOENT for an empty configuration.
        let files = reader(vec![
            (
                format!("{HOME}/.claude.json"),
                json!({ "mcpServers": { "a": { "command": "old" } } }),
            ),
            (
                format!("{CONFIG_DIR}/.claude.json"),
                json!({ "mcpServers": { "a": { "command": "current" } } }),
            ),
        ]);
        let (found, _) = collect_claude_servers(
            &["/w".to_string()],
            &files,
            Path::new(HOME),
            Path::new(CONFIG_DIR),
        );
        assert_eq!(found[0].server.command.as_deref(), Some("current"));
        // Labelled by the path, with home shortened: a person looking at two entries
        // that disagree needs to know WHICH of the two files won.
        assert_eq!(found[0].source, "~/.claude/.claude.json");
    }

    /// Where a plugin lands when Claude Code installs it.
    const SLACK_ROOT: &str = "/home/t/.claude/plugins/cache/official/slack/1.1.0";
    const CDT_ROOT: &str = "/home/t/.claude/plugins/cache/cdt-plugins/chrome-devtools-mcp/1.4.0";

    /// `installed_plugins.json` plus the two definition shapes that exist in the wild.
    fn plugin_files() -> Vec<(String, Value)> {
        vec![
            (
                format!("{CONFIG_DIR}/plugins/installed_plugins.json"),
                json!({ "version": 2, "plugins": {
                    "slack@official": [{ "scope": "user", "installPath": SLACK_ROOT, "version": "1.1.0" }],
                    "chrome-devtools-mcp@cdt-plugins": [{ "scope": "user", "installPath": CDT_ROOT }],
                }}),
            ),
            // Shape one: a `.mcp.json` beside the plugin, wrapped in `mcpServers`.
            (
                format!("{SLACK_ROOT}/.mcp.json"),
                json!({ "mcpServers": { "slack": {
                    "type": "http",
                    "url": "https://mcp.slack.com/mcp",
                    "oauth": { "clientId": "1601185624273.8899143856786", "callbackPort": 3118 },
                }}}),
            ),
            (
                format!("{SLACK_ROOT}/.claude-plugin/plugin.json"),
                json!({ "name": "slack", "version": "1.1.0" }),
            ),
            // Shape two: declared inline in the manifest, no `.mcp.json` at all.
            (
                format!("{CDT_ROOT}/.claude-plugin/plugin.json"),
                json!({ "name": "chrome-devtools-mcp", "mcpServers": {
                    "chrome-devtools": { "command": "npx", "args": ["chrome-devtools-mcp@1.4.0"] }
                }}),
            ),
        ]
    }

    #[test]
    fn collect_a_plugins_servers_are_found_in_either_shape_namespaced_as_claude_code_does() {
        // Both shapes are live on the machine that reported this, and neither is in
        // any file this command used to read. That is why it found nothing while
        // three servers worked.
        let (found, errors) = collect_plugin_servers(
            Path::new(CONFIG_DIR),
            &["/w".to_string()],
            &reader(plugin_files()),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let mut names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "plugin:chrome-devtools-mcp:chrome-devtools",
                "plugin:slack:slack"
            ]
        );
        let slack = found.iter().find(|f| f.name.ends_with(":slack")).unwrap();
        assert_eq!(
            slack.server.url.as_deref(),
            Some("https://mcp.slack.com/mcp")
        );
        // The pre-registered client is carried: Slack publishes
        // `registration_endpoint: null`, so without it the adopted entry is
        // un-reauthorizable the moment its copied grant expires.
        assert_eq!(
            slack.server.client_id.as_deref(),
            Some("1601185624273.8899143856786")
        );
    }

    #[test]
    fn collect_a_bare_server_map_is_read_too_not_just_the_mcp_servers_wrapper() {
        // terraform, linear and github ship the bare shape; reading only the wrapper
        // silently found nothing in them.
        let files = reader(vec![
            (
                format!("{CONFIG_DIR}/plugins/installed_plugins.json"),
                json!({ "plugins": { "tf@m": { "installPath": "/p" } } }),
            ),
            (
                "/p/.mcp.json".into(),
                json!({ "terraform": { "command": "tf-mcp" } }),
            ),
        ]);
        let (found, _) = collect_plugin_servers(Path::new(CONFIG_DIR), &[], &files);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "plugin:tf:terraform");
    }

    #[test]
    fn collect_claude_plugin_root_becomes_the_directory_the_plugin_is_installed_in() {
        // `bun run --cwd ${CLAUDE_PLUGIN_ROOT}` copied verbatim spawns in a directory
        // literally named that.
        let files = reader(vec![
            (
                format!("{CONFIG_DIR}/plugins/installed_plugins.json"),
                json!({ "plugins": { "mem@m": { "installPath": "/plugins/mem" } } }),
            ),
            (
                "/plugins/mem/.mcp.json".into(),
                json!({ "mcpServers": { "search": {
                    "command": "bun",
                    "args": ["run", "${CLAUDE_PLUGIN_ROOT}/index.ts"],
                    "cwd": "${CLAUDE_PLUGIN_ROOT}",
                    "env": { "ROOT": "${CLAUDE_PLUGIN_ROOT}/data" },
                }}}),
            ),
        ]);
        let (found, _) = collect_plugin_servers(Path::new(CONFIG_DIR), &[], &files);
        let s = &found[0].server;
        assert_eq!(
            s.args.as_ref().unwrap(),
            &["run".to_string(), "/plugins/mem/index.ts".into()]
        );
        assert_eq!(s.cwd.as_deref(), Some("/plugins/mem"));
        assert_eq!(s.env.as_ref().unwrap()["ROOT"], "/plugins/mem/data");
    }

    #[test]
    fn collect_only_installed_plugins_and_a_project_scoped_one_only_for_its_project() {
        // A project-scoped install is scoped to that checkout precisely so it does not
        // follow you into unrelated ones.
        let files = reader(vec![
            (
                format!("{CONFIG_DIR}/plugins/installed_plugins.json"),
                json!({ "plugins": {
                    "here@m": [{ "scope": "project", "projectPath": "/w", "installPath": "/p/here" }],
                    "elsewhere@m": [{ "scope": "project", "projectPath": "/other", "installPath": "/p/there" }],
                }}),
            ),
            (
                "/p/here/.mcp.json".into(),
                json!({ "mcpServers": { "a": { "command": "x" } } }),
            ),
            (
                "/p/there/.mcp.json".into(),
                json!({ "mcpServers": { "b": { "command": "y" } } }),
            ),
        ]);
        let (found, _) = collect_plugin_servers(Path::new(CONFIG_DIR), &["/w".to_string()], &files);
        assert_eq!(
            found.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["plugin:here:a"]
        );
    }

    // ---- what gets WRITTEN: the credential-safety suite ---------------------

    #[tokio::test]
    async fn a_stdio_server_keeps_its_command_args_env_and_cwd() {
        let run = Run::new(
            reader(vec![(
                format!("{HOME}/.claude.json"),
                json!({ "mcpServers": { "chrome-devtools": {
                    "type": "stdio",
                    "command": "npx",
                    "args": ["chrome-devtools-mcp@latest"],
                    "env": { "CHROME_PATH": "/Applications/Chrome.app" },
                }}}),
            )]),
            no_keychain(),
            registry_file(),
        );
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 0, "{}", run.text());
        assert_eq!(
            run.registry()["servers"]["chrome-devtools"],
            json!({
                "command": "npx",
                "args": ["chrome-devtools-mcp@latest"],
                "env": { "CHROME_PATH": "/Applications/Chrome.app" },
                "headers": {},
            })
        );
    }

    #[tokio::test]
    async fn a_claude_ai_server_gets_a_keychain_reference_never_a_token() {
        let run = Run::new(
            reader(vec![(
                format!("{HOME}/.claude.json"),
                json!({ "mcpServers": { "gmail": { "type": "http", "url": "https://mcp.claude.ai/gmail" } } }),
            )]),
            no_keychain(),
            registry_file(),
        );
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 0, "{}", run.text());
        assert_eq!(
            run.registry()["servers"]["gmail"]["headers"]["Authorization"],
            "Bearer ${keychain:Claude Code-credentials#claudeAiOauth.accessToken}"
        );
        // The registry is served by GET /mcp/servers and rendered in the panel, so the
        // one thing that must never appear in it is the secret itself.
        let text = run.raw();
        assert!(!text.contains("sk-"), "{text}");
        assert!(text.contains("${keychain:"), "{text}");
    }

    #[tokio::test]
    async fn a_third_party_remote_server_is_registered_without_anthropics_token() {
        // The generalization this refuses: "it is remote, so give it the bearer token"
        // would post the user's Anthropic OAuth credential to whatever host a config
        // file happens to name. A lookalike domain must not match either.
        let run = Run::new(
            reader(vec![(
                format!("{HOME}/.claude.json"),
                json!({ "mcpServers": {
                    "linear": { "type": "http", "url": "https://mcp.linear.app/sse" },
                    "lookalike": { "type": "http", "url": "https://claude.ai.evil.example/mcp" },
                }}),
            )]),
            no_keychain(),
            registry_file(),
        );
        run_sync_mcp(&[], &run.deps).await;
        let doc = run.registry();
        assert_eq!(doc["servers"]["linear"]["headers"], json!({}));
        assert_eq!(doc["servers"]["lookalike"]["headers"], json!({}));
        assert!(!run.raw().contains("account-token"), "{}", run.raw());
    }

    #[tokio::test]
    async fn a_server_that_exists_only_as_a_keychain_grant_is_synced_the_slack_case() {
        // A connector authorized through Claude Code leaves NOTHING in
        // ~/.claude.json, so there was no definition to copy; and the account token is
        // deliberately withheld from third parties, so there was no credential to
        // point at either. The grant supplies both.
        let run = Run::new(no_files(), keychain(slack_grant()), registry_file());
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 0, "{}", run.text());
        let slack = run.registry()["servers"]["slack"].clone();
        assert_eq!(slack["url"], "https://slack.example.com/mcp");
        assert_eq!(
            slack["headers"]["Authorization"],
            "Bearer ${keychain:Claude Code-credentials#mcpOAuth.slack|a1b2c3.accessToken}"
        );
        let text = run.raw();
        // Its OWN grant, never the account token — Slack would reject that anyway.
        assert!(!text.contains("claudeAiOauth"), "{text}");
        // And no secret is written down, which is the invariant the command holds.
        assert!(!text.contains("slack-secret-token"), "{text}");
    }

    #[tokio::test]
    async fn a_plugin_namespaced_name_is_renamed_to_a_slug_bough_accepts() {
        // The real failure, verbatim: `plugin:slack:slack  failed — invalid server
        // name`. The name is what you type in /mcp, so `slack` is the right answer and
        // the rename has to be said out loud.
        let run = Run::new(
            no_files(),
            keychain(json!({ "plugin:slack:slack|a1b2c3": {
                "serverName": "plugin:slack:slack",
                "serverUrl": "https://slack.example.com/mcp",
                "accessToken": "t",
                "expiresAt": 4_000_000_000_000i64,
            }})),
            registry_file(),
        );
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 0, "{}", run.text());
        let servers = run.registry()["servers"].clone();
        assert_eq!(
            servers.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["slack"]
        );
        // The grant is keyed by CLAUDE CODE's name — the rename is bough's business.
        assert_eq!(
            servers["slack"]["headers"]["Authorization"],
            "Bearer ${keychain:Claude Code-credentials#mcpOAuth.plugin:slack:slack|a1b2c3.accessToken}"
        );
        assert!(
            run.text().contains("renamed from plugin:slack:slack"),
            "{}",
            run.text()
        );
    }

    #[test]
    fn a_rename_never_lands_on_top_of_another_server() {
        let set =
            |names: &[&str]| -> BTreeSet<String> { names.iter().map(|s| s.to_string()).collect() };
        // Already valid: untouched.
        assert_eq!(bough_name("slack", &set(&[])).as_deref(), Some("slack"));
        assert_eq!(
            bough_name("plugin:slack:slack", &set(&[])).as_deref(),
            Some("slack")
        );
        // `slack` is spoken for, so the whole name is slugified rather than merged.
        assert_eq!(
            bough_name("plugin:slack:slack", &set(&["slack"])).as_deref(),
            Some("plugin-slack-slack")
        );
        assert_eq!(
            bough_name("plugin:slack:slack", &set(&["slack", "plugin-slack-slack"])),
            None
        );
        assert_eq!(
            bough_name("Weird Name!", &set(&[])).as_deref(),
            Some("weird-name")
        );
    }

    #[tokio::test]
    async fn a_configured_server_is_matched_to_its_grant_by_name_then_by_url() {
        let mut grants = slack_grant();
        grants["notion|d4e5"] = json!({
            "serverName": "notion",
            "serverUrl": "https://notion.example.com/mcp",
            "accessToken": "t",
            "expiresAt": 4_000_000_000_000i64,
        });
        let run = Run::new(
            reader(vec![(
                format!("{HOME}/.claude.json"),
                json!({ "mcpServers": {
                    // Same name as the grant…
                    "slack": { "type": "http", "url": "https://slack.example.com/mcp" },
                    // …and a different name, same endpoint but for a trailing slash.
                    "notes": { "type": "http", "url": "https://notion.example.com/mcp/" },
                }}),
            )]),
            keychain(grants),
            registry_file(),
        );
        run_sync_mcp(&[], &run.deps).await;
        let servers = run.registry()["servers"].clone();
        assert!(
            servers["slack"]["headers"]["Authorization"]
                .as_str()
                .unwrap()
                .contains("mcpOAuth.slack|a1b2c3"),
            "{servers}"
        );
        assert!(
            servers["notes"]["headers"]["Authorization"]
                .as_str()
                .unwrap()
                .contains("mcpOAuth.notion|d4e5"),
            "{servers}"
        );
        // Matched, so the grant does not ALSO land as a second server.
        assert!(servers.get("notion").is_none(), "{servers}");
    }

    #[tokio::test]
    async fn an_expired_grant_is_synced_and_said_out_loud_not_silently_sent() {
        let run = Run::new(
            no_files(),
            keychain(json!({ "slack|a1b2c3": {
                "serverName": "slack",
                "serverUrl": "https://slack.example.com/mcp",
                "accessToken": "stale",
                "expiresAt": 1_600_000_000_000i64,
            }})),
            registry_file(),
        );
        run_sync_mcp(&[], &run.deps).await;
        assert!(
            run.registry()["servers"]["slack"].is_object(),
            "still registered"
        );
        let text = run.text().to_lowercase();
        assert!(text.contains("expired"), "{text}");
        // …and how to refresh it.
        assert!(text.contains("claude"), "{text}");
    }

    #[tokio::test]
    async fn the_same_endpoint_under_a_different_name_is_one_server_not_two() {
        // The screenshot that reported this: `slack` and `plugin-slack-slack` side by
        // side, same URL. The registry is keyed by name, so nothing downstream would
        // ever have noticed the duplicate.
        let file = registry_file();
        let first = Run::new(no_files(), keychain(slack_grant()), file.clone());
        run_sync_mcp(&[], &first.deps).await;
        // Now the same endpoint arrives under Claude Code's namespaced name.
        let second = Run::new(
            no_files(),
            keychain(json!({ "plugin:slack:slack|zz99": {
                "serverName": "plugin:slack:slack",
                "serverUrl": "https://slack.example.com/mcp",
                "accessToken": "t",
                "expiresAt": 4_000_000_000_000i64,
            }})),
            file,
        );
        run_sync_mcp(&[], &second.deps).await;
        assert_eq!(
            second.registry()["servers"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["slack"]
        );
    }

    #[tokio::test]
    async fn with_a_duplicate_already_there_the_credential_lands_on_the_better_name() {
        // `bough_name` refuses a name that is taken — and here the taker IS this same
        // server, so name-first logic renamed it into a duplicate of itself and
        // credentialed the ugly one. The endpoint decides first.
        let file = registry_file();
        std::fs::write(
            &file,
            json!({ "servers": {
                "slack": { "url": "https://mcp.slack.com/mcp", "args": [], "env": {}, "headers": {} },
                "plugin-slack-slack": { "url": "https://mcp.slack.com/mcp", "args": [], "env": {}, "headers": {} },
            }, "activations": {}})
            .to_string(),
        )
        .unwrap();
        let run = Run::new(
            no_files(),
            keychain(json!({ "plugin:slack:slack|a1b2": {
                "serverName": "plugin:slack:slack",
                "serverUrl": "https://mcp.slack.com/mcp",
                "accessToken": "t",
                "expiresAt": 4_000_000_000_000i64,
            }})),
            file,
        );
        run_sync_mcp(&[], &run.deps).await;
        let servers = run.registry()["servers"].clone();
        assert!(
            servers["slack"]["headers"]["Authorization"]
                .as_str()
                .unwrap_or("")
                .contains("mcpOAuth.plugin:slack:slack|a1b2"),
            "{servers}"
        );
        assert_eq!(servers["plugin-slack-slack"]["headers"], json!({}));
        // …and the duplicate that was already there is named, since silence is how it
        // survives. Removing it is the user's call, not this command's.
        assert!(run.text().contains("are the same server"), "{}", run.text());
    }

    #[tokio::test]
    async fn an_entry_with_no_credential_gets_one_when_a_grant_exists_for_it() {
        // "It should copy over everything, I shouldn't need to auth". A server
        // registered before this command could read grants sits there with no
        // Authorization at all, so the panel says "needs auth".
        let file = registry_file();
        let files = vec![(
            format!("{HOME}/.claude.json"),
            json!({ "mcpServers": { "slack": { "type": "http", "url": "https://slack.example.com/mcp" } } }),
        )];
        let first = Run::new(reader(files.clone()), no_keychain(), file.clone());
        run_sync_mcp(&[], &first.deps).await;
        assert_eq!(first.registry()["servers"]["slack"]["headers"], json!({}));

        // Second sync, now that the grant is readable — WITHOUT --force.
        let second = Run::new(reader(files), keychain(slack_grant()), file);
        run_sync_mcp(&[], &second.deps).await;
        assert!(
            second.registry()["servers"]["slack"]["headers"]["Authorization"]
                .as_str()
                .unwrap_or("")
                .contains("mcpOAuth.slack|a1b2c3"),
            "{}",
            second.registry()
        );
        assert!(
            second.text().contains("added the missing credential"),
            "{}",
            second.text()
        );
    }

    #[tokio::test]
    async fn an_entry_that_already_has_a_credential_is_left_alone() {
        // The complement: adding a header where there was none is not a clobber, but
        // REPLACING one is exactly what --force exists for.
        let file = registry_file();
        let files = vec![(
            format!("{HOME}/.claude.json"),
            json!({ "mcpServers": { "slack": {
                "type": "http",
                "url": "https://slack.example.com/mcp",
                "headers": { "Authorization": "Bearer ${MY_OWN_TOKEN}" },
            }}}),
        )];
        let first = Run::new(reader(files.clone()), no_keychain(), file.clone());
        run_sync_mcp(&[], &first.deps).await;
        let second = Run::new(reader(files), keychain(slack_grant()), file);
        run_sync_mcp(&[], &second.deps).await;
        assert_eq!(
            second.registry()["servers"]["slack"]["headers"]["Authorization"],
            "Bearer ${MY_OWN_TOKEN}"
        );
    }

    #[tokio::test]
    async fn an_existing_entry_is_kept_unless_force_and_dry_run_writes_nothing() {
        let file = registry_file();
        let before = vec![(
            format!("{HOME}/.claude.json"),
            json!({ "mcpServers": { "alpha": { "command": "from-claude" } } }),
        )];
        let after = vec![(
            format!("{HOME}/.claude.json"),
            json!({ "mcpServers": { "alpha": { "command": "changed-upstream" } } }),
        )];
        let first = Run::new(reader(before), no_keychain(), file.clone());
        run_sync_mcp(&[], &first.deps).await;

        // A hand-fixed local definition must survive a second sync.
        let second = Run::new(reader(after.clone()), no_keychain(), file.clone());
        run_sync_mcp(&[], &second.deps).await;
        assert_eq!(
            second.registry()["servers"]["alpha"]["command"],
            "from-claude"
        );

        let dry = Run::new(reader(after.clone()), no_keychain(), file.clone());
        assert_eq!(
            run_sync_mcp(&argv(&["--dry-run", "--force"]), &dry.deps).await,
            0
        );
        assert_eq!(dry.registry()["servers"]["alpha"]["command"], "from-claude");
        assert!(dry.text().contains("nothing written"), "{}", dry.text());

        let forced = Run::new(reader(after), no_keychain(), file);
        run_sync_mcp(&argv(&["--force"]), &forced.deps).await;
        assert_eq!(
            forced.registry()["servers"]["alpha"]["command"],
            "changed-upstream"
        );
    }

    #[tokio::test]
    async fn an_unusable_entry_is_reported_and_the_others_still_land() {
        let run = Run::new(
            reader(vec![(
                format!("{HOME}/.claude.json"),
                json!({ "mcpServers": { "broken": { "type": "stdio" }, "good": { "command": "ok" } } }),
            )]),
            no_keychain(),
            registry_file(),
        );
        // Something failed…
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 1, "{}", run.text());
        // …and the rest synced.
        assert!(
            run.registry()["servers"]["good"].is_object(),
            "{}",
            run.registry()
        );
        let text = run.text();
        assert!(text.contains("broken") && text.contains("failed"), "{text}");
    }

    #[tokio::test]
    async fn a_grant_that_is_empty_or_already_expired_is_adopted_and_said() {
        // Four servers synced cleanly, then failed one at a time in the panel — three
        // "expired", one "has no string at #mcpOAuth…" — with nothing tying any of it
        // back to the sync that wrote the references. Adopting anyway is correct:
        // Claude Code refreshes its own tokens. Doing it silently was not.
        let run = Run::new(
            reader(plugin_files()),
            keychain(json!({
                "dead|aaa": {
                    "serverName": "dead",
                    "serverUrl": "https://dead.example/mcp",
                    "accessToken": "",
                    "expiresAt": 0,
                },
                "stale|bbb": {
                    "serverName": "stale",
                    "serverUrl": "https://stale.example/mcp",
                    "accessToken": "an-old-token",
                    "expiresAt": 1_600_000_000_000i64,
                },
            })),
            registry_file(),
        );
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 0, "{}", run.text());
        let text = run.text();
        // The empty one names itself and says what to do — it can never resolve.
        assert!(text.contains("\"dead\" holds no token"), "{text}");
        assert!(text.contains("Re-authorize it in Claude Code"), "{text}");
        // The expired one is a note, not a warning: it is expected to come back.
        assert!(text.contains("\"stale\""), "{text}");
        assert!(text.contains("already expired"), "{text}");
        assert!(text.contains("bough does not refresh them"), "{text}");
        // Both are still adopted — the reference is written either way.
        let servers = run.registry()["servers"].clone();
        assert!(
            servers["dead"].is_object() && servers["stale"].is_object(),
            "{servers}"
        );
        // And no secret rides along.
        assert!(!text.contains("an-old-token"), "{text}");
        assert!(!run.raw().contains("an-old-token"), "{}", run.raw());
    }

    #[test]
    fn a_literal_looking_secret_in_env_is_warned_about_not_silently_republished() {
        assert!(looks_secret("GITHUB_TOKEN", "ghp_averyrealtokenvalue"));
        // Already a reference.
        assert!(!looks_secret("API_KEY", "${MY_KEY}"));
        assert!(!looks_secret("CHROME_PATH", "/Applications/Chrome.app"));
        assert!(!looks_secret("TOKEN", "short"));
    }

    #[tokio::test]
    async fn a_literal_secret_in_a_synced_env_is_said_out_loud() {
        let run = Run::new(
            reader(vec![(
                format!("{HOME}/.claude.json"),
                json!({ "mcpServers": { "gh": {
                    "command": "gh-mcp",
                    "env": { "GITHUB_TOKEN": "ghp_averyrealtokenvalue" },
                }}}),
            )]),
            no_keychain(),
            registry_file(),
        );
        // A warning, never a refusal: the entry still lands.
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 0, "{}", run.text());
        assert!(run.registry()["servers"]["gh"].is_object());
        assert!(
            run.text().contains("looks like a literal secret"),
            "{}",
            run.text()
        );
        assert!(run.text().contains("GET /mcp/servers"), "{}", run.text());
    }

    #[tokio::test]
    async fn syncing_twice_is_the_same_as_syncing_once_stdio_servers_included() {
        // Every plugin server needs a rename, so a second run found the first run's
        // name "taken", fell through to slugifying the whole name, and added
        // `plugin-claude-mem-mcp-search` beside `mcp-search`.
        let file = registry_file();
        let files = vec![
            (
                format!("{CONFIG_DIR}/plugins/installed_plugins.json"),
                json!({ "plugins": { "claude-mem@m": { "installPath": "/p/mem" } } }),
            ),
            (
                "/p/mem/.mcp.json".into(),
                json!({ "mcpServers": { "mcp-search": { "command": "bun", "args": ["run", "x"] } } }),
            ),
        ];
        let first = Run::new(reader(files.clone()), no_keychain(), file.clone());
        run_sync_mcp(&[], &first.deps).await;
        let after_one: Vec<String> = first.registry()["servers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(after_one, vec!["mcp-search".to_string()]);

        let second = Run::new(reader(files), no_keychain(), file);
        run_sync_mcp(&[], &second.deps).await;
        let after_two: Vec<String> = second.registry()["servers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            after_two, after_one,
            "running a sync twice must be running it once"
        );
    }

    #[tokio::test]
    async fn no_plugins_leaves_a_plugins_servers_alone() {
        let run = Run::new(reader(plugin_files()), no_keychain(), registry_file());
        assert_eq!(run_sync_mcp(&argv(&["--no-plugins"]), &run.deps).await, 0);
        assert!(run.text().contains("nothing to sync"), "{}", run.text());
    }

    #[tokio::test]
    async fn a_users_own_definition_beats_the_plugins_under_the_same_name() {
        // A plugin's definition is the default; someone who has written their own is
        // saying they want theirs.
        let mut files = plugin_files();
        files.push((
            format!("{HOME}/.claude.json"),
            json!({ "mcpServers": { "plugin:slack:slack": {
                "type": "http",
                "url": "https://slack.mine/mcp",
            }}}),
        ));
        let run = Run::new(reader(files), no_keychain(), registry_file());
        run_sync_mcp(&[], &run.deps).await;
        assert_eq!(
            run.registry()["servers"]["slack"]["url"],
            "https://slack.mine/mcp"
        );
    }

    #[tokio::test]
    async fn no_servers_anywhere_is_a_true_answer_not_a_failure() {
        let run = Run::new(no_files(), no_keychain(), registry_file());
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 0);
        assert!(
            run.text()
                .contains("no MCP servers found in Claude Code's config"),
            "{}",
            run.text()
        );
    }

    #[tokio::test]
    async fn a_denied_credential_prompt_is_said_and_the_config_half_still_works() {
        // A read failure is NOT an error: the config-file half of this command must
        // still work, and a denied dialog is a decision the user just made.
        let denied = reader_fn(|_| async { KeychainResult::miss(128, "user canceled", None) });
        let run = Run::new(
            reader(vec![(
                format!("{HOME}/.claude.json"),
                json!({ "mcpServers": { "gh": { "command": "gh-mcp" } } }),
            )]),
            denied,
            registry_file(),
        );
        assert_eq!(run_sync_mcp(&[], &run.deps).await, 0, "{}", run.text());
        assert!(run.text().contains("was denied"), "{}", run.text());
        assert!(
            run.text().contains("re-run and choose Allow"),
            "{}",
            run.text()
        );
        assert!(run.registry()["servers"]["gh"].is_object());
    }
}
