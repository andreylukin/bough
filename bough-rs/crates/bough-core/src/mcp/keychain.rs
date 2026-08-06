//! Reading a secret out of the credential store another client on this machine
//! already put it in, for MCP servers that client has already authorized (port of
//! `src/mcp/keychain.ts`).
//!
//! THE INVARIANT THIS HOLDS, and it is the registry's own rule extended one step:
//! **the registry stores a REFERENCE, never a secret.** What is written down is the
//! item's name; the read happens at CONNECT time and the value goes into one request
//! header and nowhere else. Never logged, never persisted, never part of a status
//! response, never reachable from a program.
//!
//! SECOND — **`security` is executed as ARGV, never through a shell.** A service name
//! is user-supplied text with spaces in it (`Claude Code-credentials`); the one-line
//! version everyone writes first hands a template string to `sh -c`, and then a
//! service name is a command.
//!
//! THIRD — **an expired token is reported, not refreshed.** Those tokens belong to
//! the client that obtained them; refreshing on its behalf is impersonation rather
//! than plumbing, and the fix — open that client once — is both trivial and the
//! user's.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use crate::errors::{BoughError, ErrorKind};

/// Which store produced a result. `None` means the keychain, so an injected reader in
/// a test keeps the wording it was written against.
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
    /// `security`'s exit code, or the file reader's imitation of one. 44 is "the item
    /// does not exist"; 128 is a denied prompt.
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

/// Absent = whichever store this machine keeps the credential in.
#[derive(Clone, Default)]
pub struct KeychainOptions {
    pub keychain: Option<KeychainReader>,
}

impl std::fmt::Debug for KeychainOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeychainOptions")
            .field("keychain", &self.keychain.as_ref().map(|_| "<reader>"))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// The two stores
// ---------------------------------------------------------------------------

/// `security find-generic-password -s <service> -w`.
///
/// NO PLATFORM GATE. A missing `security` binary reports as "no such item" (44), the
/// same as a keychain that simply does not hold it, so both stores can be tried
/// anywhere and whichever one answers wins.
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
        // No `security` on PATH: there is no keychain on this machine to hold the
        // item. 44 rather than an error, because "this store does not have it" is the
        // truth and it is what lets the next store be asked.
        Err(e) => KeychainResult::miss(44, e.to_string(), Some(StoreKind::Keychain)),
    }
}

/// [`security_read`] as an injectable reader.
pub fn security_reader() -> KeychainReader {
    reader_fn(|service: String| async move { security_read(&service).await })
}

/// Where Claude Code keeps its configuration and its credentials file.
///
/// `CLAUDE_CONFIG_DIR` is Claude Code's own override and is honoured for the same
/// reason `BOUGH_HOME` is: a machine that has moved its config has moved the
/// credentials with it, and reading the default path would silently find nothing.
pub fn claude_config_dir_from(env: &dyn Fn(&str) -> Option<String>, home: &Path) -> PathBuf {
    match env("CLAUDE_CONFIG_DIR") {
        Some(o) if !o.trim().is_empty() => PathBuf::from(o),
        _ => home.join(".claude"),
    }
}

/// The same, against the live process environment.
pub fn claude_config_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    claude_config_dir_from(&|n| std::env::var(n).ok(), &home)
}

/// The credentials file inside it.
pub fn credentials_path_from(env: &dyn Fn(&str) -> Option<String>, home: &Path) -> PathBuf {
    claude_config_dir_from(env, home).join(".credentials.json")
}

/// The same, against the live process environment.
pub fn credentials_path() -> PathBuf {
    claude_config_dir().join(".credentials.json")
}

