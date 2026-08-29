//! Invariant: THE CONFIG STORES A REFERENCE, NEVER A SECRET. A header (or stdio env) value of the
//! form `${keychain:<service>#<a.b.c>}` names a credential item another client on this machine
//! already holds; the read happens at CONNECT time, the resolved value goes into that one request
//! header (or child environment) and nowhere else: never logged, never persisted, never in
//! `--dump-config` (which renders the reference text, because that is what the config holds), and
//! never part of an error string.
//!
//! Ported from the pre-rebuild tree's `mcp/keychain.rs`, with the same three rules:
//!
//! 1. **`security` is executed as ARGV, never through a shell.** A service name is user-supplied
//!    text with spaces in it ("Claude Code-credentials"); handing a template string to `sh -c`
//!    makes a service name a command.
//! 2. **An expired token is reported, not refreshed.** The token belongs to the client that
//!    obtained it; refreshing on its behalf is impersonation rather than plumbing, and the fix
//!    (open that client once) is both trivial and the user's.
//! 3. **Every failure names the item and says what to do about it**, because all of them are
//!    recoverable by the human and none is diagnosable from the 401 that would otherwise arrive
//!    seconds later at a different layer.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use bough_plugin_mcp::McpError;
use futures::future::BoxFuture;
use serde_json::Value;

/// Which store produced a result. `None` means an injected test reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    Keychain,
    File,
}

/// The outcome of one credential read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainResult {
    /// The item's secret, verbatim. Empty when `code` is non-zero.
    pub value: String,
    /// `security`'s exit code, or the file reader's imitation of one. 44 is "the item does not
    /// exist"; 128 is a denied prompt.
    pub code: i32,
    /// Whatever the store said, trimmed. Never contains the secret.
    pub error: String,
    /// Which store produced this.
    pub store: Option<StoreKind>,
}

impl KeychainResult {
    pub fn ok(value: impl Into<String>, store: Option<StoreKind>) -> Self {
        Self {
            value: value.into(),
            code: 0,
            error: String::new(),
            store,
        }
    }
    pub fn miss(code: i32, error: impl Into<String>, store: Option<StoreKind>) -> Self {
        Self {
            value: String::new(),
            code,
            error: error.into(),
            store,
        }
    }
}

/// How a credential read is performed. Injected so tests never touch a real store.
pub type KeychainReader = Arc<dyn Fn(String) -> BoxFuture<'static, KeychainResult> + Send + Sync>;

/// Wrap an async fn as a [`KeychainReader`].
pub fn reader_fn<F, Fut>(f: F) -> KeychainReader
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = KeychainResult> + Send + 'static,
{
    Arc::new(move |service| Box::pin(f(service)) as BoxFuture<'static, KeychainResult>)
}

// ---------------------------------------------------------------------------
// The two stores
// ---------------------------------------------------------------------------

/// The item Claude Code keeps its login and its per-server MCP OAuth grants in.
pub const CLAUDE_CODE_ITEM: &str = "Claude Code-credentials";

/// `security find-generic-password -s <service> -w`.
///
/// NO PLATFORM GATE. A missing `security` binary reports as "no such item" (44), the same as a
/// keychain that simply does not hold it, so both stores can be tried anywhere and whichever one
/// answers wins.
pub async fn security_read(service: &str) -> KeychainResult {
    let out = tokio::process::Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    match out {
        Ok(out) => KeychainResult {
            value: String::from_utf8_lossy(&out.stdout).into_owned(),
            code: out.status.code().unwrap_or(1),
            error: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            store: Some(StoreKind::Keychain),
        },
        // No `security` on PATH: there is no keychain on this machine to hold the item. 44 rather
        // than an error, because "this store does not have it" is the truth and it is what lets
        // the next store be asked.
        Err(e) => KeychainResult::miss(44, e.to_string(), Some(StoreKind::Keychain)),
    }
}

/// Where Claude Code keeps its configuration. `CLAUDE_CONFIG_DIR` is Claude Code's own override
/// and is honoured for the same reason `BOUGH_HOME` is: a machine that has moved its config has
/// moved the credentials with it, and reading the default path would silently find nothing.
pub fn claude_config_dir() -> PathBuf {
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(o) if !o.trim().is_empty() => PathBuf::from(o),
        _ => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"));
            home.join(".claude")
        }
    }
}

