//! OAuth for remote MCP servers, and the callback bough hosts for it (port of
//! `src/mcp/oauth.ts`).
//!
//! THE INVARIANT THIS HOLDS: **an unauthorized server is a QUESTION, never a failure
//! and never a hang.** Everything here exists so that a remote server answering 401
//! turns into one sentence a human can act on — "not authorized — open the mcp panel
//! (^p) and press a" — and one URL they can open.
//!
//! **bough is a PUBLIC client and hosts its own redirect.**
//! `token_endpoint_auth_method` is `"none"` — there is no client secret to keep, PKCE
//! carries the proof — and the authorization server sends the browser back to
//! `GET /mcp/oauth/callback` on bough's own port. No shim binary, no second listener,
//! no cloud redirect.
//!
//! **The provider captures, it does not redirect.** `redirect_to_authorization`
//! stores the URL instead of opening it. A headless server that shells out to a
//! browser is a server that hangs when there is no browser, and the model must never
//! be handed a URL to "click".
//!
//! **Credentials are per server, private, and outside the model's reach.**
//! `~/.bough/mcp/tokens/<server>.json`, dir 0700, file 0600.
//!
//! **The `state` round-trip binds a callback to the server that started it.** The
//! nonce is minted per flow and stored as `<server>.<nonce>`; the callback splits it,
//! matches the stored nonce, and refuses otherwise.
//!
//! PORT NOTE. The TS drives the MCP SDK's `auth()`; there is no such crate here, so
//! the flow itself — RFC 9728 protected-resource discovery, RFC 8414 authorization-
//! server metadata, RFC 7591 dynamic client registration, PKCE (S256), the code
//! exchange and the refresh — is hand-rolled in [`flow`] below. It is four requests;
//! the provider semantics (stored-wins, prefill, invalidate scopes, capture-not-
//! navigate) are the part that is bough's and they are preserved exactly.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::errors::{BoughError, ErrorKind};
use crate::paths::{bough_path, confine};

fn mcp(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Mcp, message)
}

// ---------------------------------------------------------------------------
// Where the callback lives
// ---------------------------------------------------------------------------

/// The port the callback URL advertises. Set once at boot from the port the listener
/// actually bound, because the redirect URI is registered with the authorization
/// server and baked into the authorization request: if it names a port nothing is
/// listening on, the user approves access in their browser and lands on a connection
/// error with no way back.
///
/// Process-level rather than injected, and this is the one place it is justified: the
/// value is a property of the PROCESS, and every reader must agree with the socket.
static CONFIGURED_PORT: Mutex<Option<u16>> = Mutex::new(None);

/// Boot wiring: pin the callback to the port the server actually bound.
pub fn configure_oauth_callback(port: u16) {
    *CONFIGURED_PORT.lock().unwrap_or_else(|e| e.into_inner()) = Some(port);
}

/// The port the callback URL names.
pub fn callback_port() -> u16 {
    if let Some(p) = *CONFIGURED_PORT.lock().unwrap_or_else(|e| e.into_inner()) {
        return p;
    }
    std::env::var("BOUGH_PORT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(4321)
}

/// The redirect target — bough's own HTTP surface, loopback only.
pub fn callback_url() -> String {
    format!("http://127.0.0.1:{}{}", callback_port(), CALLBACK_PATH)
}

/// The path half of [`callback_url`], so the route entry and the URL agree.
pub const CALLBACK_PATH: &str = "/mcp/oauth/callback";

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// Registry names are slugs (`mcp/config.rs` owns that rule). Restated here rather
/// than imported because this is a different job: config validates what may be
/// WRITTEN to the registry, and this validates what may become a FILENAME. The
/// callback's `state` parameter arrives from a browser, so the server name in it is
/// untrusted input steering the server's own path construction ([`confine`] is the
/// second half of the same guard).
fn is_slug(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn assert_server_name(server: &str) -> Result<(), BoughError> {
    if is_slug(server) {
        return Ok(());
    }
    Err(mcp(
        400,
        format!(
            "{} is not a valid MCP server name — names are lowercase slugs (a-z, 0-9, - \
             and _, starting with a letter or digit). Nothing was read or written.",
            serde_json::to_string(server).unwrap_or_else(|_| format!("{server:?}"))
        ),
    ))
}

/// A dynamic client registration, or a pre-registered app. Extra keys the
/// authorization server returned are carried through untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// What a token endpoint returned. `expires_in` is relative; the absolute
/// [`Stored::expires_at`] is derived from it at save.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub token_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Everything one server's flow needs to survive a restart.
///
/// Read leniently, field by field (see [`Stored::from_value`]): a token file written
/// by the TypeScript build carries the SDK's own `discovery` shape, and a document
/// this build cannot fully understand must still yield its tokens.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stored {
    /// Dynamic client registration — survives a token clear; re-registering is
    /// wasteful.
    pub client: Option<ClientInfo>,
    pub tokens: Option<OAuthTokens>,
    /// Absolute ms when the access token expires, derived from `expires_in` at save.
    pub expires_at: Option<i64>,
    /// In-flight PKCE verifier; consumed by the code exchange.
    pub code_verifier: Option<String>,
    /// In-flight authorization nonce; cleared when tokens land.
    pub state: Option<String>,
    /// Cached RFC 9728 / RFC 8414 discovery, so a reconnect re-probes nothing. Held
    /// as raw JSON so a document written by another build round-trips intact.
    pub discovery: Option<Value>,
    /// Anything else the file held, preserved across writes.
    pub extra: Map<String, Value>,
}

impl Stored {
    fn from_value(v: Value) -> Self {
        let Value::Object(mut o) = v else {
            return Self::default();
        };
        let take = |o: &mut Map<String, Value>, k: &str| o.remove(k).filter(|v| !v.is_null());
        let client = take(&mut o, "client").and_then(|v| serde_json::from_value(v).ok());
        let tokens = take(&mut o, "tokens").and_then(|v| serde_json::from_value(v).ok());
        let expires_at = take(&mut o, "expiresAt")
            .and_then(|v| v.as_f64())
            .map(|f| f as i64);
        let code_verifier =
            take(&mut o, "codeVerifier").and_then(|v| v.as_str().map(|s| s.to_string()));
        let state = take(&mut o, "state").and_then(|v| v.as_str().map(|s| s.to_string()));
        let discovery = take(&mut o, "discovery");
        Self {
            client,
            tokens,
            expires_at,
            code_verifier,
            state,
            discovery,
            extra: o,
        }
    }

    fn to_value(&self) -> Value {
        let mut o = self.extra.clone();
        let mut put = |k: &str, v: Option<Value>| match v {
            Some(v) => {
                o.insert(k.to_string(), v);
            }
            None => {
                o.remove(k);
            }
        };
        put(
            "client",
            self.client
                .as_ref()
                .and_then(|c| serde_json::to_value(c).ok()),
        );
        put(
            "tokens",
            self.tokens
                .as_ref()
                .and_then(|t| serde_json::to_value(t).ok()),
        );
        put("expiresAt", self.expires_at.map(|n| json!(n)));
        put(
            "codeVerifier",
            self.code_verifier.as_ref().map(|s| json!(s)),
        );
        put("state", self.state.as_ref().map(|s| json!(s)));
        put("discovery", self.discovery.clone());
        Value::Object(o)
    }
}

/// Where token files live. Absent = `~/.bough/mcp/tokens`. Injected in tests.
#[derive(Debug, Clone, Default)]
pub struct TokenStoreOptions {
    pub dir: Option<PathBuf>,
}

/// `~/.bough/mcp/tokens` — one file per server, never one file for all of them.
pub fn default_tokens_dir() -> PathBuf {
    bough_path(&["mcp", "tokens"])
}

/// Per-server credential files. Synchronous on purpose: a store that cannot lose a
/// write to an interleaving is worth more here than the microseconds an async read
/// would save.
#[derive(Debug, Clone)]
pub struct TokenStore {
    pub dir: PathBuf,
}

impl TokenStore {
    pub fn new(opts: &TokenStoreOptions) -> Self {
        Self {
            dir: opts.dir.clone().unwrap_or_else(default_tokens_dir),
        }
    }

    /// The file one server's credentials live in. Confined to `dir`.
    pub fn file_for(&self, server: &str) -> Result<PathBuf, BoughError> {
        assert_server_name(server)?;
        confine(&self.dir, std::path::Path::new(&format!("{server}.json")))
    }

    /// Everything stored for `server`. Absent or unreadable = nothing stored.
    ///
    /// A corrupt credential file must fail CLOSED — as "not authorized", which the
    /// human can fix with one command — rather than as a parse error in the middle of
    /// a turn. A missing file is the ordinary case and reads the same. A bad NAME is
    /// not a missing file and still throws.
    pub fn load(&self, server: &str) -> Result<Stored, BoughError> {
        let file = self.file_for(server)?;
        let Ok(raw) = std::fs::read_to_string(&file) else {
            return Ok(Stored::default());
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
            return Ok(Stored::default());
        };
        Ok(Stored::from_value(parsed))
    }

    /// Replace the whole document. Creates the directory 0700, the file 0600.
    pub fn write(&self, server: &str, stored: &Stored) -> Result<(), BoughError> {
        let file = self.file_for(server)?;
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| mcp(500, format!("could not create {}: {e}", self.dir.display())))?;
        // mkdir's mode is umask-masked; this is not.
        let _ = set_mode(&self.dir, 0o700);
        let body = serde_json::to_string_pretty(&stored.to_value()).unwrap_or_default() + "\n";
        std::fs::write(&file, body)
            .map_err(|e| mcp(500, format!("could not write {}: {e}", file.display())))?;
        let _ = set_mode(&file, 0o600);
        Ok(())
    }

    /// Merge a delta into what is stored.
    pub fn patch(&self, server: &str, delta: impl FnOnce(&mut Stored)) -> Result<(), BoughError> {
        let mut stored = self.load(server)?;
        delta(&mut stored);
        self.write(server, &stored)
    }

    /// Forget everything for one server ("logout"). Returns whether there was any.
    pub fn clear(&self, server: &str) -> bool {
        match self.file_for(server) {
            Ok(file) => std::fs::remove_file(file).is_ok(),
            Err(_) => false,
        }
    }
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}
#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// A clock, injected so a token-expiry assertion needs no sleeping.
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Where the REGISTRY is read from, for the pre-registered-client fallback and the
/// remote-URL lookup.
///
/// Injected for the same reason `dir` is: a provider test must not read the
/// developer's own `~/.bough/mcp.json`, and it carries the `env` lookup that resolves
/// a `${VAR}` secret without touching the real environment. Production call sites
/// pass [`RegistryAccess::default`] and get the real registry through
/// [`registry_bridge`].
#[derive(Clone, Default)]
pub struct RegistryAccess {
    /// `(name) -> the entry`, or `None` when unregistered. The entry's
    /// `clientSecret` is returned ALREADY EXPANDED, so an unset `${VAR}` surfaces as
    /// config's own message rather than an opaque 401 from the token endpoint.
    #[allow(clippy::type_complexity)]
    pub lookup:
        Option<Arc<dyn Fn(&str) -> Result<Option<RegistryEntry>, BoughError> + Send + Sync>>,
    /// `(name, url) -> ()` — rewrites one entry's `url`, carrying every other field
    /// through. Used by the resource-redeclaration correction.
    #[allow(clippy::type_complexity)]
    pub set_url: Option<Arc<dyn Fn(&str, &str) -> Result<(), BoughError> + Send + Sync>>,
}