/// `$CLAUDE_CONFIG_DIR/.credentials.json`, the store Claude Code uses where it is not
/// using a keychain.
///
/// THIS READER ANSWERS FOR EXACTLY ONE ITEM. A file holding Claude Code's login is not
/// a general vault: answering some OTHER service's read with this file's contents
/// would hand one client's credential to a reference that asked for a different one.
pub async fn credentials_file_read(service: &str) -> KeychainResult {
    if service != CLAUDE_CODE_ITEM {
        return KeychainResult::miss(44, "", Some(StoreKind::File));
    }
    let path = credentials_path();
    match std::fs::read_to_string(&path) {
        Ok(v) => KeychainResult::ok(v, Some(StoreKind::File)),
        Err(e) => {
            // Absent is the ordinary state of a Mac that uses its keychain, so it
            // reports as "not there" and lets the next store answer. A permission
            // problem is NOT that: the file exists and is being withheld.
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

/// [`credentials_file_read`] as an injectable reader.
pub fn credentials_file_reader() -> KeychainReader {
    reader_fn(|service: String| async move { credentials_file_read(&service).await })
}

/// The two stores, in the order this platform should ask them.
///
/// ORDER IS BY AUTHORITY, not availability. The keychain goes first WHERE IT IS THE
/// ONE CLAUDE CODE WRITES TO, so a stale `.credentials.json` left behind by an older
/// install cannot shadow a live token; everywhere else the file is what gets written
/// and is asked first, so the ordinary case costs no spawn. Either way the other store
/// is still consulted, so neither setup is out of reach.
pub fn store_order(platform: &str) -> [StoreKind; 2] {
    if platform == "darwin" || platform == "macos" {
        [StoreKind::Keychain, StoreKind::File]
    } else {
        [StoreKind::File, StoreKind::Keychain]
    }
}

/// The readers for [`store_order`], in that order.
pub fn credential_stores(platform: &str) -> Vec<KeychainReader> {
    store_order(platform)
        .into_iter()
        .map(|kind| match kind {
            StoreKind::Keychain => security_reader(),
            StoreKind::File => credentials_file_reader(),
        })
        .collect()
}

/// This machine's stores. `std::env::consts::OS` is `"macos"` where TS says `"darwin"`.
pub fn default_stores() -> Vec<KeychainReader> {
    credential_stores(std::env::consts::OS)
}

/// The store this machine actually keeps Claude Code's credentials in.
pub async fn default_credential_read(service: &str) -> KeychainResult {
    read_from_stores(service, &default_stores(), None).await
}

/// The same store selection, but for a caller that knows what it needs to find.
///
/// `default_credential_read` cannot express "the store that has the grants" because a
/// reader is handed a service name and nothing else. Anything reading a FIELD out of
/// the item — [`read_keychain_ref`], and `sync-mcp` reading the `mcpOAuth` map — needs
/// the store chosen by content, since the two stores on one machine can hold different
/// halves of the same item.
pub fn credential_reader_for(satisfies: Arc<dyn Fn(&str) -> bool + Send + Sync>) -> KeychainReader {
    reader_fn(move |service: String| {
        let satisfies = satisfies.clone();
        async move { read_from_stores(&service, &default_stores(), Some(&*satisfies)).await }
    })
}

/// First store that SATISFIES the read wins; if none does, the most specific failure
/// is what gets reported.
///
/// The question a store has to answer is not "do you have this item" but "do you have
/// what was asked for". A store that returns an item missing the requested path is a
/// MISS, and the next store gets asked. The unsatisfying bytes are still remembered
/// and returned when nothing satisfies, because the caller's error message names what
/// the item DOES hold.
///
/// A MEASUREMENT TRAP worth leaving written down: over SSH the login keychain is not
/// unlocked, so `security` returns nothing and every probe concludes "this machine has
/// only a file". Diagnose store questions from the server's own context, never from a
/// remote shell.
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

/// Does this item hold a usable string at `path`? The `satisfies` predicate for an
/// ordinary `${keychain:…#a.b}` reference. An empty path means the whole item is the
/// secret, and any bytes satisfy that.
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

/// `${keychain:NAME}` / `${keychain:NAME#a.b}`. The service may contain spaces.
///
/// Hand-parsed rather than regex'd so the character classes stay obvious: the TS
/// pattern is `^\$\{keychain:([^#{}]+?)(?:#([^{}]*))?\}$`.
pub fn parse_keychain_ref(value: &str) -> Option<KeychainRef> {
    let t = value.trim();
    let inner = t.strip_prefix("${keychain:")?.strip_suffix('}')?;
    // Neither capture may contain `{` or `}`.
    if inner.contains('{') || inner.contains('}') {
        return None;
    }
    let (service, path_str) = match inner.find('#') {
        Some(i) => (&inner[..i], &inner[i + 1..]),
        None => (inner, ""),
    };
    // The service capture is `[^#{}]+?` — at least one char, and no `#`.
    if service.is_empty() || path_str.contains('#') {
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

/// Resolve one reference to its secret.
///
/// Every failure names the item and says what to do about it, because all of them are
/// recoverable by the human and none of them is diagnosable from the 401 that would
/// otherwise arrive several seconds later at a different layer.
pub async fn read_keychain_ref(
    keychain_ref: &KeychainRef,
    opts: &KeychainOptions,
) -> Result<String, BoughError> {
    // An INJECTED reader is one store and is asked as one; the store-picking rule
    // below only has meaning when there is more than one store to pick between.
    let result = match &opts.keychain {
        Some(reader) => reader(keychain_ref.service.clone()).await,
        None => {
            let path = keychain_ref.path.clone();
            let pred = move |v: &str| holds_path(v, &path);
            read_from_stores(&keychain_ref.service, &default_stores(), Some(&pred)).await
        }
    };
    // `security -w` terminates its output with a newline that is not part of the
    // secret. Stripped HERE rather than in the reader so it holds for every reader: a
    // token with a newline welded on produces a header the remote end rejects for
    // reasons it will not explain, and that is a bad afternoon.
    let value = strip_one_newline(&result.value);
    if result.code != 0 || value.is_empty() {
        return Err(mcp(
            400,
            keychain_failure(
                &keychain_ref.service,
                result.code,
                &result.error,
                result.store,
            ),
        ));
    }
    if keychain_ref.path.is_empty() {
        return Ok(value);
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&value) else {
        return Err(mcp(
            400,
            format!(
                "the keychain item \"{}\" is not JSON, so #{} cannot be read out of it \
                 — drop the #path to use the whole item as the secret.",
                keychain_ref.service,
                keychain_ref.path.join(".")
            ),
        ));
    };
    let (found, container) = walk(&parsed, &keychain_ref.path);
    let found = match found {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(mcp(
                400,
                format!(
                    "the keychain item \"{}\" has no string at #{}. It holds: {}.",
                    keychain_ref.service,
                    keychain_ref.path.join("."),
                    describe(&parsed)
                ),
            ))
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
// Prefill
// ---------------------------------------------------------------------------

/// The item Claude Code keeps its login in.
pub const CLAUDE_CODE_ITEM: &str = "Claude Code-credentials";
/// …and the field the bearer token is at.
const CLAUDE_CODE_PATH: [&str; 2] = ["claudeAiOauth", "accessToken"];

/// Hosts the Claude Code credential BELONGS to.
///
/// Prefill happens without anybody pressing a key, so the question it has to answer is
/// not "would a token help here" but "may this server be told this secret". An MCP
/// server receives the Authorization header verbatim and is usually somebody else's
/// process on somebody else's machine.
const COVERED_HOSTS: [&str; 2] = ["claude.ai", "anthropic.com"];

/// The hostname of a URL, lowercased — or `None` when this is not a URL.
pub(crate) fn hostname(url: &str) -> Option<String> {
    let after_scheme = url.find("://")?;
    let scheme = &url[..after_scheme];
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return None;
    }
    let rest = &url[after_scheme + 3..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let host = if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) => &authority[1..end],
            None => return None,
        }
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

pub fn is_covered_host(url: &str) -> bool {
    let Some(host) = hostname(url) else {
        return false;
    };
    COVERED_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// The bearer token to START a covered server's connection with, or `None`.
///
/// PREFILL, not authorization. **Every failure here is silent.** That is the opposite
/// of the rule the rest of this module follows, and it is deliberate: a missing
/// `${keychain:…}` reference is a broken configuration and must be reported loudly; a
/// missing prefill is the ordinary state of a machine that never ran Claude Code.
pub async fn claude_code_prefill(url: &str, opts: &KeychainOptions) -> Option<String> {
    if !is_covered_host(url) {
        return None;
    }
    let keychain_ref = KeychainRef {
        service: CLAUDE_CODE_ITEM.to_string(),
        path: CLAUDE_CODE_PATH.iter().map(|s| s.to_string()).collect(),
    };
    read_keychain_ref(&keychain_ref, opts).await.ok()
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn mcp(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Mcp, message)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// An `expiresAt` sitting beside the token is a fact about the token, so it is
/// checked. Epoch milliseconds or an ISO string — both are common and neither is worth
/// guessing wrong about.
fn assert_fresh(
    keychain_ref: &KeychainRef,
    container: Option<&Value>,
    now: i64,
) -> Result<(), BoughError> {
    let Some(Value::Object(obj)) = container else {
        return Ok(());
    };
    let at = match obj.get("expiresAt") {
        Some(Value::Number(n)) => n.as_f64().map(|f| f as i64),
        Some(Value::String(s)) => parse_iso_ms(s),
        _ => None,
    };
    let Some(at) = at else { return Ok(()) };
    if at > now {
        return Ok(());
    }
    Err(mcp(
        400,
        format!(
            "the token in keychain item \"{}\" expired at {}. bough does not refresh a \
             credential it did not obtain — open the client that owns this item (for \
             \"Claude Code-credentials\", run `claude` once) and it will refresh it in place.",
            keychain_ref.service,
            iso_from_ms(at)
        ),
    ))
}

/// `Date.parse` for the shapes an `expiresAt` string actually takes (RFC 3339).
pub(crate) fn parse_iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// `new Date(ms).toISOString()`.
pub(crate) fn iso_from_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| format!("{ms}"))
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
        // A KEY THAT CONTAINS DOTS. `mcpOAuth."slack|mcp.example.com#a1b2"` is the
        // shape Claude Code stores a per-server OAuth grant under, and it is
        // unaddressable by splitting alone. Rejoining the remaining segments
        // longest-first finds it, and only ever runs when the plain segment missed, so
        // an exact key still wins and no existing reference changes meaning.
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

/// The item's SHAPE, for an error message. Never a value — this is a secret.
fn describe(parsed: &Value) -> String {
    match parsed {
        Value::Array(a) => format!("an array of {}", a.len()),
        Value::Object(o) if o.is_empty() => "an empty object".to_string(),
        Value::Object(o) => {
            format!(
                "an object with {}",
                o.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        }
        Value::String(_) => "string".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::Bool(_) => "boolean".to_string(),
        Value::Null => "object".to_string(), // `typeof null === "object"`
    }
}

/// What went wrong, in the user's terms, and in terms of the store that said so.
///
/// "NOT FOUND" NAMES BOTH STORES, because both were tried. A message that mentions
/// only the one that happened to answer last reads as though the other was never
/// looked at. Every other failure names one store, correctly: those are specific to it.
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
                 keychain, and {creds} does not hold it either. Check the name with \
                 `security find-generic-password -s \"{service}\"`. {advice}"
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
                "{head}: the keychain access prompt was denied or cancelled. macOS asks once \
                 per program — answer \"Always Allow\" to stop it asking again."
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
    //! Every test injects the reader, so nothing here spawns `security`, reads the
    //! developer's login keychain, or raises an access dialog. What is asserted is the
    //! part that can actually be wrong: which server a secret is allowed to reach,
    //! which credential wins when there are two, and whether a failure is loud or
    //! silent — those are opposite answers for the two paths and getting them the
    //! wrong way round is either a leak or a broken server.

    use super::*;
    use std::sync::Mutex;

    /// `CLAUDE_CONFIG_DIR` is process-global; the tests that move it take this.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn ok_reader(value: &str) -> KeychainReader {
        let value = value.to_string();
        reader_fn(move |_| {
            let value = value.clone();
            async move {
                KeychainResult {
                    value,
                    code: 0,
                    error: String::new(),
                    store: None,
                }
            }
        })
    }
    fn fails(code: i32, error: &str) -> KeychainReader {
        let error = error.to_string();
        reader_fn(move |_| {
            let error = error.clone();
            async move { KeychainResult::miss(code, error, None) }
        })
    }
    /// A store that HAS the item, tagged so the winner is identifiable.
    fn tagged(value: String, store: StoreKind) -> KeychainReader {
        reader_fn(move |_| {
            let value = value.clone();
            async move { KeychainResult::ok(value, Some(store)) }
        })
    }
    fn lacks(store: StoreKind, code: i32) -> KeychainReader {
        reader_fn(move |_| async move { KeychainResult::miss(code, "", Some(store)) })
    }

    /// What Claude Code stores, in the shape it stores it.
    fn blob(expires_at: i64) -> String {
        serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-TOKEN",
                "refreshToken": "sk-ant-ort01-REFRESH",
                "expiresAt": expires_at,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "max",
            }
        })
        .to_string()
    }
    fn fresh_blob() -> String {
        blob(now_ms() + 3_600_000)
    }
    fn parse(v: &str) -> KeychainRef {
        parse_keychain_ref(v).expect("a reference")
    }

    // ---- the reference ----------------------------------------------------

    #[test]
    fn a_reference_names_an_item_and_optionally_a_field_inside_it() {
        assert_eq!(
            parse_keychain_ref("${keychain:Claude Code-credentials}"),
            Some(KeychainRef {
                service: "Claude Code-credentials".into(),
                path: vec![]
            })
        );
        assert_eq!(
            parse_keychain_ref("${keychain:Claude Code-credentials#a.b}"),
            Some(KeychainRef {
                service: "Claude Code-credentials".into(),
                path: vec!["a".into(), "b".into()]
            })
        );
        // Not a reference: an ordinary header value, and an env reference, both of
        // which must fall through to the expansion that already existed.
        assert_eq!(parse_keychain_ref("Bearer abc"), None);
        assert_eq!(parse_keychain_ref("${TOKEN}"), None);
        assert_eq!(parse_keychain_ref("${keychain:}"), None);
    }

    #[tokio::test]
    async fn a_field_is_read_out_of_json_and_a_plain_item_is_used_whole() {
        let r = parse("${keychain:x#claudeAiOauth.accessToken}");
        let opts = KeychainOptions {
            keychain: Some(ok_reader(&fresh_blob())),
        };
        assert_eq!(
            read_keychain_ref(&r, &opts).await.unwrap(),
            "sk-ant-oat01-TOKEN"
        );
        let whole = parse("${keychain:x}");
        let opts = KeychainOptions {
            keychain: Some(ok_reader("plain-secret")),
        };
        assert_eq!(
            read_keychain_ref(&whole, &opts).await.unwrap(),
            "plain-secret"
        );
    }

    #[tokio::test]
    async fn a_key_with_dots_in_it_is_still_addressable() {
        let item = serde_json::json!({
            "mcpOAuth": {
                "slack|a1b2": { "accessToken": "plain-key-token" },
                "notion|mcp.notion.com|d4": { "accessToken": "dotted-key-token" },
            }
        })
        .to_string();
        let opts = KeychainOptions {
            keychain: Some(ok_reader(&item)),
        };
        let plain = parse("${keychain:x#mcpOAuth.slack|a1b2.accessToken}");
        assert_eq!(
            read_keychain_ref(&plain, &opts).await.unwrap(),
            "plain-key-token"
        );
        let dotted = parse("${keychain:x#mcpOAuth.notion|mcp.notion.com|d4.accessToken}");
        assert_eq!(
            read_keychain_ref(&dotted, &opts).await.unwrap(),
            "dotted-key-token"
        );
    }

    #[tokio::test]
    async fn an_exact_key_still_wins_over_a_rejoined_one() {
        let item = serde_json::json!({ "a": { "b": { "c": "nested" } }, "a.b": { "c": "flat" } })
            .to_string();
        let opts = KeychainOptions {
            keychain: Some(ok_reader(&item)),
        };
        assert_eq!(
            read_keychain_ref(&parse("${keychain:x#a.b.c}"), &opts)
                .await
                .unwrap(),
            "nested"
        );
    }

    #[tokio::test]
    async fn security_w_newline_is_not_part_of_the_secret() {
        let opts = KeychainOptions {
            keychain: Some(ok_reader("token\n")),
        };
        assert_eq!(
            read_keychain_ref(&parse("${keychain:x}"), &opts)
                .await
                .unwrap(),
            "token"
        );
    }

    #[tokio::test]
    async fn every_failure_names_the_item_and_says_what_to_do() {
        let r = parse("${keychain:Claude Code-credentials}");
        async fn message(r: &KeychainRef, reader: KeychainReader) -> String {
            let opts = KeychainOptions {
                keychain: Some(reader),
            };
            let err = read_keychain_ref(r, &opts).await.expect_err("a throw");
            assert_eq!(err.name(), "McpError");
            err.to_string()
        }
        let missing = message(&r, fails(44, "")).await;
        assert!(
            missing.contains("no generic-password item with that service name"),
            "{missing}"
        );
        assert!(missing.contains("Claude Code-credentials"), "{missing}");
        let denied = message(&r, fails(128, "")).await;
        assert!(
            denied.contains("prompt was denied or cancelled"),
            "{denied}"
        );
        let other = message(&r, fails(1, "boom")).await;
        assert!(other.contains("security exited 1 — boom"), "{other}");
    }

    #[tokio::test]
    async fn an_expired_token_is_reported_not_refreshed() {
        let r = parse("${keychain:Claude Code-credentials#claudeAiOauth.accessToken}");
        let stale = blob(now_ms() - 1_000);
        let opts = KeychainOptions {
            keychain: Some(ok_reader(&stale)),
        };
        let err = read_keychain_ref(&r, &opts)
            .await
            .expect_err("a throw")
            .to_string();
        assert!(err.contains("expired at"), "{err}");
        assert!(
            err.contains("does not refresh a credential it did not obtain"),
            "{err}"
        );
        assert!(err.contains("run `claude` once"), "{err}");
    }

    #[tokio::test]
    async fn an_error_never_contains_the_secret_only_the_items_shape() {
        let r = parse("${keychain:x#nope.missing}");
        let opts = KeychainOptions {
            keychain: Some(ok_reader(&fresh_blob())),
        };
        let err = read_keychain_ref(&r, &opts)
            .await
            .expect_err("a throw")
            .to_string();
        assert!(err.contains("has no string at #nope.missing"), "{err}");
        assert!(err.contains("an object with claudeAiOauth"), "{err}");
        assert!(!err.contains("sk-ant-"), "{err}");
    }

    #[tokio::test]
    async fn an_iso_expiry_string_is_understood_too() {
        let iso = iso_from_ms(now_ms() - 1_000);
        let item = serde_json::json!({ "t": { "accessToken": "x", "expiresAt": iso } }).to_string();
        let opts = KeychainOptions {
            keychain: Some(ok_reader(&item)),
        };
        let err = read_keychain_ref(&parse("${keychain:x#t.accessToken}"), &opts)
            .await
            .expect_err("a throw")
            .to_string();
        assert!(err.contains("expired at"), "{err}");
    }

    // ---- headers ----------------------------------------------------------

    #[tokio::test]
    async fn headers_resolve_at_send_time_keychain_env_and_plain_text() {
        use crate::mcp::config::{expand_headers, McpConfigOptions};
        use std::collections::BTreeMap;

        let headers: BTreeMap<String, String> = [
            (
                "Authorization",
                "Bearer ${keychain:Claude Code-credentials#claudeAiOauth.accessToken}",
            ),
            (
                "X-Token",
                "${keychain:Claude Code-credentials#claudeAiOauth.refreshToken}",
            ),
            ("X-Env", "${SOME_TOKEN}"),
            ("X-Static", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let cfg = McpConfigOptions {
            env: Some(Arc::new(|n: &str| {
                (n == "SOME_TOKEN").then(|| "from-env".to_string())
            })),
            ..Default::default()
        };
        let opts = KeychainOptions {
            keychain: Some(ok_reader(&fresh_blob())),
        };
        let out = expand_headers(&headers, &cfg, &opts).await.unwrap();
        assert_eq!(
            out.get("Authorization").map(String::as_str),
            Some("Bearer sk-ant-oat01-TOKEN")
        );
        assert_eq!(
            out.get("X-Token").map(String::as_str),
            Some("sk-ant-ort01-REFRESH")
        );
        assert_eq!(out.get("X-Env").map(String::as_str), Some("from-env"));
        assert_eq!(out.get("X-Static").map(String::as_str), Some("1"));
    }

    // ---- prefill ----------------------------------------------------------

    #[test]
    fn prefill_is_confined_to_hosts_the_credential_belongs_to() {
        assert!(is_covered_host("https://mcp.claude.ai/mcp"));
        assert!(is_covered_host("https://claude.ai/api/mcp"));
        assert!(is_covered_host("https://api.anthropic.com/v1/mcp"));
        assert!(!is_covered_host("https://mcp.linear.app/sse"));
        assert!(!is_covered_host("https://claude.ai.evil.example/mcp"));
        assert!(!is_covered_host("not a url"));
    }

    #[tokio::test]
    async fn prefill_answers_for_a_covered_host_and_stays_silent_everywhere_else() {
        let opts = KeychainOptions {
            keychain: Some(ok_reader(&fresh_blob())),
        };
        assert_eq!(
            claude_code_prefill("https://mcp.claude.ai/mcp", &opts)
                .await
                .as_deref(),
            Some("sk-ant-oat01-TOKEN")
        );
        assert_eq!(
            claude_code_prefill("https://mcp.linear.app/sse", &opts).await,
            None
        );
    }

    #[tokio::test]
    async fn a_missing_or_stale_prefill_is_silent() {
        // The opposite rule from the explicit reference above, deliberately.
        let url = "https://mcp.claude.ai/mcp";
        let missing = KeychainOptions {
            keychain: Some(fails(44, "")),
        };
        assert_eq!(claude_code_prefill(url, &missing).await, None);
        let junk = KeychainOptions {
            keychain: Some(ok_reader("not json")),
        };
        assert_eq!(claude_code_prefill(url, &junk).await, None);
        let stale = KeychainOptions {
            keychain: Some(ok_reader(&blob(now_ms() - 1))),
        };
        assert_eq!(claude_code_prefill(url, &stale).await, None);
        assert_eq!(CLAUDE_CODE_ITEM, "Claude Code-credentials");
    }

    // ---- the store the reference resolves against -------------------------

    #[test]
    fn the_config_directory_is_claude_config_dir_when_set_else_dot_claude() {
        let none = |_: &str| None;
        let set = |n: &str| (n == "CLAUDE_CONFIG_DIR").then(|| "/elsewhere".to_string());
        let blank = |n: &str| (n == "CLAUDE_CONFIG_DIR").then(|| "  ".to_string());
        let home = Path::new("/home/t");
        assert_eq!(
            claude_config_dir_from(&none, home),
            PathBuf::from("/home/t/.claude")
        );
        assert_eq!(
            claude_config_dir_from(&set, home),
            PathBuf::from("/elsewhere")
        );
        // Blank is not a location. Treating it as one moves the read to a relative path.
        assert_eq!(
            claude_config_dir_from(&blank, home),
            PathBuf::from("/home/t/.claude")
        );
        assert_eq!(
            credentials_path_from(&none, home),
            PathBuf::from("/home/t/.claude/.credentials.json")
        );
    }

    /// Runs `f` with `CLAUDE_CONFIG_DIR` pointed at a fresh directory.
    fn with_config_dir<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("bough-creds-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let before = std::env::var("CLAUDE_CONFIG_DIR").ok();
        std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
        let out = f(&dir);
        match before {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn off_macos_the_credential_comes_out_of_claude_codes_credentials_file() {
        // The whole reason this store exists: there is no login keychain on Linux, so
        // a `${keychain:…}` reference had nothing to resolve against.
        let rt = tokio::runtime::Runtime::new().unwrap();
        with_config_dir(|dir| {
            std::fs::write(dir.join(".credentials.json"), fresh_blob()).unwrap();
            rt.block_on(async {
                let result = credentials_file_read(CLAUDE_CODE_ITEM).await;
                assert_eq!(result.code, 0);
                assert_eq!(result.store, Some(StoreKind::File));
                let opts = KeychainOptions {
                    keychain: Some(credentials_file_reader()),
                };
                let r = KeychainRef {
                    service: CLAUDE_CODE_ITEM.into(),
                    path: vec!["claudeAiOauth".into(), "accessToken".into()],
                };
                assert_eq!(
                    read_keychain_ref(&r, &opts).await.unwrap(),
                    "sk-ant-oat01-TOKEN"
                );
            });
        });
    }

    #[test]
    fn the_credentials_file_answers_for_one_item_not_as_a_general_vault() {
        // A reference naming some other service must not be handed Claude Code's login.
        let rt = tokio::runtime::Runtime::new().unwrap();
        with_config_dir(|dir| {
            std::fs::write(dir.join(".credentials.json"), fresh_blob()).unwrap();
            rt.block_on(async {
                let other = credentials_file_read("Some Other App").await;
                assert_eq!(other.code, 44);
                assert_eq!(other.value, "");
            });
        });
    }

    #[test]
    fn an_absent_credentials_file_reads_as_absent_and_says_where_it_looked() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        with_config_dir(|_| {
            rt.block_on(async {
                assert_eq!(credentials_file_read(CLAUDE_CODE_ITEM).await.code, 44);
                let opts = KeychainOptions {
                    keychain: Some(credentials_file_reader()),
                };
                let r = KeychainRef {
                    service: CLAUDE_CODE_ITEM.into(),
                    path: vec!["claudeAiOauth".into(), "accessToken".into()],
                };
                let err = read_keychain_ref(&r, &opts)
                    .await
                    .expect_err("a throw")
                    .to_string();
                // The advice has to be true for the store that actually answered.
                assert!(err.contains(".credentials.json"), "{err}");
                assert!(err.contains("CLAUDE_CONFIG_DIR"), "{err}");
                assert!(!err.contains("generic-password"), "{err}");
            });
        });
    }

    // ---- both setups, on either platform ----------------------------------

    #[tokio::test]
    async fn either_store_can_be_the_one_that_answers_whichever_platform_this_is() {
        let file_only = read_from_stores(
            CLAUDE_CODE_ITEM,
            &[
                lacks(StoreKind::Keychain, 44),
                tagged(fresh_blob(), StoreKind::File),
            ],
            None,
        )
        .await;
        assert_eq!(file_only.store, Some(StoreKind::File));
        assert_eq!(file_only.code, 0);

        let keychain_only = read_from_stores(
            CLAUDE_CODE_ITEM,
            &[
                lacks(StoreKind::File, 44),
                tagged(fresh_blob(), StoreKind::Keychain),
            ],
            None,
        )
        .await;
        assert_eq!(keychain_only.store, Some(StoreKind::Keychain));
        assert_eq!(keychain_only.code, 0);
    }

    #[tokio::test]
    async fn ordering_is_by_authority_not_availability() {
        assert_eq!(
            store_order("darwin"),
            [StoreKind::Keychain, StoreKind::File]
        );
        assert_eq!(store_order("macos"), [StoreKind::Keychain, StoreKind::File]);
        assert_eq!(store_order("linux"), [StoreKind::File, StoreKind::Keychain]);
        assert_eq!(store_order("win32"), [StoreKind::File, StoreKind::Keychain]);
        // …and `credential_stores` really returns THOSE readers, in that order — asked
        // for a service neither store holds, each one tags its own answer.
        for platform in ["darwin", "linux", "win32"] {
            let stores = credential_stores(platform);
            assert_eq!(stores.len(), 2, "{platform}");
            let mut kinds = Vec::new();
            for s in &stores {
                kinds.push(s("bough-test-no-such-item-58a98873".into()).await.store);
            }
            let want: Vec<_> = store_order(platform).into_iter().map(Some).collect();
            assert_eq!(kinds, want, "{platform}");
        }
    }

    /// The real split: the keychain kept the login, the file kept the MCP grants.
    const GRANT_KEY: &str = "notion|eac663db915250e7";
    fn with_grants(store: StoreKind) -> KeychainReader {
        let value = serde_json::json!({
            "mcpOAuth": { GRANT_KEY: { "accessToken": "grant-token", "serverName": "notion" } }
        })
        .to_string();
        tagged(value, store)
    }

    #[tokio::test]
    async fn the_store_that_has_the_field_wins_not_the_store_that_has_the_item() {
        // On this developer's Mac the keychain item holds `claudeAiOauth` alone while
        // the `mcpOAuth` grants live in `.credentials.json`. Under "first store with
        // bytes wins" the keychain answers with a valid blob that cannot contain the
        // reference's path, and the token in the next store along is never reached.
        let r = parse(&format!(
            "${{keychain:{CLAUDE_CODE_ITEM}#mcpOAuth.{GRANT_KEY}.accessToken}}"
        ));
        let path = r.path.clone();
        let pred = move |v: &str| holds_path(v, &path);
        let picked = read_from_stores(
            CLAUDE_CODE_ITEM,
            &[
                tagged(fresh_blob(), StoreKind::Keychain),
                with_grants(StoreKind::File),
            ],
            Some(&pred),
        )
        .await;
        assert_eq!(picked.store, Some(StoreKind::File));
        let v: Value = serde_json::from_str(&picked.value).unwrap();
        assert_eq!(v["mcpOAuth"][GRANT_KEY]["accessToken"], "grant-token");
    }

    #[tokio::test]
    async fn a_whole_item_reference_still_takes_the_first_store_with_bytes() {
        // No path means there is nothing to look inside for, so the authority ordering
        // is the whole rule and the second store must not get a say.
        let pred = |v: &str| holds_path(v, &[]);
        let picked = read_from_stores(
            CLAUDE_CODE_ITEM,
            &[
                tagged(fresh_blob(), StoreKind::Keychain),
                with_grants(StoreKind::File),
            ],
            Some(&pred),
        )
        .await;
        assert_eq!(picked.store, Some(StoreKind::Keychain));
    }

    #[tokio::test]
    async fn when_no_store_holds_the_field_the_error_names_what_was_actually_found() {
        let path: Vec<String> = vec!["mcpOAuth".into(), GRANT_KEY.into(), "accessToken".into()];
        let p2 = path.clone();
        let reader = reader_fn(move |service: String| {
            let p2 = p2.clone();
            async move {
                let pred = move |v: &str| holds_path(v, &p2);
                read_from_stores(
                    &service,
                    &[
                        tagged(fresh_blob(), StoreKind::Keychain),
                        tagged(fresh_blob(), StoreKind::File),
                    ],
                    Some(&pred),
                )
                .await
            }
        });
        let r = KeychainRef {
            service: CLAUDE_CODE_ITEM.into(),
            path,
        };
        let opts = KeychainOptions {
            keychain: Some(reader),
        };
        let err = read_keychain_ref(&r, &opts)
            .await
            .expect_err("a throw")
            .to_string();
        assert!(err.contains("has no string at #mcpOAuth"), "{err}");
        assert!(err.contains("an object with claudeAiOauth"), "{err}");
    }

    #[tokio::test]
    async fn a_specific_failure_beats_a_bare_absence_when_neither_store_has_it() {
        let denied = read_from_stores(
            CLAUDE_CODE_ITEM,
            &[lacks(StoreKind::File, 44), lacks(StoreKind::Keychain, 128)],
            None,
        )
        .await;
        assert_eq!(denied.code, 128);
        assert_eq!(denied.store, Some(StoreKind::Keychain));
    }

    #[tokio::test]
    async fn not_found_names_both_stores_since_both_were_tried() {
        let reader = reader_fn(|service: String| async move {
            read_from_stores(
                &service,
                &[lacks(StoreKind::File, 44), lacks(StoreKind::Keychain, 44)],
                None,
            )
            .await
        });
        let r = KeychainRef {
            service: CLAUDE_CODE_ITEM.into(),
            path: vec![],
        };
        let opts = KeychainOptions {
            keychain: Some(reader),
        };
        let err = read_keychain_ref(&r, &opts)
            .await
            .expect_err("a throw")
            .to_string();
        assert!(err.contains(".credentials.json"), "{err}");
        assert!(err.contains("keychain"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_security_binary_is_an_absent_store_not_an_error() {
        // It reports 44 so the OTHER store still gets asked. A bogus service name gives
        // the same 44 on a Mac where the binary does exist, so this holds on both
        // platforms.
        let result = security_read("bough-test-no-such-item-58a98873").await;
        assert_eq!(result.code, 44);
        assert_eq!(result.store, Some(StoreKind::Keychain));
        assert_eq!(result.value, "");
    }
}