/// The credentials file inside it, the store Claude Code uses where it is not using a keychain.
pub fn credentials_path() -> PathBuf {
    claude_config_dir().join(".credentials.json")
}

/// THIS READER ANSWERS FOR EXACTLY ONE ITEM. A file holding Claude Code's login is not a general
/// vault: answering some OTHER service's read with this file's contents would hand one client's
/// credential to a reference that asked for a different one.
pub async fn credentials_file_read(service: &str) -> KeychainResult {
    if service != CLAUDE_CODE_ITEM {
        return KeychainResult::miss(44, "", Some(StoreKind::File));
    }
    let path = credentials_path();
    match std::fs::read_to_string(&path) {
        Ok(v) => KeychainResult::ok(v, Some(StoreKind::File)),
        Err(e) => {
            // Absent is the ordinary state of a Mac that uses its keychain, so it reports as "not
            // there" and lets the next store answer. A permission problem is NOT that: the file
            // exists and is being withheld.
            let code = match e.kind() {
                std::io::ErrorKind::NotFound => 44,
                std::io::ErrorKind::PermissionDenied => 128,
                _ => 1,
            };
            KeychainResult::miss(
                code,
                format!("{}: {}", path.display(), e),
                Some(StoreKind::File),
            )
        }
    }
}

/// The two stores, in the order this platform should ask them.
///
/// ORDER IS BY AUTHORITY, not availability. The keychain goes first WHERE IT IS THE ONE CLAUDE
/// CODE WRITES TO, so a stale `.credentials.json` left behind by an older install cannot shadow a
/// live token; everywhere else the file is what gets written and is asked first, so the ordinary
/// case costs no spawn. Either way the other store is still consulted.
pub fn default_stores() -> Vec<KeychainReader> {
    let keychain = reader_fn(|service: String| async move { security_read(&service).await });
    let file = reader_fn(|service: String| async move { credentials_file_read(&service).await });
    if std::env::consts::OS == "macos" {
        vec![keychain, file]
    } else {
        vec![file, keychain]
    }
}

/// First store that SATISFIES the read wins; if none does, the most specific failure is what gets
/// reported.
///
/// The question a store has to answer is not "do you have this item" but "do you have what was
/// asked for". A store that returns an item missing the requested path is a MISS, and the next
/// store gets asked. The unsatisfying bytes are still remembered and returned when nothing
/// satisfies, because the caller's error message names what the item DOES hold.
pub async fn read_from_stores(
    service: &str,
    stores: &[KeychainReader],
    satisfies: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
) -> KeychainResult {
    let mut worst: Option<KeychainResult> = None;
    let mut unsatisfying: Option<KeychainResult> = None;
    for read in stores {
        let result = read(service.to_string()).await;
        if result.code == 0 && !result.value.is_empty() {
            match satisfies {
                None => return result,
                Some(pred) if pred(&result.value) => return result,
                Some(_) => {
                    if unsatisfying.is_none() {
                        unsatisfying = Some(result);
                    }
                    continue;
                }
            }
        }
        let replace = match &worst {
            None => true,
            Some(w) => w.code == 44 && result.code != 44,
        };
        if replace {
            worst = Some(result);
        }
    }
    unsatisfying
        .or(worst)
        .unwrap_or_else(|| KeychainResult::miss(44, "", Some(StoreKind::File)))
}