impl std::fmt::Debug for RegistryAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryAccess").finish_non_exhaustive()
    }
}

/// The three registry fields OAuth reads off an entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegistryEntry {
    /// Remote servers only. A stdio entry has none, and has no OAuth.
    pub url: Option<String>,
    pub client_id: Option<String>,
    /// Already expanded from its `${VAR}` reference.
    pub client_secret: Option<String>,
}

impl RegistryAccess {
    fn get(&self, name: &str) -> Result<Option<RegistryEntry>, BoughError> {
        match &self.lookup {
            Some(f) => f(name),
            None => registry_bridge::get_entry(name),
        }
    }
    fn write_url(&self, name: &str, url: &str) -> Result<(), BoughError> {
        match &self.set_url {
            Some(f) => f(name, url),
            None => registry_bridge::set_url(name, url),
        }
    }
}

#[derive(Clone, Default)]
pub struct ProviderOptions {
    /// Where token files live. Absent = `~/.bough/mcp/tokens`.
    pub dir: Option<PathBuf>,
    /// Override the redirect URI. Absent = [`callback_url`].
    pub redirect_url: Option<String>,
    /// Clock, injected so a token-expiry assertion needs no sleeping.
    pub now: Option<NowFn>,
    /// Where the registry is read from, for the pre-registered-client fallback.
    pub config: RegistryAccess,
    /// A bearer token to fall back on when this server has none of its own yet —
    /// `keychain.rs`'s prefill, resolved by the caller because reading it is async.
    ///
    /// PREFILL, and only that. The moment a real flow stores tokens for this server,
    /// those win. Nothing is written to the token store from here — the secret
    /// belongs to the client that obtained it and bough keeps exactly one copy of it,
    /// in the keychain where it already was.
    pub prefill: Option<String>,
}

impl std::fmt::Debug for ProviderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderOptions")
            .field("dir", &self.dir)
            .field("redirect_url", &self.redirect_url)
            .field("prefill", &self.prefill.as_ref().map(|_| "<token>"))
            .finish_non_exhaustive()
    }
}

/// bough's OAuth client provider: persistence plus one refusal.
///
/// The refusal is `redirect_to_authorization`, which captures rather than navigates.
/// Everything else is storage, and every method is written so that a half-finished
/// flow leaves the previous state alone: saving tokens keeps the registration and
/// drops the nonce, invalidating tokens keeps the registration, and only an explicit
/// `clear_auth` / `invalidate_credentials("all")` throws it away.
pub struct BoughOAuthProvider {
    pub server: String,
    /// Set when the flow wanted the user agent sent somewhere. Captured, not followed.
    authorization_url: Mutex<Option<String>>,
    store: TokenStore,
    redirect_url: Option<String>,
    now: NowFn,
    config: RegistryAccess,
    prefill: Option<String>,
}

/// The scopes [`BoughOAuthProvider::invalidate_credentials`] understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidateScope {
    All,
    Client,
    Tokens,
    Verifier,
    Discovery,
}

impl BoughOAuthProvider {
    pub fn new(server: &str, opts: &ProviderOptions) -> Result<Self, BoughError> {
        assert_server_name(server)?;
        Ok(Self {
            server: server.to_string(),
            authorization_url: Mutex::new(None),
            store: TokenStore::new(&TokenStoreOptions {
                dir: opts.dir.clone(),
            }),
            redirect_url: opts.redirect_url.clone(),
            now: opts.now.clone().unwrap_or_else(|| Arc::new(now_ms)),
            config: opts.config.clone(),
            prefill: opts.prefill.clone(),
        })
    }

    pub fn authorization_url(&self) -> Option<String> {
        self.authorization_url
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn redirect_url(&self) -> String {
        self.redirect_url.clone().unwrap_or_else(callback_url)
    }

    /// Public client. There is no secret to store, and PKCE carries the proof.
    pub fn client_metadata(&self) -> Value {
        json!({
            "client_name": "bough",
            "redirect_uris": [self.redirect_url()],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        })
    }

    /// `<server>.<nonce>` — the callback needs both halves.
    pub fn state(&self) -> Result<String, BoughError> {
        let nonce = uuid::Uuid::new_v4().to_string();
        self.store
            .patch(&self.server, |s| s.state = Some(nonce.clone()))?;
        Ok(format!("{}.{}", self.server, nonce))
    }

    /// The OAuth client to authorize as, dynamically registered or pre-registered.
    ///
    /// A STORED client WINS. One that came back from a real registration is the one
    /// the authorization server issued and knows; a static id is what to fall back on
    /// when there was never a registration to do.
    ///
    /// Read FRESH rather than cached at construction: the panel can write a
    /// `clientId` onto the entry between one attempt and the next, and the second
    /// press of `a` has to see it.
    pub fn client_information(&self) -> Result<Option<ClientInfo>, BoughError> {
        if let Some(stored) = self.store.load(&self.server)?.client {
            return Ok(Some(stored));
        }
        let Some(entry) = self.config.get(&self.server)? else {
            return Ok(None);
        };
        let Some(client_id) = entry.client_id else {
            return Ok(None);
        };
        Ok(Some(ClientInfo {
            client_id,
            client_secret: entry.client_secret,
            extra: Map::new(),
        }))
    }

    pub fn save_client_information(&self, client: ClientInfo) -> Result<(), BoughError> {
        self.store.patch(&self.server, |s| s.client = Some(client))
    }

    /// This server's tokens: the ones bough's own flow stored, or the prefill.
    ///
    /// STORED WINS, always. A user who ran `a` and completed an authorization has
    /// said something specific about how this server should be reached, and a
    /// credential that merely happens to be on the machine must never quietly
    /// displace it.
    pub fn tokens(&self) -> Result<Option<OAuthTokens>, BoughError> {
        if let Some(stored) = self.store.load(&self.server)?.tokens {
            return Ok(Some(stored));
        }
        Ok(self.prefill.as_ref().map(|t| OAuthTokens {
            access_token: t.clone(),
            token_type: "Bearer".to_string(),
            ..Default::default()
        }))
    }

    pub fn save_tokens(&self, tokens: OAuthTokens) -> Result<(), BoughError> {
        let expires_at = tokens
            .expires_in
            .map(|s| (self.now)() + (s * 1000.0) as i64);
        self.store.patch(&self.server, |s| {
            s.tokens = Some(tokens);
            s.expires_at = expires_at;
            // Tokens landing means the in-flight authorization finished. Dropping the
            // nonce and the verifier is what stops a replayed callback from
            // exchanging the same code twice.
            s.state = None;
            s.code_verifier = None;
        })
    }

    pub fn redirect_to_authorization(&self, url: &str) {
        *self
            .authorization_url
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(url.to_string());
    }

    pub fn save_code_verifier(&self, code_verifier: &str) -> Result<(), BoughError> {
        self.store.patch(&self.server, |s| {
            s.code_verifier = Some(code_verifier.to_string())
        })
    }

    pub fn code_verifier(&self) -> Result<String, BoughError> {
        match self.store.load(&self.server)?.code_verifier {
            Some(v) if !v.is_empty() => Ok(v),
            _ => Err(mcp(
                400,
                format!(
                    "no PKCE verifier is stored for \"{s}\", so this authorization cannot be \
                     completed — it was started by a different process or already finished. \
                     Open the mcp panel (^p) and press a on {s} to start a fresh one.",
                    s = self.server
                ),
            )),
        }
    }

    /// The recovery hook, and the reason an expired refresh token degrades into an
    /// authorization prompt instead of an OAuth stack trace: the flow catches the
    /// rejected grant, calls this with `Tokens`, and retries — which now finds no
    /// refresh token and starts a fresh authorization, returning REDIRECT.
    pub fn invalidate_credentials(&self, scope: InvalidateScope) -> Result<(), BoughError> {
        if scope == InvalidateScope::All {
            self.store.clear(&self.server);
            return Ok(());
        }
        self.store.patch(&self.server, |s| match scope {
            InvalidateScope::Client => s.client = None,
            InvalidateScope::Tokens => {
                s.tokens = None;
                s.expires_at = None;
            }
            InvalidateScope::Verifier => s.code_verifier = None,
            InvalidateScope::Discovery => s.discovery = None,
            InvalidateScope::All => unreachable!("handled above"),
        })
    }

    pub fn save_discovery_state(&self, discovery: Value) -> Result<(), BoughError> {
        self.store
            .patch(&self.server, |s| s.discovery = Some(discovery))
    }

    pub fn discovery_state(&self) -> Option<Value> {
        self.store.load(&self.server).ok().and_then(|s| s.discovery)
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// What `/mcp` shows next to a remote server, and what the catalog reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub server: String,
    /// Something is stored that the transport can present or refresh.
    pub authorized: bool,
    /// The access token is past its expiry — refreshable, not broken.
    pub expired: bool,
    /// A refresh token is stored, so an expiry heals itself inside the transport.
    pub refreshable: bool,
    /// Where the browser comes back to, so the panel can say it.
    pub callback: String,
}

pub fn auth_status(
    server: &str,
    opts: &TokenStoreOptions,
    now: Option<&NowFn>,
) -> Result<AuthStatus, BoughError> {
    let stored = TokenStore::new(opts).load(server)?;
    let now = match now {
        Some(f) => f(),
        None => now_ms(),
    };
    Ok(AuthStatus {
        server: server.to_string(),
        authorized: stored.tokens.is_some(),
        expired: stored.expires_at.is_some_and(|at| at <= now),
        refreshable: stored
            .tokens
            .as_ref()
            .and_then(|t| t.refresh_token.as_ref())
            .is_some(),
        callback: callback_url(),
    })
}

/// Whether anything is stored for `server`.
pub fn has_tokens(server: &str, opts: &TokenStoreOptions) -> bool {
    TokenStore::new(opts)
        .load(server)
        .map(|s| s.tokens.is_some())
        .unwrap_or(false)
}

/// Forget a server's registration and tokens ("logout").
pub fn clear_auth(server: &str, opts: &TokenStoreOptions) -> bool {
    TokenStore::new(opts).clear(server)
}

// ---------------------------------------------------------------------------
// The flow
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStart {
    /// `"authorized"` or `"redirect"`.
    pub status: String,
    pub server: String,
    /// Present for "redirect": the URL the human must open to approve access.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    /// Present when the registry's `url` was corrected from the server's own
    /// advertised resource.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrected_url: Option<String>,
}

/// One HTTP round trip, injectable so the flow's tests need no socket.
#[derive(Debug, Clone)]
pub struct HttpReq {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HttpRes {
    pub status: u16,
    /// Lowercased names. `remote.rs` reads `mcp-session-id` and `content-type` off
    /// them; the OAuth flow reads none.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpRes {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// HTTP for discovery, registration and token exchange. Injected in tests.
pub type FetchFn =
    Arc<dyn Fn(HttpReq) -> BoxFuture<'static, Result<HttpRes, String>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct AuthFlowOptions {
    pub provider: ProviderOptions,
    /// HTTP for discovery, registration and token exchange.
    pub fetch: Option<FetchFn>,
    /// Per-request deadline for the flow's HTTP. Absent = [`AUTH_HTTP_MS`].
    pub timeout_ms: Option<u64>,
}

/// How long any one request in the flow may take. The flow is three or four round
/// trips to a server bough does not control, reached from an HTTP handler a human is
/// waiting on: unbounded, an authorization server that accepts a connection and
/// stalls parks that request forever, which is the same hang `remote.rs` refuses on
/// the JSON-RPC channel.
pub const AUTH_HTTP_MS: u64 = 15_000;

/// `fetch` with a deadline, wrapping whatever was injected.
fn bounded_fetch(base: Option<FetchFn>, timeout_ms: u64) -> FetchFn {
    match base {
        Some(inner) => inner,
        None => Arc::new(move |req: HttpReq| {
            Box::pin(async move { reqwest_once(req, timeout_ms).await })
                as BoxFuture<'static, Result<HttpRes, String>>
        }),
    }
}

async fn reqwest_once(req: HttpReq, timeout_ms: u64) -> Result<HttpRes, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| e.to_string())?;
    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .map_err(|e| format!("bad method: {e}"))?;
    let mut b = client.request(method, &req.url);
    for (k, v) in &req.headers {
        b = b.header(k, v);
    }
    if let Some(body) = req.body {
        b = b.body(body);
    }
    let res = b.send().await.map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let headers = res
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let body = res.text().await.map_err(|e| e.to_string())?;
    Ok(HttpRes {
        status,
        headers,
        body,
    })
}

/// Turn whatever escaped the flow into a sentence naming the server and the move.
fn auth_failure(server: &str, server_url: &str, error: BoughError) -> BoughError {
    let detail = error.to_string();
    // A failure that is already a shaped McpError passes through: it already names
    // the server and the move.
    if matches!(
        &error,
        BoughError::Http {
            kind: ErrorKind::Mcp,
            ..
        }
    ) && !detail.is_empty()
    {
        return error;
    }
    // NOT A BROKEN URL, so it must not say "check the url". An authorization server
    // with no `registration_endpoint` is working exactly as designed and wants an app
    // the user creates; the generic advice sends them to re-check a setting that was
    // right, which is how a solvable stop becomes a dead end.
    if detail
        .to_lowercase()
        .contains("does not support dynamic client registration")
    {
        return mcp(
            502,
            format!(
                "\"{server}\" ({server_url}) requires an OAuth client you register yourself — \
                 its authorization server does not offer dynamic registration. Create an app \
                 with that provider, set its redirect URL to {}, then put the id and secret on \
                 the registry entry — `clientId`, and `clientSecret` as a ${{VAR}} reference to \
                 a variable in ~/.bough/env — and press a again. Nothing was stored.",
                callback_url()
            ),
        );
    }
    mcp(
        502,
        format!(
            "could not run the OAuth flow for \"{server}\" against {server_url}: {detail}. \
             Check `url` in the registry (GET /mcp/servers) — it must point at the MCP \
             endpoint itself — and that the server is reachable. Nothing was stored."
        ),
    )
}

/// Start — or silently finish — the OAuth flow for one remote server.
///
/// "authorized" means the stored tokens were usable or refreshable and nothing is
/// asked of the human. "redirect" hands back the URL they must open. Neither outcome
/// is an error and neither one blocks.
pub async fn begin_auth(
    server: &str,
    server_url: &str,
    opts: &AuthFlowOptions,
) -> Result<AuthStart, BoughError> {
    let provider = BoughOAuthProvider::new(server, &opts.provider)?;
    begin_auth_with(&provider, server, server_url, opts).await
}

/// [`begin_auth`] against a caller-owned provider, so a test can read back the
/// captured `authorization_url` and the store it wrote through.
pub async fn begin_auth_with(
    provider: &BoughOAuthProvider,
    server: &str,
    server_url: &str,
    opts: &AuthFlowOptions,
) -> Result<AuthStart, BoughError> {
    let fetch = bounded_fetch(opts.fetch.clone(), opts.timeout_ms.unwrap_or(AUTH_HTTP_MS));
    let result = flow::auth(provider, server_url, None, &fetch)
        .await
        .map_err(|e| auth_failure(server, server_url, e))?;
    if result == flow::AuthResult::Authorized {
        return Ok(AuthStart {
            status: "authorized".into(),
            server: server.into(),
            authorization_url: None,
            corrected_url: None,
        });
    }
    let Some(url) = provider.authorization_url() else {
        return Err(mcp(
            502,
            format!(
                "the authorization server for \"{server}\" produced no authorization URL, so \
                 there is nothing to approve. Check `url` in the registry (GET /mcp/servers) \
                 — it must point at the MCP endpoint itself."
            ),
        ));
    };
    Ok(AuthStart {
        status: "redirect".into(),
        server: server.into(),
        authorization_url: Some(url),
        corrected_url: None,
    })
}

#[derive(Clone, Default)]
pub struct CompleteAuthOptions {
    pub flow: AuthFlowOptions,
    /// The registry lookup. Injected so the callback can be tested without a registry.
    #[allow(clippy::type_complexity)]
    pub server_url_for: Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>,
}

/// Finish the flow from the browser callback: validate the `state` round-trip,
/// exchange the code, persist the tokens. Returns the server the tokens belong to.
///
/// The state check happens BEFORE anything touches the network, so a forged or
/// replayed callback costs one string comparison and cannot start an exchange against
/// a server it named.
pub async fn complete_auth(
    state: &str,
    code: &str,
    opts: &CompleteAuthOptions,
) -> Result<String, BoughError> {
    let Some(dot) = state.rfind('.').filter(|d| *d > 0) else {
        return Err(mcp(
            400,
            format!(
                "malformed state {} — a bough callback carries \"<server>.<nonce>\". Start the \
                 flow again from the mcp panel (^p, then a).",
                serde_json::to_string(state).unwrap_or_default()
            ),
        ));
    };
    let server = &state[..dot];
    assert_server_name(server)?;
    let nonce = &state[dot + 1..];
    let store = TokenStore::new(&TokenStoreOptions {
        dir: opts.flow.provider.dir.clone(),
    });
    if nonce.is_empty() || store.load(server)?.state.as_deref() != Some(nonce) {
        return Err(mcp(
            400,
            format!(
                "state mismatch for \"{server}\" — this callback does not match the \
                 authorization bough started (it may have been completed already, or started \
                 by a different process). Open the mcp panel (^p) and press a on {server} to \
                 start a fresh one."
            ),
        ));
    }
    let server_url = match &opts.server_url_for {
        Some(f) => f(server),
        None => remote_server_url(server, &opts.flow.provider.config)?,
    };
    let Some(server_url) = server_url else {
        return Err(mcp(
            404,
            format!(
                "\"{server}\" is not a registered remote MCP server, so there is nothing to \
                 authorize. Register it with PUT /mcp/servers/{server} first."
            ),
        ));
    };
    let provider = BoughOAuthProvider::new(server, &opts.flow.provider)?;
    let fetch = bounded_fetch(
        opts.flow.fetch.clone(),
        opts.flow.timeout_ms.unwrap_or(AUTH_HTTP_MS),
    );
    let result = flow::auth(&provider, &server_url, Some(code), &fetch)
        .await
        .map_err(|e| auth_failure(server, &server_url, e))?;
    if result != flow::AuthResult::Authorized {
        return Err(mcp(
            502,
            format!(
                "the token exchange for \"{server}\" did not complete — the authorization \
                 server accepted the code but returned no tokens. Press a on {server} again \
                 (^p)."
            ),
        ));
    }
    Ok(server.to_string())
}

/// The registry's `url` for a server, read FRESH (MCP state is never cached).
pub fn remote_server_url(
    server: &str,
    config: &RegistryAccess,
) -> Result<Option<String>, BoughError> {
    Ok(config.get(server)?.and_then(|e| e.url))
}

// ---------------------------------------------------------------------------
// The HTTP surface (bodies only; the routes live in `bough-server`)
// ---------------------------------------------------------------------------

/// The registry entry a remote-auth request is about, or a readable refusal.
pub fn require_remote(name: &str, config: &RegistryAccess) -> Result<String, BoughError> {
    assert_server_name(name)?;
    let Some(entry) = config.get(name)? else {
        return Err(mcp(
            404,
            format!(
                "\"{name}\" is not a registered MCP server. Register it with PUT \
                 /mcp/servers/{name}."
            ),
        ));
    };
    entry.url.ok_or_else(|| {
        mcp(
            400,
            format!(
                "\"{name}\" is a local stdio server — it runs as a subprocess and has no \
                 OAuth. Authorization applies to remote (`url`) servers only."
            ),
        )
    })
}

/// `GET /mcp/servers/:name/auth` — is this server authorized, and where does the flow
/// return?
pub fn auth_status_route(name: &str, config: &RegistryAccess) -> Result<AuthStatus, BoughError> {
    require_remote(name, config)?;
    auth_status(name, &TokenStoreOptions::default(), None)
}

/// `POST /mcp/servers/:name/auth` — start the flow. This is what the mcp panel's `a`
/// calls. It returns the URL; it never opens a browser and never blocks waiting for
/// one, so a headless install behaves the same as a desktop one.
pub async fn begin_auth_route(name: &str, opts: &AuthFlowOptions) -> Result<AuthStart, BoughError> {
    let url = require_remote(name, &opts.provider.config)?;
    match begin_auth(name, &url, opts).await {
        Ok(start) => Ok(start),
        Err(error) => {
            // THE URL IN THE DOCS IS OFTEN NOT THE URL THE FLOW WANTS. Linear
            // publishes `https://mcp.linear.app/sse`; that endpoint's RFC 9728
            // metadata declares its resource as `https://mcp.linear.app/mcp`, and the
            // flow refuses the mismatch — correctly, since a resource indicator that
            // does not match is how a token gets minted for the wrong audience. But
            // the server has just TOLD us the right URL.
            //
            // Same-origin only. Following a cross-origin redeclaration would let a
            // server point bough's registry at someone else's endpoint.
            let Some(advertised) = declared_resource(&error.to_string(), &url) else {
                return Err(error);
            };
            opts.provider.config.write_url(name, &advertised)?;
            let mut start = begin_auth(name, &advertised, opts).await?;
            start.corrected_url = Some(advertised);
            Ok(start)
        }
    }
}

/// `DELETE /mcp/servers/:name/auth` — forget the tokens ("logout").
pub fn clear_auth_route(name: &str) -> Result<Value, BoughError> {
    assert_server_name(name)?;
    Ok(json!({ "server": name, "cleared": clear_auth(name, &TokenStoreOptions::default()) }))
}

/// The resource URL a failed flow says the server actually declares, when it is safe
/// to adopt: same origin, and genuinely different from what we tried.
pub fn declared_resource(text: &str, tried: &str) -> Option<String> {
    let marker = "Protected resource ";
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let found = rest.split_whitespace().next()?;
    if !rest[found.len()..]
        .trim_start()
        .starts_with("does not match expected ")
    {
        return None;
    }
    let found = normalize_url(found)?;
    let tried_n = normalize_url(tried)?;
    if origin_of(&found)? != origin_of(&tried_n)? {
        return None;
    }
    if found == tried_n {
        return None;
    }
    Some(found)
}

/// `GET /mcp/oauth/callback` — where the user's browser lands.
///
/// The audience is a HUMAN in a browser tab, so every outcome is a readable page
/// rather than a JSON error: they cannot act on `{"error": …}` and they will not see
/// a status code. The page is self-contained — no CDN, no font, no image.
pub async fn oauth_callback_page(query: &str, opts: &CompleteAuthOptions) -> (u16, String) {
    let q = parse_query(query);
    if let Some(error) = q.get("error") {
        // The authorization server refused, or the user declined. Their words, not
        // ours.
        let detail = match q.get("error_description") {
            Some(d) => format!("{}: {}", escape_html(error), escape_html(d)),
            None => escape_html(error),
        };
        return page(
            400,
            "Authorization was declined",
            &detail,
            "Nothing was stored. Start again from bough's mcp panel (^p, then a).",
        );
    }
    let (Some(code), Some(state)) = (q.get("code"), q.get("state")) else {
        return page(
            400,
            "That link is not a bough callback",
            "The authorization server did not send a <code>code</code> and <code>state</code>.",
            "Start the flow from bough's mcp panel (^p, then a) and open the URL it prints.",
        );
    };
    match complete_auth(state, code, opts).await {
        Ok(server) => page(
            200,
            &format!("Connected to {}", escape_html(&server)),
            "bough stored the tokens for this server. You can close this tab.",
            "Its tools appear in the next turn's catalog.",
        ),
        // Deliberately a page, not a throw: the router's catch would answer JSON, and
        // this response is being read by a person in a browser.
        Err(e) => page(
            e.status(),
            "Authorization did not complete",
            &escape_html(&e.to_string()),
            "Nothing was stored. Start again from bough's mcp panel (^p, then a).",
        ),
    }
}

fn page(status: u16, title: &str, detail: &str, footer: &str) -> (u16, String) {
    (
        status,
        format!(
            "<!doctype html><meta charset=\"utf-8\"><title>bough — {title}</title>\
             <style>\
             body{{font:15px/1.6 ui-sans-serif,system-ui,sans-serif;margin:0;padding:12vh 6vw;\
             background:#111;color:#eee}}\
             @media(prefers-color-scheme:light){{body{{background:#fff;color:#111}}}}\
             h1{{font-size:1.25rem;margin:0 0 .5rem}}p{{margin:.25rem 0;opacity:.85}}\
             code{{font-family:ui-monospace,monospace;opacity:1}}\
             </style>\
             <h1>{title}</h1><p>{detail}</p><p>{footer}</p>\n"
        ),
    )
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The registry bridge — the one place OAuth reaches `mcp::config`
// ---------------------------------------------------------------------------

mod registry_bridge {
    //! Production defaults for [`super::RegistryAccess`]. Everything else in this
    //! module takes its registry through injection, so this is the only edge from
    //! `oauth` to `config`.

    use std::collections::BTreeMap;

    use super::{BoughError, RegistryEntry};
    use crate::mcp::config::{expand_env, get_server, upsert_server, McpConfigOptions};

    pub fn get_entry(name: &str) -> Result<Option<RegistryEntry>, BoughError> {
        let opts = McpConfigOptions::default();
        let Some(entry) = get_server(name, &opts) else {
            return Ok(None);
        };
        // The secret is a `${VAR}` reference by schema, expanded here and nowhere
        // earlier — `expand_env` throws naming the variable when it is not set, which
        // is the message the user needs and the one they would otherwise get as an
        // opaque 401 from the token endpoint.
        let client_secret = match &entry.client_secret {
            None => None,
            Some(raw) => {
                let mut one = BTreeMap::new();
                one.insert("clientSecret".to_string(), raw.clone());
                expand_env(&one, &opts)?.remove("clientSecret")
            }
        };
        Ok(Some(RegistryEntry {
            url: entry.url.clone(),
            client_id: entry.client_id.clone(),
            client_secret,
        }))
    }

    /// The whole entry is rewritten with the corrected url; every other field is
    /// carried through, because `upsert_server` replaces rather than merges.
    pub fn set_url(name: &str, url: &str) -> Result<(), BoughError> {
        let opts = McpConfigOptions::default();
        let mut raw = match get_server(name, &opts) {
            Some(entry) => serde_json::to_value(&entry).unwrap_or_else(|_| serde_json::json!({})),
            None => serde_json::json!({}),
        };
        raw["url"] = serde_json::json!(url);
        upsert_server(name, &raw, &opts)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The flow engine (the SDK `auth()` replacement)
// ---------------------------------------------------------------------------

pub mod flow {
    //! RFC 9728 discovery → RFC 8414 metadata → RFC 7591 registration → PKCE → the
    //! code exchange, or a refresh. Four requests, all bounded by the caller's fetch.

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AuthResult {
        Authorized,
        Redirect,
    }

    /// What discovery settled on. Cached in the token file so a reconnect re-probes
    /// nothing.
    #[derive(Debug, Clone, Default)]
    pub struct Discovered {
        pub authorization_server_url: String,
        pub authorization_endpoint: String,
        pub token_endpoint: String,
        pub registration_endpoint: Option<String>,
        /// The canonical resource indicator, when the server declares one.
        pub resource: Option<String>,
    }

    impl Discovered {
        fn to_value(&self) -> Value {
            json!({
                "authorizationServerUrl": self.authorization_server_url,
                "authorizationServerMetadata": {
                    "authorization_endpoint": self.authorization_endpoint,
                    "token_endpoint": self.token_endpoint,
                    "registration_endpoint": self.registration_endpoint,
                },
                "resourceMetadata": self.resource.as_ref().map(|r| json!({ "resource": r })),
            })
        }

        /// Read back a cached document — including one written by the TypeScript
        /// build, whose `authorizationServerMetadata` carries the same RFC 8414 field
        /// names.
        fn from_value(v: &Value) -> Option<Self> {
            let asm = v.get("authorizationServerMetadata")?;
            let authorization_endpoint = asm.get("authorization_endpoint")?.as_str()?.to_string();
            let token_endpoint = asm.get("token_endpoint")?.as_str()?.to_string();
            Some(Self {
                authorization_server_url: v
                    .get("authorizationServerUrl")
                    .and_then(|u| u.as_str())
                    .unwrap_or_default()
                    .to_string(),
                authorization_endpoint,
                token_endpoint,
                registration_endpoint: asm
                    .get("registration_endpoint")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string()),
                resource: v
                    .get("resourceMetadata")
                    .and_then(|m| m.get("resource"))
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string()),
            })
        }
    }

    /// The whole flow. `authorization_code` present = finish; absent = start (or
    /// discover that nothing needs asking).
    pub async fn auth(
        provider: &BoughOAuthProvider,
        server_url: &str,
        authorization_code: Option<&str>,
        fetch: &FetchFn,
    ) -> Result<AuthResult, BoughError> {
        let discovered = match provider
            .discovery_state()
            .as_ref()
            .and_then(Discovered::from_value)
        {
            Some(d) => d,
            None => {
                let d = discover(server_url, fetch).await?;
                let _ = provider.save_discovery_state(d.to_value());
                d
            }
        };

        let client = match provider.client_information()? {
            Some(c) => c,
            None => {
                let registered = register(&discovered, provider, fetch).await?;
                provider.save_client_information(registered.clone())?;
                registered
            }
        };

        // Finish: exchange the code the browser came back with.
        if let Some(code) = authorization_code {
            let verifier = provider.code_verifier()?;
            let tokens = exchange(&discovered, &client, provider, code, &verifier, fetch).await?;
            provider.save_tokens(tokens)?;
            return Ok(AuthResult::Authorized);
        }

        // A refreshable pair means nothing is asked of the human.
        //
        // NO EXPIRY SHORTCUT, deliberately. This is also the transport's 401 path
        // (`remote.rs`): the token that just came back 401 is known-bad whatever
        // `expiresAt` says, and short-circuiting on a stored expiry would present it
        // again forever. Trading one refresh round trip for that is the right price.
        if let Some(tokens) = provider.tokens()? {
            if let Some(refresh_token) = tokens.refresh_token.as_deref() {
                match refresh(&discovered, &client, refresh_token, fetch).await {
                    Ok(next) => {
                        provider.save_tokens(next)?;
                        return Ok(AuthResult::Authorized);
                    }
                    // A rejected refresh token is not a fault: dropping it and asking
                    // again is the whole point of the recovery hook. Without this an
                    // expired refresh token loops instead of degrading to a prompt.
                    Err(_) => provider.invalidate_credentials(InvalidateScope::Tokens)?,
                }
            }
        }

        // Start: PKCE, a nonce, and a URL for the human.
        let verifier = random_verifier();
        let challenge = s256_challenge(&verifier);
        let state = provider.state()?;
        provider.save_code_verifier(&verifier)?;
        let mut params = vec![
            ("response_type", "code".to_string()),
            ("client_id", client.client_id.clone()),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256".to_string()),
            ("redirect_uri", provider.redirect_url()),
            ("state", state),
        ];
        if let Some(resource) = &discovered.resource {
            params.push(("resource", resource.clone()));
        }
        let sep = if discovered.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{sep}{}",
            discovered.authorization_endpoint,
            form_encode(&params)
        );
        provider.redirect_to_authorization(&url);
        Ok(AuthResult::Redirect)
    }

    // ---- discovery ---------------------------------------------------------

    async fn discover(server_url: &str, fetch: &FetchFn) -> Result<Discovered, BoughError> {
        let origin = origin_of(server_url)
            .ok_or_else(|| mcp(400, format!("{server_url} is not an absolute URL")))?;
        let path = path_of(server_url).unwrap_or_default();

        // RFC 9728: the path-aware well-known first, then the root one.
        let mut resource: Option<String> = None;
        let mut as_url: Option<String> = None;
        let candidates = if path.is_empty() || path == "/" {
            vec![format!("{origin}/.well-known/oauth-protected-resource")]
        } else {
            vec![
                format!("{origin}/.well-known/oauth-protected-resource{path}"),
                format!("{origin}/.well-known/oauth-protected-resource"),
            ]
        };
        for url in candidates {
            if let Some(doc) = get_json(&url, fetch).await {
                resource = doc
                    .get("resource")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string());
                as_url = doc
                    .get("authorization_servers")
                    .and_then(|a| a.as_array())
                    .and_then(|a| a.first())
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string());
                break;
            }
        }