/// Does this item hold a usable string at `path`? The `satisfies` predicate for an ordinary
/// `${keychain:…#a.b}` reference. An empty path means the whole item is the secret, and any bytes
/// satisfy that.
pub fn holds_path(value: &str, path: &[String]) -> bool {
    if path.is_empty() {
        return true;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(value) else {
        return false;
    };
    let (found, _) = walk(&parsed, path);
    matches!(found, Some(Value::String(s)) if !s.is_empty())
}

// ---------------------------------------------------------------------------
// The reference
// ---------------------------------------------------------------------------

/// A parsed `${keychain:<service>}` or `${keychain:<service>#<a.b.c>}` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainRef {
    pub service: String,
    /// Dotted path into the item's JSON. Empty = the whole secret, verbatim.
    pub path: Vec<String>,
}

/// Is this config value shaped like a reference at all? Cheaper than parsing, for the validator's
/// "looks like one but does not parse" rejection.
pub fn looks_like_keychain_ref(value: &str) -> bool {
    value.trim().starts_with("${keychain:")
}

/// `${keychain:NAME}` / `${keychain:NAME#a.b}`. The service may contain spaces. Neither capture
/// may contain `{`, `}`, and the path may not contain a second `#`.
pub fn parse_keychain_ref(value: &str) -> Option<KeychainRef> {
    let t = value.trim();
    let inner = t.strip_prefix("${keychain:")?.strip_suffix('}')?;
    if inner.contains('{') || inner.contains('}') {
        return None;
    }
    let (service, path_str) = match inner.find('#') {
        Some(i) => (&inner[..i], &inner[i + 1..]),
        None => (inner, ""),
    };
    if path_str.contains('#') {
        return None;
    }
    let service = service.trim().to_string();
    if service.is_empty() {
        return None;
    }
    let path = path_str
        .split('.')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    Some(KeychainRef { service, path })
}

/// Resolve one config value: a keychain reference becomes its secret, anything else passes
/// through verbatim. The one entry point `server.rs` uses, for headers and stdio env alike.
pub async fn resolve_value(
    value: &str,
    reader: Option<&KeychainReader>,
) -> Result<String, McpError> {
    match parse_keychain_ref(value) {
        Some(keychain_ref) => read_keychain_ref(&keychain_ref, reader).await,
        None => Ok(value.to_string()),
    }
}

/// Resolve one reference to its secret.
pub async fn read_keychain_ref(
    keychain_ref: &KeychainRef,
    reader: Option<&KeychainReader>,
) -> Result<String, McpError> {
    // An INJECTED reader is one store and is asked as one; the store-picking rule only has
    // meaning when there is more than one store to pick between.
    let result = match reader {
        Some(read) => read(keychain_ref.service.clone()).await,
        None => {
            let path = keychain_ref.path.clone();
            let pred = move |v: &str| holds_path(v, &path);
            read_from_stores(&keychain_ref.service, &default_stores(), Some(&pred)).await
        }
    };
    // `security -w` terminates its output with a newline that is not part of the secret. Stripped
    // HERE rather than in the reader so it holds for every reader: a token with a newline welded
    // on produces a header the remote end rejects for reasons it will not explain.
    let value = strip_one_newline(&result.value);
    if result.code != 0 || value.is_empty() {
        return Err(McpError::Transport(keychain_failure(
            &keychain_ref.service,
            result.code,
            &result.error,
            result.store,
        )));
    }
    if keychain_ref.path.is_empty() {
        return Ok(value);
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&value) else {
        return Err(McpError::Transport(format!(
            "the keychain item \"{}\" is not JSON, so #{} cannot be read out of it — drop the \
             #path to use the whole item as the secret",
            keychain_ref.service,
            keychain_ref.path.join(".")
        )));
    };
    let (found, container) = walk(&parsed, &keychain_ref.path);
    let found = match found {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(McpError::Transport(format!(
                "the keychain item \"{}\" has no string at #{}. It holds: {}",
                keychain_ref.service,
                keychain_ref.path.join("."),
                describe(&parsed)
            )))
        }
    };
    assert_fresh(keychain_ref, container, now_ms())?;
    Ok(found)
}