        // A declared resource that does not cover the URL we were pointed at is how a
        // token gets minted for the wrong audience, so it is refused. The message
        // shape is what `declared_resource` reads the correction out of.
        if let Some(declared) = &resource {
            if !resource_allows(declared, server_url) {
                return Err(mcp(
                    502,
                    format!(
                        "Protected resource {declared} does not match expected {server_url} \
                         (or origin)"
                    ),
                ));
            }
        }

        let as_url = as_url.unwrap_or_else(|| origin.clone());
        let as_origin = origin_of(&as_url).unwrap_or_else(|| as_url.clone());
        let as_path = path_of(&as_url).unwrap_or_default();
        let as_path = if as_path == "/" {
            String::new()
        } else {
            as_path
        };

        // RFC 8414, then OpenID Connect discovery.
        let mut metadata: Option<Value> = None;
        for url in [
            format!("{as_origin}/.well-known/oauth-authorization-server{as_path}"),
            format!("{as_origin}/.well-known/oauth-authorization-server"),
            format!("{as_origin}{as_path}/.well-known/openid-configuration"),
            format!("{as_origin}/.well-known/openid-configuration"),
        ] {
            if let Some(doc) = get_json(&url, fetch).await {
                metadata = Some(doc);
                break;
            }
        }
        let str_at = |doc: &Option<Value>, key: &str| -> Option<String> {
            doc.as_ref()
                .and_then(|d| d.get(key))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        Ok(Discovered {
            authorization_endpoint: str_at(&metadata, "authorization_endpoint")
                .unwrap_or_else(|| format!("{as_origin}/authorize")),
            token_endpoint: str_at(&metadata, "token_endpoint")
                .unwrap_or_else(|| format!("{as_origin}/token")),
            registration_endpoint: str_at(&metadata, "registration_endpoint"),
            authorization_server_url: as_url,
            resource,
        })
    }