fn strip_one_newline(raw: &str) -> String {
    let s = raw.strip_suffix('\n').unwrap_or(raw);
    s.strip_suffix('\r').unwrap_or(s).to_string()
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// An `expiresAt` sitting beside the token is a fact about the token, so it is checked. Epoch
/// milliseconds or an RFC 3339 string; both are common and neither is worth guessing wrong about.
fn assert_fresh(
    keychain_ref: &KeychainRef,
    container: Option<&Value>,
    now: i64,
) -> Result<(), McpError> {
    let Some(Value::Object(obj)) = container else {
        return Ok(());
    };
    let at = match obj.get("expiresAt") {
        Some(Value::Number(n)) => n.as_f64().map(|f| f as i64),
        Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.timestamp_millis()),
        _ => None,
    };
    let Some(at) = at else { return Ok(()) };
    if at > now {
        return Ok(());
    }
    let when = chrono::DateTime::from_timestamp_millis(at)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| format!("{at}"));
    Err(McpError::Transport(format!(
        "the token in keychain item \"{}\" expired at {when}. bough does not refresh a credential \
         it did not obtain — open the client that owns this item (for \"{CLAUDE_CODE_ITEM}\", run \
         `claude` once) and it will refresh it in place",
        keychain_ref.service,
    )))
}

/// The value at `path`, and the object it was found in (for the expiry check).
fn walk<'a>(root: &'a Value, path: &[String]) -> (Option<&'a Value>, Option<&'a Value>) {
    let mut container: Option<&Value> = None;
    let mut node: &Value = root;
    let mut i = 0usize;
    while i < path.len() {
        let Value::Object(here) = node else {
            return (None, container);
        };
        container = Some(node);
        if let Some(next) = here.get(&path[i]) {
            node = next;
            i += 1;
            continue;
        }
        // A KEY THAT CONTAINS DOTS. `mcpOAuth."slack|mcp.example.com#a1b2"` is the shape Claude
        // Code stores a per-server OAuth grant under, and it is unaddressable by splitting alone.
        // Rejoining the remaining segments longest-first finds it, and only ever runs when the
        // plain segment missed, so an exact key still wins and no existing reference changes
        // meaning.
        let mut matched = false;
        let mut end = path.len();
        while end > i + 1 {
            let joined = path[i..end].join(".");
            if let Some(next) = here.get(&joined) {
                node = next;
                i = end;
                matched = true;
                break;
            }
            end -= 1;
        }
        if !matched {
            return (None, container);
        }
    }
    (Some(node), container)
}