    /// RFC 8707: the declared resource must be the requested URL or a prefix of it,
    /// on the same origin.
    fn resource_allows(declared: &str, requested: &str) -> bool {
        let (Some(d_origin), Some(r_origin)) = (origin_of(declared), origin_of(requested)) else {
            return false;
        };
        if d_origin != r_origin {
            return false;
        }
        let d_path = path_of(declared).unwrap_or_default();
        let r_path = path_of(requested).unwrap_or_default();
        let d_path = d_path.trim_end_matches('/');
        r_path == d_path || r_path.starts_with(&format!("{d_path}/")) || d_path.is_empty()
    }

    async fn get_json(url: &str, fetch: &FetchFn) -> Option<Value> {
        let res = fetch(HttpReq {
            method: "GET".into(),
            url: url.to_string(),
            headers: vec![("accept".into(), "application/json".into())],
            body: None,
        })
        .await
        .ok()?;
        if res.status < 200 || res.status >= 300 {
            return None;
        }
        serde_json::from_str(&res.body).ok()
    }

    // ---- registration ------------------------------------------------------

    async fn register(
        d: &Discovered,
        provider: &BoughOAuthProvider,
        fetch: &FetchFn,
    ) -> Result<ClientInfo, BoughError> {
        let Some(endpoint) = &d.registration_endpoint else {
            return Err(mcp(
                502,
                format!(
                    "the authorization server at {} does not support dynamic client \
                     registration",
                    d.authorization_server_url
                ),
            ));
        };
        let res = fetch(HttpReq {
            method: "POST".into(),
            url: endpoint.clone(),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "application/json".into()),
            ],
            body: Some(provider.client_metadata().to_string()),
        })
        .await
        .map_err(|e| mcp(502, format!("dynamic client registration failed: {e}")))?;
        if res.status < 200 || res.status >= 300 {
            return Err(mcp(
                502,
                format!(
                    "dynamic client registration was refused ({}): {}",
                    res.status, res.body
                ),
            ));
        }
        serde_json::from_str::<ClientInfo>(&res.body).map_err(|e| {
            mcp(
                502,
                format!("dynamic client registration returned no client_id: {e}"),
            )
        })
    }

    // ---- the token endpoint ------------------------------------------------

    async fn exchange(
        d: &Discovered,
        client: &ClientInfo,
        provider: &BoughOAuthProvider,
        code: &str,
        verifier: &str,
        fetch: &FetchFn,
    ) -> Result<OAuthTokens, BoughError> {
        let mut params = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("code_verifier", verifier.to_string()),
            ("client_id", client.client_id.clone()),
            ("redirect_uri", provider.redirect_url()),
        ];
        if let Some(secret) = &client.client_secret {
            params.push(("client_secret", secret.clone()));
        }
        if let Some(resource) = &d.resource {
            params.push(("resource", resource.clone()));
        }
        post_tokens(&d.token_endpoint, &params, fetch).await
    }

    async fn refresh(
        d: &Discovered,
        client: &ClientInfo,
        refresh_token: &str,
        fetch: &FetchFn,
    ) -> Result<OAuthTokens, BoughError> {
        let mut params = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.to_string()),
            ("client_id", client.client_id.clone()),
        ];
        if let Some(secret) = &client.client_secret {
            params.push(("client_secret", secret.clone()));
        }
        if let Some(resource) = &d.resource {
            params.push(("resource", resource.clone()));
        }
        let mut tokens = post_tokens(&d.token_endpoint, &params, fetch).await?;
        // RFC 6749 §6: the authorization server MAY omit a new refresh token, and the
        // old one stays valid. Losing it here would turn every second refresh into a
        // fresh authorization prompt.
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = Some(refresh_token.to_string());
        }
        Ok(tokens)
    }

    async fn post_tokens(
        endpoint: &str,
        params: &[(&str, String)],
        fetch: &FetchFn,
    ) -> Result<OAuthTokens, BoughError> {
        let res = fetch(HttpReq {
            method: "POST".into(),
            url: endpoint.to_string(),
            headers: vec![
                (
                    "content-type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
                ("accept".into(), "application/json".into()),
            ],
            body: Some(form_encode(params)),
        })
        .await
        .map_err(|e| mcp(502, format!("the token endpoint could not be reached: {e}")))?;
        if res.status < 200 || res.status >= 300 {
            let code = serde_json::from_str::<Value>(&res.body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| format!("HTTP {}", res.status));
            return Err(mcp(
                502,
                format!("the token endpoint refused the grant: {code}"),
            ));
        }
        serde_json::from_str::<OAuthTokens>(&res.body).map_err(|e| {
            mcp(
                502,
                format!("the token endpoint returned no access token: {e}"),
            )
        })
    }

    // ---- PKCE --------------------------------------------------------------

    /// 32 random bytes, base64url. `uuid` v4 is the workspace's randomness.
    fn random_verifier() -> String {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    fn s256_challenge(verifier: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(verifier.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `scheme://host[:port]`, lowercased scheme and host.
pub(crate) fn origin_of(url: &str) -> Option<String> {
    let i = url.find("://")?;
    let scheme = url[..i].to_ascii_lowercase();
    if scheme.is_empty() {
        return None;
    }
    let rest = &url[i + 3..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

/// The path component, `""` when there is none.
pub(crate) fn path_of(url: &str) -> Option<String> {
    let i = url.find("://")?;
    let rest = &url[i + 3..];
    let after_host = rest.find(['/', '?', '#'])?;
    let tail = &rest[after_host..];
    if !tail.starts_with('/') {
        return Some(String::new());
    }
    Some(tail.split(['?', '#']).next().unwrap_or("").to_string())
}

/// Origin + path + query, so two spellings of the same URL compare equal.
fn normalize_url(url: &str) -> Option<String> {
    let origin = origin_of(url)?;
    let path = path_of(url).unwrap_or_default();
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    let query = url.find('?').map(|i| &url[i..]).unwrap_or("");
    let query = query.split('#').next().unwrap_or("");
    Some(format!("{origin}{path}{query}"))
}

fn form_encode(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `application/x-www-form-urlencoded` — everything outside the unreserved set is
/// escaped, and a space is `%20` (`URLSearchParams` uses `+`; both are accepted by
/// every token endpoint, and `%20` is also correct inside a query string).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `?a=1&b=2` query string (with or without the leading `?`).
pub fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .trim_start_matches('?')
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.find('=') {
            Some(i) => (percent_decode(&p[..i]), percent_decode(&p[i + 1..])),
            None => (percent_decode(p), String::new()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! Two properties carry the weight here:
    //!
    //!   - **the callback belongs to exactly one flow.** The `state` round-trip is
    //!     checked before anything touches the network, so a forged or replayed
    //!     callback cannot graft tokens onto a server it named;
    //!   - **credentials are private and per server.** Directory 0700, file 0600, one
    //!     file per server, and a name that is not a slug never becomes a path.
    //!
    //! The rest is the flow itself, driven end to end against a scripted authorization
    //! server: dynamic registration, PKCE, and the code exchange through the real
    //! callback handler. Hermetic: no socket, no real `~/.bough`, no outbound network.

    use super::*;
    use std::sync::Mutex as StdMutex;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bough-oauth-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_at(dir: &std::path::Path) -> TokenStore {
        TokenStore::new(&TokenStoreOptions {
            dir: Some(dir.to_path_buf()),
        })
    }

    fn provider_at(server: &str, dir: &std::path::Path) -> BoughOAuthProvider {
        BoughOAuthProvider::new(
            server,
            &ProviderOptions {
                dir: Some(dir.to_path_buf()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// What the flow saw, so PKCE and the grant type can be asserted.
    #[derive(Default, Debug)]
    struct Seen {
        grants: Vec<String>,
        verifiers: Vec<Option<String>>,
        registered: usize,
    }

    /// A scripted authorization server: discovery, registration, token exchange.
    /// Injected as a [`FetchFn`], so nothing binds a socket.
    fn auth_server(base: &str, codes: Vec<(&str, &str)>) -> (FetchFn, Arc<StdMutex<Seen>>) {
        let seen = Arc::new(StdMutex::new(Seen::default()));
        let base = base.to_string();
        let codes: Vec<(String, String)> = codes
            .into_iter()
            .map(|(a, b)| (a.into(), b.into()))
            .collect();
        let out = seen.clone();
        let fetch: FetchFn = Arc::new(move |req: HttpReq| {
            let base = base.clone();
            let codes = codes.clone();
            let seen = out.clone();
            Box::pin(async move {
                let path = path_of(&req.url).unwrap_or_default();
                let json_res = |v: Value, status: u16| {
                    Ok(HttpRes {
                        status,
                        body: v.to_string(),
                        ..Default::default()
                    })
                };
                if path.starts_with("/.well-known/oauth-protected-resource") {
                    return json_res(
                        json!({ "resource": format!("{base}/mcp"), "authorization_servers": [base] }),
                        200,
                    );
                }
                if path.starts_with("/.well-known/") {
                    return json_res(
                        json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/authorize"),
                            "token_endpoint": format!("{base}/token"),
                            "registration_endpoint": format!("{base}/register"),
                            "response_types_supported": ["code"],
                            "code_challenge_methods_supported": ["S256"],
                            "token_endpoint_auth_methods_supported": ["none"],
                        }),
                        200,
                    );
                }
                if path == "/register" {
                    seen.lock().unwrap().registered += 1;
                    let metadata: Value =
                        serde_json::from_str(req.body.as_deref().unwrap_or("{}")).unwrap();
                    return json_res(
                        json!({
                            "client_id": "dyn-client",
                            "redirect_uris": metadata["redirect_uris"],
                            "token_endpoint_auth_method": "none",
                        }),
                        201,
                    );
                }
                if path == "/token" {
                    let form = parse_query(req.body.as_deref().unwrap_or(""));
                    {
                        let mut s = seen.lock().unwrap();
                        s.grants
                            .push(form.get("grant_type").cloned().unwrap_or_default());
                        s.verifiers.push(form.get("code_verifier").cloned());
                    }
                    let key = match form.get("grant_type").map(String::as_str) {
                        Some("refresh_token") => form.get("refresh_token").cloned(),
                        _ => form.get("code").cloned(),
                    }
                    .unwrap_or_default();
                    let minted = codes
                        .iter()
                        .find(|(c, _)| *c == key)
                        .map(|(_, m)| m.clone());
                    return match minted {
                        None => json_res(json!({ "error": "invalid_grant" }), 400),
                        Some(m) => json_res(
                            json!({ "access_token": m, "token_type": "Bearer", "expires_in": 3600 }),
                            200,
                        ),
                    };
                }
                Ok(HttpRes {
                    status: 404,
                    body: "not found".into(),
                    ..Default::default()
                })
            }) as BoxFuture<'static, Result<HttpRes, String>>
        });
        (fetch, seen)
    }

    // ---- the store and the provider ---------------------------------------

    #[test]
    fn the_provider_persists_registration_tokens_and_verifier() {
        let dir = temp_dir("provider");
        let store = store_at(&dir);
        let provider = BoughOAuthProvider::new(
            "notion",
            &ProviderOptions {
                dir: Some(dir.clone()),
                now: Some(Arc::new(|| 1_000)),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(provider.client_information().unwrap(), None);
        assert!(!has_tokens(
            "notion",
            &TokenStoreOptions {
                dir: Some(dir.clone())
            }
        ));

        provider
            .save_client_information(ClientInfo {
                client_id: "abc".into(),
                ..Default::default()
            })
            .unwrap();
        provider.save_code_verifier("ver1").unwrap();
        let state = provider.state().unwrap();
        assert!(state.starts_with("notion."), "{state}");
        assert_eq!(
            provider.client_information().unwrap(),
            Some(ClientInfo {
                client_id: "abc".into(),
                ..Default::default()
            })
        );
        assert_eq!(provider.code_verifier().unwrap(), "ver1");

        provider
            .save_tokens(OAuthTokens {
                access_token: "tok".into(),
                token_type: "Bearer".into(),
                expires_in: Some(60.0),
                ..Default::default()
            })
            .unwrap();
        assert!(has_tokens(
            "notion",
            &TokenStoreOptions {
                dir: Some(dir.clone())
            }
        ));
        assert_eq!(provider.tokens().unwrap().unwrap().access_token, "tok");
        // The registration survives; the in-flight nonce and verifier do not — that is
        // what stops a replayed callback from exchanging the same code twice.
        assert_eq!(
            provider.client_information().unwrap(),
            Some(ClientInfo {
                client_id: "abc".into(),
                ..Default::default()
            })
        );
        let s = store.load("notion").unwrap();
        assert_eq!(s.state, None);
        assert_eq!(s.code_verifier, None);
        assert_eq!(s.expires_at, Some(1_000 + 60_000));

        // Expiry is reported, not acted on: the transport refreshes, this is display.
        let now: NowFn = Arc::new(|| 2_000_000);
        let status = auth_status(
            "notion",
            &TokenStoreOptions {
                dir: Some(dir.clone()),
            },
            Some(&now),
        )
        .unwrap();
        assert!(status.authorized);
        assert!(status.expired);
        assert!(!status.refreshable);

        assert!(clear_auth(
            "notion",
            &TokenStoreOptions {
                dir: Some(dir.clone())
            }
        ));
        assert!(!has_tokens("notion", &TokenStoreOptions { dir: Some(dir) }));
    }

    #[test]
    fn a_missing_verifier_is_a_restartable_message_not_a_crash() {
        let dir = temp_dir("verifier");
        let err = provider_at("notion", &dir)
            .code_verifier()
            .expect_err("a throw");
        assert!(err.to_string().contains("press a on notion"), "{err}");
    }

    #[test]
    fn invalidate_credentials_drops_exactly_its_scope() {
        let dir = temp_dir("invalidate");
        let store = store_at(&dir);
        let provider = provider_at("linear", &dir);
        let seed = || {
            store
                .write(
                    "linear",
                    &Stored {
                        client: Some(ClientInfo {
                            client_id: "c".into(),
                            ..Default::default()
                        }),
                        tokens: Some(OAuthTokens {
                            access_token: "t".into(),
                            token_type: "Bearer".into(),
                            ..Default::default()
                        }),
                        code_verifier: Some("v".into()),
                        discovery: Some(json!({ "authorizationServerUrl": "http://as.invalid" })),
                        ..Default::default()
                    },
                )
                .unwrap()
        };

        seed();
        provider
            .invalidate_credentials(InvalidateScope::Tokens)
            .unwrap();
        assert_eq!(store.load("linear").unwrap().tokens, None);
        // The registration is the expensive half — re-registering on every rejected
        // refresh leaves a trail of dead clients on the authorization server.
        assert_eq!(
            store.load("linear").unwrap().client,
            Some(ClientInfo {
                client_id: "c".into(),
                ..Default::default()
            })
        );
        assert_eq!(
            store.load("linear").unwrap().code_verifier.as_deref(),
            Some("v")
        );

        seed();
        provider
            .invalidate_credentials(InvalidateScope::Discovery)
            .unwrap();
        assert_eq!(store.load("linear").unwrap().discovery, None);
        assert!(store.load("linear").unwrap().tokens.is_some());

        seed();
        provider
            .invalidate_credentials(InvalidateScope::All)
            .unwrap();
        assert_eq!(store.load("linear").unwrap(), Stored::default());
    }

    #[test]
    fn the_provider_is_a_public_client_whose_redirect_is_boughs_own_callback() {
        configure_oauth_callback(4444);
        assert_eq!(callback_url(), "http://127.0.0.1:4444/mcp/oauth/callback");
        let dir = temp_dir("public");
        let provider = provider_at("x", &dir);
        let meta = provider.client_metadata();
        assert_eq!(meta["token_endpoint_auth_method"], "none");
        assert_eq!(
            meta["redirect_uris"],
            json!(["http://127.0.0.1:4444/mcp/oauth/callback"])
        );
        assert_eq!(
            provider.redirect_url(),
            "http://127.0.0.1:4444/mcp/oauth/callback"
        );
    }

    #[test]
    fn token_files_are_private_one_per_server() {
        let dir = temp_dir("perms");
        provider_at("sec", &dir)
            .save_tokens(OAuthTokens {
                access_token: "t".into(),
                token_type: "Bearer".into(),
                ..Default::default()
            })
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = dir.join("sec.json");
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn a_server_name_that_is_not_a_slug_never_becomes_a_path() {
        let dir = temp_dir("slug");
        let store = store_at(&dir);
        for name in ["../evil", "a/b", "Notion", ""] {
            let err = store.load(name).expect_err(name);
            assert_eq!(err.status(), 400, "expected {name:?} to be refused");
        }
    }

    #[test]
    fn a_token_file_written_by_the_typescript_build_still_loads() {
        // The bridge that keeps an authorized install authorized across the cutover:
        // the SDK's `discovery` shape is not this build's, and it must not cost the
        // user their tokens.
        let dir = temp_dir("compat");
        let store = store_at(&dir);
        std::fs::write(
            dir.join("linear.json"),
            r#"{
              "client": {"client_id":"c1","redirect_uris":["http://127.0.0.1:4321/mcp/oauth/callback"]},
              "tokens": {"access_token":"at","token_type":"Bearer","refresh_token":"rt","expires_in":3600},
              "expiresAt": 1754400000000,
              "discovery": {"authorizationServerUrl":"https://as.example","somethingNew":{"a":1}}
            }"#,
        )
        .unwrap();
        let s = store.load("linear").unwrap();
        assert_eq!(s.tokens.as_ref().unwrap().access_token, "at");
        assert_eq!(
            s.tokens.as_ref().unwrap().refresh_token.as_deref(),
            Some("rt")
        );
        assert_eq!(s.client.as_ref().unwrap().client_id, "c1");
        assert_eq!(s.expires_at, Some(1754400000000));
        // …and a rewrite carries the parts this build does not model through intact.
        store.write("linear", &s).unwrap();
        let round: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("linear.json")).unwrap())
                .unwrap();
        assert_eq!(round["discovery"]["somethingNew"]["a"], 1);
        assert_eq!(
            round["client"]["redirect_uris"][0],
            "http://127.0.0.1:4321/mcp/oauth/callback"
        );
    }

    // ---- the flow ----------------------------------------------------------

    #[tokio::test]
    async fn complete_auth_validates_the_state_round_trip_before_touching_the_network() {
        let dir = temp_dir("state");
        let never: FetchFn = Arc::new(|_| {
            Box::pin(async { panic!("the network must not be touched before the state check") })
        });
        let opts = CompleteAuthOptions {
            flow: AuthFlowOptions {
                provider: ProviderOptions {
                    dir: Some(dir.clone()),
                    ..Default::default()
                },
                fetch: Some(never),
                ..Default::default()
            },
            server_url_for: Some(Arc::new(|_| Some("http://as.invalid/mcp".into()))),
        };

        let e = complete_auth("nodot", "c", &opts)
            .await
            .expect_err("a throw");
        assert!(e.to_string().contains("malformed state"), "{e}");
        // Nothing stored for this server at all.
        let e = complete_auth("notion.deadbeef", "c", &opts)
            .await
            .expect_err("a throw");
        assert!(e.to_string().contains("state mismatch"), "{e}");
        // A flow is in progress, but this is not its nonce.
        provider_at("notion", &dir).state().unwrap();
        let e = complete_auth("notion.wrong", "c", &opts)
            .await
            .expect_err("a throw");
        assert!(e.to_string().contains("state mismatch"), "{e}");
        // The nonce matches, but the server is no longer a registered remote.
        let state = provider_at("notion", &dir).state().unwrap();
        let gone = CompleteAuthOptions {
            server_url_for: Some(Arc::new(|_| None)),
            ..opts.clone()
        };
        let e = complete_auth(&state, "c", &gone)
            .await
            .expect_err("a throw");
        assert_eq!(e.status(), 404);
    }

    #[tokio::test]
    async fn begin_auth_captures_the_authorization_url_instead_of_navigating() {
        let dir = temp_dir("begin");
        let base = "http://127.0.0.1:59999";
        let (fetch, seen) = auth_server(base, vec![("the-code", "granted-1")]);
        let provider = BoughOAuthProvider::new(
            "acme",
            &ProviderOptions {
                dir: Some(dir.clone()),
                redirect_url: Some("http://127.0.0.1:4321/mcp/oauth/callback".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let opts = AuthFlowOptions {
            provider: ProviderOptions {
                dir: Some(dir.clone()),
                ..Default::default()
            },
            fetch: Some(fetch),
            ..Default::default()
        };
        let started = begin_auth_with(&provider, "acme", &format!("{base}/mcp"), &opts)
            .await
            .unwrap();
        assert_eq!(started.status, "redirect");
        assert_eq!(started.server, "acme");
        let url = started.authorization_url.unwrap();
        assert!(url.starts_with(&format!("{base}/authorize?")), "{url}");
        let q = parse_query(url.split_once('?').unwrap().1);
        assert_eq!(q.get("client_id").map(String::as_str), Some("dyn-client"));
        assert_eq!(
            q.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            q.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:4321/mcp/oauth/callback")
        );
        // The nonce in the URL is the one that was stored, and it names the server.
        let store = store_at(&dir);
        assert_eq!(
            q.get("state").cloned(),
            Some(format!(
                "acme.{}",
                store.load("acme").unwrap().state.unwrap()
            ))
        );
        // A verifier is waiting for the callback — the flow is genuinely half-done.
        assert!(store.load("acme").unwrap().code_verifier.is_some());
        // …and a real registration happened, once.
        assert_eq!(seen.lock().unwrap().registered, 1);
        // Nothing has been exchanged yet.
        assert!(seen.lock().unwrap().grants.is_empty());
    }

    #[tokio::test]
    async fn the_callback_exchanges_the_code_and_stores_the_tokens() {
        let dir = temp_dir("callback");
        let base = "http://127.0.0.1:59998";
        let (fetch, seen) = auth_server(base, vec![("the-code", "granted-1")]);
        let mcp_url = format!("{base}/mcp");
        let store = store_at(&dir);
        // A flow already begun: registered client, PKCE verifier, nonce.
        store
            .write(
                "acme",
                &Stored {
                    client: Some(ClientInfo {
                        client_id: "dyn-client".into(),
                        ..Default::default()
                    }),
                    code_verifier: Some("verifier-1".into()),
                    state: Some("nonce-1".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let opts = CompleteAuthOptions {
            flow: AuthFlowOptions {
                provider: ProviderOptions {
                    dir: Some(dir.clone()),
                    ..Default::default()
                },
                fetch: Some(fetch),
                ..Default::default()
            },
            server_url_for: Some(Arc::new(move |_| Some(mcp_url.clone()))),
        };

        let (status, body) = oauth_callback_page("?code=the-code&state=acme.nonce-1", &opts).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("Connected to acme"), "{body}");

        // The tokens landed.
        assert_eq!(
            store.load("acme").unwrap().tokens.unwrap().access_token,
            "granted-1"
        );
        // PKCE was actually proven, and the flow state is spent.
        assert_eq!(seen.lock().unwrap().grants, vec!["authorization_code"]);
        assert_eq!(
            seen.lock().unwrap().verifiers,
            vec![Some("verifier-1".to_string())]
        );
        assert_eq!(store.load("acme").unwrap().state, None);
        assert_eq!(store.load("acme").unwrap().code_verifier, None);

        // Replaying the same callback is refused without a second exchange.
        let (status, body) = oauth_callback_page("?code=the-code&state=acme.nonce-1", &opts).await;
        assert_eq!(status, 400);
        assert!(body.contains("state mismatch"), "{body}");
        assert_eq!(seen.lock().unwrap().grants, vec!["authorization_code"]);
    }

    #[tokio::test]
    async fn the_callback_refuses_a_request_that_is_not_a_bough_callback() {
        let opts = CompleteAuthOptions::default();
        let (status, body) = oauth_callback_page("", &opts).await;
        assert_eq!(status, 400);
        assert!(body.contains("not a bough callback"), "{body}");

        let (status, body) = oauth_callback_page("?error=access_denied", &opts).await;
        assert_eq!(status, 400);
        assert!(body.contains("declined"), "{body}");
        assert!(body.contains("access_denied"), "{body}");
    }

    #[tokio::test]
    async fn an_expired_refresh_token_degrades_into_an_authorization_prompt() {
        // Without the invalidate hook this loops on the same rejection and escapes as
        // a raw OAuth error instead of a question.
        let dir = temp_dir("refresh");
        let base = "http://127.0.0.1:59997";
        let (fetch, seen) = auth_server(base, vec![]);
        let store = store_at(&dir);
        store
            .write(
                "acme",
                &Stored {
                    client: Some(ClientInfo {
                        client_id: "dyn-client".into(),
                        ..Default::default()
                    }),
                    tokens: Some(OAuthTokens {
                        access_token: "stale".into(),
                        token_type: "Bearer".into(),
                        refresh_token: Some("dead".into()),
                        ..Default::default()
                    }),
                    expires_at: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        let opts = AuthFlowOptions {
            provider: ProviderOptions {
                dir: Some(dir.clone()),
                ..Default::default()
            },
            fetch: Some(fetch),
            ..Default::default()
        };
        let started = begin_auth("acme", &format!("{base}/mcp"), &opts)
            .await
            .unwrap();
        assert_eq!(started.status, "redirect");
        assert!(started.authorization_url.is_some());
        assert_eq!(seen.lock().unwrap().grants, vec!["refresh_token"]);
        // The dead grant was dropped rather than retried forever.
        assert_eq!(store.load("acme").unwrap().tokens, None);
    }

    #[tokio::test]
    async fn a_refreshable_pair_asks_the_human_nothing() {
        // "authorized" means the stored tokens were usable or refreshable, and the
        // refresh is what proves it — there is no expiry shortcut, because this same
        // path serves the transport's 401 and that token is known-bad.
        let dir = temp_dir("authorized");
        let base = "http://127.0.0.1:59996";
        let (fetch, seen) = auth_server(base, vec![("r-good", "fresh-1")]);
        let store = store_at(&dir);
        store
            .write(
                "acme",
                &Stored {
                    client: Some(ClientInfo {
                        client_id: "dyn-client".into(),
                        ..Default::default()
                    }),
                    tokens: Some(OAuthTokens {
                        access_token: "stale".into(),
                        token_type: "Bearer".into(),
                        refresh_token: Some("r-good".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        let opts = AuthFlowOptions {
            provider: ProviderOptions {
                dir: Some(dir),
                ..Default::default()
            },
            fetch: Some(fetch),
            ..Default::default()
        };
        let started = begin_auth("acme", &format!("{base}/mcp"), &opts)
            .await
            .unwrap();
        assert_eq!(started.status, "authorized");
        assert_eq!(started.authorization_url, None);
        assert_eq!(seen.lock().unwrap().grants, vec!["refresh_token"]);
        assert_eq!(
            store.load("acme").unwrap().tokens.unwrap().access_token,
            "fresh-1"
        );
        // The registration was reused rather than repeated.
        assert_eq!(seen.lock().unwrap().registered, 0);
        // An authorization server that omits a rotated refresh token leaves the old
        // one valid — losing it would turn the next expiry into a fresh prompt.
        assert_eq!(
            store
                .load("acme")
                .unwrap()
                .tokens
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("r-good")
        );
    }

    // ---- the docs URL is often not the flow's URL --------------------------

    #[test]
    fn an_advertised_same_origin_resource_is_adopted() {
        let err = "Protected resource https://mcp.linear.app/mcp does not match expected \
                   https://mcp.linear.app/sse (or origin)";
        assert_eq!(
            declared_resource(err, "https://mcp.linear.app/sse").as_deref(),
            Some("https://mcp.linear.app/mcp")
        );
    }

    #[test]
    fn a_cross_origin_redeclaration_is_refused() {
        // Following this would let a server point bough's registry at someone else's
        // endpoint — and the next flow would mint a token for that audience.
        let err = "Protected resource https://evil.example.com/mcp does not match expected \
                   https://mcp.linear.app/sse (or origin)";
        assert_eq!(declared_resource(err, "https://mcp.linear.app/sse"), None);
    }

    #[test]
    fn an_unrelated_failure_is_not_mistaken_for_a_redeclaration() {
        assert_eq!(
            declared_resource("fetch failed", "https://x.example/mcp"),
            None
        );
        assert_eq!(declared_resource("", "https://x.example/mcp"), None);
    }

    #[test]
    fn a_resource_identical_to_what_was_tried_is_not_a_correction() {
        // Retrying the same URL would loop.
        let err = "Protected resource https://x.example/mcp does not match expected \
                   https://x.example/mcp (or origin)";
        assert_eq!(declared_resource(err, "https://x.example/mcp"), None);
    }

    #[tokio::test]
    async fn a_declared_resource_that_does_not_cover_the_url_is_refused_in_that_shape() {
        // The refusal `declared_resource` reads the correction out of is produced by
        // the flow itself — the two halves have to agree or the correction never fires.
        let dir = temp_dir("mismatch");
        let fetch: FetchFn = Arc::new(|req: HttpReq| {
            Box::pin(async move {
                let path = path_of(&req.url).unwrap_or_default();
                if path.starts_with("/.well-known/oauth-protected-resource") {
                    return Ok(HttpRes {
                        status: 200,
                        body: json!({
                            "resource": "https://mcp.linear.app/mcp",
                            "authorization_servers": ["https://mcp.linear.app"],
                        })
                        .to_string(),
                        ..Default::default()
                    });
                }
                Ok(HttpRes {
                    status: 404,
                    body: "no".into(),
                    ..Default::default()
                })
            }) as BoxFuture<'static, Result<HttpRes, String>>
        });
        let opts = AuthFlowOptions {
            provider: ProviderOptions {
                dir: Some(dir),
                ..Default::default()
            },
            fetch: Some(fetch),
            ..Default::default()
        };
        let err = begin_auth("linear", "https://mcp.linear.app/sse", &opts)
            .await
            .expect_err("a throw")
            .to_string();
        assert_eq!(
            declared_resource(&err, "https://mcp.linear.app/sse").as_deref(),
            Some("https://mcp.linear.app/mcp"),
            "{err}"
        );
    }

    // ---- a pre-registered OAuth client -------------------------------------

    /// A registry lookup that answers for `slack` and nothing else.
    fn slack_registry(
        client_id: Option<&str>,
        secret: Result<Option<&str>, &str>,
    ) -> RegistryAccess {
        let client_id = client_id.map(|s| s.to_string());
        let secret = secret
            .map(|s| s.map(|s| s.to_string()))
            .map_err(|e| e.to_string());
        RegistryAccess {
            lookup: Some(Arc::new(move |name| {
                if name != "slack" {
                    return Ok(None);
                }
                let client_secret = match &secret {
                    Ok(s) => s.clone(),
                    Err(var) => {
                        return Err(mcp(
                            400,
                            format!(
                                "clientSecret refers to ${{{var}}}, which is not set — the value \
                                 is never stored in the registry."
                            ),
                        ))
                    }
                };
                Ok(Some(RegistryEntry {
                    url: Some("https://mcp.slack.com/mcp".into()),
                    client_id: client_id.clone(),
                    client_secret,
                }))
            })),
            set_url: None,
        }
    }

    #[test]
    fn a_static_client_id_is_used_when_nothing_was_ever_registered() {
        let dir = temp_dir("static");
        let provider = BoughOAuthProvider::new(
            "slack",
            &ProviderOptions {
                dir: Some(dir),
                config: slack_registry(Some("1234.5678"), Ok(Some("shhh"))),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            provider.client_information().unwrap(),
            Some(ClientInfo {
                client_id: "1234.5678".into(),
                client_secret: Some("shhh".into()),
                extra: Map::new(),
            })
        );
    }

    #[test]
    fn an_unset_secret_variable_names_itself() {
        // Without this the flow reaches the authorization server with an empty secret
        // and comes back as an opaque 401 — a failure that looks like the provider's.
        let dir = temp_dir("unset");
        let provider = BoughOAuthProvider::new(
            "slack",
            &ProviderOptions {
                dir: Some(dir),
                config: slack_registry(Some("1234.5678"), Err("SLACK_MCP_CLIENT_SECRET")),
                ..Default::default()
            },
        )
        .unwrap();
        let err = provider
            .client_information()
            .expect_err("a throw")
            .to_string();
        assert!(err.contains("SLACK_MCP_CLIENT_SECRET"), "{err}");
    }

    #[test]
    fn a_client_id_with_no_secret_is_a_public_pre_registered_client() {
        let dir = temp_dir("public-pre");
        let provider = BoughOAuthProvider::new(
            "slack",
            &ProviderOptions {
                dir: Some(dir),
                config: slack_registry(Some("1234.5678"), Ok(None)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            provider.client_information().unwrap(),
            Some(ClientInfo {
                client_id: "1234.5678".into(),
                ..Default::default()
            })
        );
    }

    #[test]
    fn a_dynamically_registered_client_shadows_the_static_one() {
        // The registered client is the one the authorization server issued and knows;
        // the static id is only what to fall back on when there was no registration.
        let dir = temp_dir("shadow");
        store_at(&dir)
            .patch("slack", |s| {
                s.client = Some(ClientInfo {
                    client_id: "registered".into(),
                    ..Default::default()
                })
            })
            .unwrap();
        let provider = BoughOAuthProvider::new(
            "slack",
            &ProviderOptions {
                dir: Some(dir),
                config: slack_registry(Some("static"), Ok(Some("shhh"))),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            provider.client_information().unwrap(),
            Some(ClientInfo {
                client_id: "registered".into(),
                ..Default::default()
            })
        );
    }

    #[test]
    fn an_entry_with_no_client_id_still_returns_none_so_dcr_runs_as_before() {
        // The guard on the whole change: a server that never asked for this must reach
        // the registration path untouched.
        let dir = temp_dir("nodcr");
        let provider = BoughOAuthProvider::new(
            "slack",
            &ProviderOptions {
                dir: Some(dir),
                config: slack_registry(None, Ok(None)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(provider.client_information().unwrap(), None);
    }

    #[test]
    fn a_prefilled_token_is_used_only_until_this_server_has_one_of_its_own() {
        let dir = temp_dir("prefill");
        let provider = BoughOAuthProvider::new(
            "claude-ai",
            &ProviderOptions {
                dir: Some(dir.clone()),
                prefill: Some("sk-ant-oat01-FROM-KEYCHAIN".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // Nothing stored: the connection is tried with the credential the machine
        // already has, so a freshly registered server works without anyone pressing `a`.
        assert_eq!(
            provider.tokens().unwrap(),
            Some(OAuthTokens {
                access_token: "sk-ant-oat01-FROM-KEYCHAIN".into(),
                token_type: "Bearer".into(),
                ..Default::default()
            })
        );
        // …and prefill is NOT authorization: nothing was written to the token store.
        assert!(!has_tokens(
            "claude-ai",
            &TokenStoreOptions {
                dir: Some(dir.clone())
            }
        ));

        // Once a real flow completes, what the user authorized WINS.
        provider
            .save_tokens(OAuthTokens {
                access_token: "from-oauth".into(),
                token_type: "Bearer".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            provider.tokens().unwrap().unwrap().access_token,
            "from-oauth"
        );
        assert!(has_tokens(
            "claude-ai",
            &TokenStoreOptions { dir: Some(dir) }
        ));
    }

    #[test]
    fn a_query_string_round_trips() {
        let q = parse_query("?code=the%20code&state=acme.n1&empty=");
        assert_eq!(q.get("code").map(String::as_str), Some("the code"));
        assert_eq!(q.get("state").map(String::as_str), Some("acme.n1"));
        assert_eq!(q.get("empty").map(String::as_str), Some(""));
    }
}