/// The item's SHAPE, for an error message. Never a value: this is a secret.
fn describe(parsed: &Value) -> String {
    match parsed {
        Value::Array(a) => format!("an array of {}", a.len()),
        Value::Object(o) if o.is_empty() => "an empty object".to_string(),
        Value::Object(o) => format!(
            "an object with {}",
            o.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
        Value::String(_) => "string".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::Bool(_) => "boolean".to_string(),
        Value::Null => "null".to_string(),
    }
}

/// What went wrong, in the user's terms, and in terms of the store that said so.
///
/// "NOT FOUND" NAMES BOTH STORES, because both were tried. A message that mentions only the one
/// that happened to answer last reads as though the other was never looked at.
fn keychain_failure(service: &str, code: i32, error: &str, store: Option<StoreKind>) -> String {
    let file = store == Some(StoreKind::File);
    let head = format!("could not read credential item \"{service}\"");
    let creds_path = credentials_path();
    let creds = creds_path.display();
    let lower = error.to_ascii_lowercase();
    if code == 44 || lower.contains("could not be found") {
        let advice = format!(
            "Make sure the client that owns it has been logged in on this machine. For \
             \"{CLAUDE_CODE_ITEM}\", run `claude` once, and set CLAUDE_CONFIG_DIR if its \
             configuration lives somewhere else."
        );
        return if file {
            format!("{head}: it is in neither {creds} nor the login keychain. {advice}")
        } else {
            format!(
                "{head}: no generic-password item with that service name is in the login \
                 keychain, and {creds} does not hold it either. Check the name with `security \
                 find-generic-password -s \"{service}\"`. {advice}"
            )
        };
    }
    if code == 128
        || lower.contains("user interaction")
        || lower.contains("denied")
        || lower.contains("cancel")
    {
        return if file {
            let tail = if error.is_empty() {
                String::new()
            } else {
                format!(": {error}")
            };
            format!("{head}: {creds} is not readable by this process{tail}.")
        } else {
            format!(
                "{head}: the keychain access prompt was denied or cancelled. macOS asks once per \
                 program — answer \"Always Allow\" to stop it asking again."
            )
        };
    }
    if file {
        let what = if error.is_empty() {
            format!("reading {creds} failed")
        } else {
            error.to_string()
        };
        format!("{head}: {what}.")
    } else {
        let tail = if error.is_empty() {
            String::new()
        } else {
            format!(" — {error}")
        };
        format!("{head}: security exited {code}{tail}.")
    }
}

#[cfg(test)]
mod tests {
    //! Every test injects the reader, so nothing here spawns `security`, reads the developer's
    //! login keychain, or raises an access dialog.

    use super::*;

    fn ok_reader(value: &str) -> KeychainReader {
        let value = value.to_string();
        reader_fn(move |_| {
            let value = value.clone();
            async move { KeychainResult::ok(value, None) }
        })
    }
    fn fails(code: i32, error: &str) -> KeychainReader {
        let error = error.to_string();
        reader_fn(move |_| {
            let error = error.clone();
            async move { KeychainResult::miss(code, error, None) }
        })
    }

    /// What Claude Code stores, in the shape it stores it: the login blob plus one per-server
    /// MCP OAuth grant whose key contains a `|` and could contain dots.
    fn blob(expires_at: i64) -> String {
        serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-TOKEN",
                "expiresAt": expires_at,
            },
            "mcpOAuth": {
                "linear-server|638130d5ab3558f4": {
                    "accessToken": "lin_oauth_TOKEN",
                    "expiresAt": expires_at,
                },
                "plugin:slack:slack|38801a7d845718b3": {
                    "accessToken": "xoxe_TOKEN",
                    "expiresAt": expires_at,
                },
            },
        })
        .to_string()
    }
    fn fresh_blob() -> String {
        blob(now_ms() + 3_600_000)
    }
    fn parse(v: &str) -> KeychainRef {
        parse_keychain_ref(v).expect("a reference")
    }

    #[test]
    fn a_reference_names_an_item_and_optionally_a_field_inside_it() {
        assert_eq!(
            parse_keychain_ref("${keychain:Claude Code-credentials}"),
            Some(KeychainRef {
                service: "Claude Code-credentials".into(),
                path: vec![],
            })
        );
        assert_eq!(
            parse_keychain_ref("${keychain:Claude Code-credentials#a.b}"),
            Some(KeychainRef {
                service: "Claude Code-credentials".into(),
                path: vec!["a".into(), "b".into()],
            })
        );
        assert_eq!(parse_keychain_ref("${keychain:}"), None);
        assert_eq!(parse_keychain_ref("${keychain:a#b#c}"), None);
        assert_eq!(parse_keychain_ref("Bearer plain-token"), None);
        assert!(looks_like_keychain_ref(" ${keychain:x}"));
        assert!(!looks_like_keychain_ref("Bearer x"));
    }

    #[tokio::test]
    async fn a_plain_value_passes_through_and_a_reference_resolves() {
        let reader = ok_reader(&fresh_blob());
        assert_eq!(
            resolve_value("Bearer plain", Some(&reader)).await.unwrap(),
            "Bearer plain"
        );
        let v = resolve_value(
            "${keychain:Claude Code-credentials#claudeAiOauth.accessToken}",
            Some(&reader),
        )
        .await
        .unwrap();
        assert_eq!(v, "sk-ant-oat01-TOKEN");
    }

    #[tokio::test]
    async fn a_grant_key_containing_a_pipe_and_dots_is_reachable_by_rejoining() {
        let reader = ok_reader(&fresh_blob());
        let v = read_keychain_ref(
            &parse(
                "${keychain:Claude Code-credentials#mcpOAuth.linear-server|638130d5ab3558f4.accessToken}",
            ),
            Some(&reader),
        )
        .await
        .unwrap();
        assert_eq!(v, "lin_oauth_TOKEN");
        let v = read_keychain_ref(
            &parse(
                "${keychain:Claude Code-credentials#mcpOAuth.plugin:slack:slack|38801a7d845718b3.accessToken}",
            ),
            Some(&reader),
        )
        .await
        .unwrap();
        assert_eq!(v, "xoxe_TOKEN");
    }

    #[tokio::test]
    async fn an_expired_token_is_reported_with_the_owners_name_never_refreshed() {
        let reader = ok_reader(&blob(now_ms() - 1_000));
        let err = read_keychain_ref(
            &parse("${keychain:Claude Code-credentials#claudeAiOauth.accessToken}"),
            Some(&reader),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expired at"), "{msg}");
        assert!(msg.contains("run `claude` once"), "{msg}");
        assert!(
            !msg.contains("TOKEN"),
            "the secret never rides an error: {msg}"
        );
    }

    #[tokio::test]
    async fn a_missing_item_and_a_denied_prompt_read_differently() {
        let miss = read_keychain_ref(
            &parse("${keychain:Nope}"),
            Some(&fails(44, "could not be found")),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(miss.contains("logged in on this machine"), "{miss}");

        let denied = read_keychain_ref(
            &parse("${keychain:Nope}"),
            Some(&fails(128, "User interaction is not allowed.")),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(denied.contains("Always Allow"), "{denied}");
    }

    #[tokio::test]
    async fn a_wrong_path_names_the_shape_and_never_a_value() {
        let err = read_keychain_ref(
            &parse("${keychain:Claude Code-credentials#claudeAiOauth.nope}"),
            Some(&ok_reader(&fresh_blob())),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("no string at"), "{err}");
        assert!(!err.contains("sk-ant"), "{err}");
    }

    #[tokio::test]
    async fn a_trailing_newline_from_security_is_not_part_of_the_secret() {
        let reader = ok_reader("raw-secret\n");
        let v = read_keychain_ref(&parse("${keychain:Whole Item}"), Some(&reader))
            .await
            .unwrap();
        assert_eq!(v, "raw-secret");
    }

    #[tokio::test]
    async fn the_store_that_satisfies_the_path_wins_over_the_one_that_merely_answers() {
        // The first store holds SOMETHING under the service name, but not the requested path; the
        // second holds the grants. The read must come back from the second.
        let empty = reader_fn(|_| async move {
            KeychainResult::ok(r#"{"other":true}"#, Some(StoreKind::Keychain))
        });
        let full = {
            let value = fresh_blob();
            reader_fn(move |_| {
                let value = value.clone();
                async move { KeychainResult::ok(value, Some(StoreKind::File)) }
            })
        };
        let path = vec!["claudeAiOauth".to_string(), "accessToken".to_string()];
        let pred = {
            let path = path.clone();
            move |v: &str| holds_path(v, &path)
        };
        let out = read_from_stores("Claude Code-credentials", &[empty, full], Some(&pred)).await;
        assert_eq!(out.store, Some(StoreKind::File));
        assert!(holds_path(&out.value, &path));
    }

    #[test]
    fn holds_path_is_the_predicate_it_claims_to_be() {
        assert!(holds_path("any bytes at all", &[]));
        assert!(holds_path(
            &fresh_blob(),
            &["claudeAiOauth".into(), "accessToken".into()]
        ));
        assert!(!holds_path(&fresh_blob(), &["nope".into()]));
        assert!(!holds_path("not json", &["a".into()]));
    }
}
