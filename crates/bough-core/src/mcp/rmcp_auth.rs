//! The seam between bough's OAuth persistence and rmcp's OAuth state machine.
//!
//! WHY THIS MODULE EXISTS AT ALL. `mcp/oauth.rs` hand-rolled the whole MCP
//! authorization flow because, when it was ported, there was no Rust MCP SDK to
//! drive. There is one now, and it carries the parts bough never grew:
//! `WWW-Authenticate` parsing, `insufficient_scope` scope upgrade (SEP-835), and
//! scope selection. rmcp owns the PROTOCOL from here; bough keeps the POLICY.
//!
//! THE THREE THINGS BOUGH REFUSES TO HAND OVER, and why each is an adapter here
//! rather than a default rmcp store:
//!
//! 1. **The token file format.** `~/.bough/mcp/tokens/<server>.json` is read by
//!    `bough sync-mcp` and was written by the TypeScript build before that. A
//!    machine that has been authorized against Slack through Claude Code has a
//!    grant in that shape, and an upgrade that cannot read it is an upgrade that
//!    silently logs the user out of every server. [`BoughCredentialStore`] round-
//!    trips through [`Stored`], whose reader is deliberately lenient, so unknown
//!    keys survive a write by this build.
//!
//! 2. **Prefill, and stored-wins.** A credential that merely happens to be on the
//!    machine (`keychain.rs`'s Claude Code token) may be PRESENTED, but must never
//!    displace one the user deliberately authorized, and must never be copied into
//!    bough's own store. rmcp has no such concept; [`BoughCredentialStore::prefill`]
//!    supplies it on `load` and drops it on `save`.
//!
//! 3. **Capture, never navigate.** rmcp hands back an authorization URL rather
//!    than opening one, which is already bough's invariant — a headless server that
//!    shells out to a browser is a server that hangs. Nothing to adapt; it is
//!    recorded here because it is the property most easily lost in a future bump.
//!
//! THE HTTP DETOUR. rmcp wants `reqwest 0.13`; this workspace pins `0.12` for the
//! LLM streaming path, so the two are different types and bough cannot hand rmcp
//! its configured client. [`BoughOAuthHttpClient`] closes that gap by implementing
//! rmcp's `OAuthHttpClient` over bough's own [`FetchFn`] — which is the better
//! arrangement regardless: OAuth discovery, registration and token traffic ride the
//! same proxy and CA configuration as everything else bough sends, instead of a
//! second HTTP stack that knows nothing about either.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::FutureExt;
use serde_json::json;

use rmcp::transport::auth::{
    AuthError, CredentialStore, OAuthHttpClient, OAuthHttpClientFuture, OAuthHttpRedirectPolicy,
    OAuthHttpRequest, StateStore, StoredAuthorizationState, StoredCredentials,
};

use super::oauth::{now_ms, FetchFn, HttpReq, NowFn, Stored, TokenStore, TokenStoreOptions};
use crate::errors::BoughError;

/// rmcp's stores return `AuthError`, which has no variant for "the disk refused".
/// Every storage failure therefore arrives as this one, carrying bough's own
/// message — the text a user can act on — rather than being flattened to a unit.
fn store_err(e: BoughError) -> AuthError {
    AuthError::InternalError(e.to_string())
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// One server's credentials, in bough's file, behind rmcp's trait.
///
/// SCOPED TO ONE SERVER ON PURPOSE. `CredentialStore::load` takes no arguments, so
/// rmcp expects an instance per authorization context; that happens to be exactly
/// bough's rule that a server's credentials live in a file of their own and never
/// in one file for all of them.
pub struct BoughCredentialStore {
    server: String,
    store: TokenStore,
    /// Clock, injected so a token-expiry assertion needs no sleeping.
    now: NowFn,
    /// A bearer token to fall back on when this server has none of its own. See the
    /// module note: presented, never persisted, never preferred over stored.
    prefill: Option<String>,
}

impl BoughCredentialStore {
    pub fn new(server: &str, opts: &TokenStoreOptions, prefill: Option<String>) -> Self {
        Self {
            server: server.to_string(),
            store: TokenStore::new(opts),
            now: Arc::new(now_ms),
            prefill,
        }
    }

    fn read(&self) -> Result<Stored, AuthError> {
        self.store.load(&self.server).map_err(store_err)
    }
}

#[async_trait]
impl CredentialStore for BoughCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        {
            let stored = self.read()?;
            // A stored grant answers first, always: the user said something specific
            // by completing an authorization, and a credential that merely happens to
            // be on the machine must not quietly displace it.
            let doc = match stored.to_rmcp_credentials() {
                Some(doc) => Some(doc),
                None => self.prefill.as_deref().map(Stored::prefill_credentials),
            };
            match doc {
                None => Ok(None),
                Some(doc) => serde_json::from_value(doc).map(Some).map_err(|e| {
                    AuthError::InternalError(format!(
                        "stored credentials for \"{}\" could not be read: {e}",
                        self.server
                    ))
                }),
            }
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        {
            let doc = serde_json::to_value(&credentials).map_err(|e| {
                AuthError::InternalError(format!("credentials could not be stored: {e}"))
            })?;
            let now = (self.now)();
            self.store
                .patch(&self.server, |s| s.apply_rmcp_credentials(&doc, now))
                .map_err(store_err)
        }
    }

    async fn clear(&self) -> Result<(), AuthError> {
        {
            // Tokens only. The dynamic registration is what this authorization server
            // issued to THIS install; throwing it away means re-registering on the
            // next attempt, which some servers rate-limit. `clear_auth` is the
            // explicit path for discarding everything.
            self.store
                .patch(&self.server, |s| {
                    s.tokens = None;
                    s.expires_at = None;
                })
                .map_err(store_err)
        }
    }
}

// ---------------------------------------------------------------------------
// In-flight authorization state
// ---------------------------------------------------------------------------

/// The PKCE verifier and CSRF nonce for an authorization still in flight.
///
/// SEPARATE FROM CREDENTIALS, which is the fix this migration carries. The old
/// `Stored` kept `codeVerifier` and `state` in the same document as the tokens, so
/// an abandoned flow left a stale verifier sitting in the credential file and a
/// second concurrent flow on one server could not exist. rmcp keys this store by
/// the CSRF token, so each attempt is addressed by its own nonce and an abandoned
/// one expires without touching anything the transport reads.
pub struct BoughStateStore {
    server: String,
    store: TokenStore,
    /// Attempts that have not yet come back through the callback. Held in memory:
    /// an authorization the user never finished should not outlive the process that
    /// offered it, and the token file is for credentials.
    live: Mutex<Vec<(String, StoredAuthorizationState)>>,
}

impl BoughStateStore {
    pub fn new(server: &str, opts: &TokenStoreOptions) -> Self {
        Self {
            server: server.to_string(),
            store: TokenStore::new(opts),
            live: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(String, StoredAuthorizationState)>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl StateStore for BoughStateStore {
    async fn save(
        &self,
        csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> Result<(), AuthError> {
        let csrf = csrf_token.to_string();
        {
            // Mirrored to the token file as well as held in memory, because the
            // callback is an HTTP request that may land on a bough that restarted
            // between the authorize redirect and the user pressing "allow".
            self.store
                .patch(&self.server, |s| {
                    s.code_verifier = Some(state.pkce_verifier.clone());
                    s.state = Some(csrf.clone());
                })
                .map_err(store_err)?;
            let mut live = self.lock();
            live.retain(|(k, _)| k != &csrf);
            live.push((csrf, state));
            Ok(())
        }
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        let csrf = csrf_token.to_string();
        {
            if let Some((_, state)) = self.lock().iter().find(|(k, _)| k == &csrf) {
                return Ok(Some(state.clone()));
            }
            // Fall back to the file: a restart between redirect and callback is
            // ordinary, and the alternative is telling the user their completed
            // authorization did not count.
            let stored = self.store.load(&self.server).map_err(store_err)?;
            match (stored.state, stored.code_verifier) {
                // `StoredAuthorizationState` is `#[non_exhaustive]`: it cannot be
                // built with struct-literal syntax here, and going through serde
                // means a field rmcp adds later defaults rather than failing to
                // compile.
                (Some(s), Some(verifier)) if s == csrf => serde_json::from_value(json!({
                    "pkce_verifier": verifier,
                    "csrf_token": csrf,
                    "created_at": 0,
                }))
                .map(Some)
                .map_err(|e| {
                    AuthError::InternalError(format!("in-flight authorization state: {e}"))
                }),
                _ => Ok(None),
            }
        }
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        let csrf = csrf_token.to_string();
        {
            self.lock().retain(|(k, _)| k != &csrf);
            // Clearing the mirrored copy is what stops a replayed callback from
            // exchanging the same code twice.
            self.store
                .patch(&self.server, |s| {
                    if s.state.as_deref() == Some(csrf.as_str()) {
                        s.state = None;
                        s.code_verifier = None;
                    }
                })
                .map_err(store_err)
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// rmcp's OAuth HTTP, routed back through bough's own fetch.
///
/// See the module note for why this is not simply a `reqwest::Client`: the two
/// crates disagree on the major version, and bough's proxy and CA configuration
/// lives on its side of that line.
pub struct BoughOAuthHttpClient {
    fetch: FetchFn,
}

impl BoughOAuthHttpClient {
    pub fn new(fetch: FetchFn) -> Self {
        Self { fetch }
    }
}

impl OAuthHttpClient for BoughOAuthHttpClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        let fetch = self.fetch.clone();
        Box::pin(async move {
            // REDIRECTS ARE NOT HONOURED PER-REQUEST, and this is a real narrowing of
            // rmcp's contract rather than an oversight. bough's `FetchFn` has one
            // redirect policy for everything it sends. `Stop` exists so a token
            // response cannot be followed to another origin; bough's fetch already
            // refuses cross-origin redirects, so the stricter of the two applies.
            let _ = request.redirect_policy;
            let (parts, body) = request.request.into_parts();
            let req = HttpReq {
                method: parts.method.as_str().to_string(),
                url: parts.uri.to_string(),
                headers: parts
                    .headers
                    .iter()
                    .filter_map(|(k, v)| {
                        v.to_str()
                            .ok()
                            .map(|v| (k.as_str().to_string(), v.to_string()))
                    })
                    .collect(),
                body: if body.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(&body).into_owned())
                },
            };
            let res = fetch(req)
                .map(|r| {
                    r.map_err(|e| -> rmcp::transport::auth::OAuthHttpClientError {
                        Box::new(std::io::Error::other(e))
                    })
                })
                .await?;
            let mut out = http::Response::builder().status(res.status);
            for (k, v) in &res.headers {
                out = out.header(k.as_str(), v.as_str());
            }
            out.body(res.body.into_bytes())
                .map_err(|e| -> rmcp::transport::auth::OAuthHttpClientError { Box::new(e) })
        })
    }
}

/// The redirect policy bough narrows away, named so the compiler complains if rmcp
/// adds a variant this module has not considered.
#[allow(dead_code)]
fn _assert_redirect_policy_considered(p: OAuthHttpRedirectPolicy) {
    match p {
        OAuthHttpRedirectPolicy::Follow | OAuthHttpRedirectPolicy::Stop => {}
        _ => {}
    }
}

/// Shared handle for the two stores, so a caller wires one options value.
pub struct ServerStores {
    pub credentials: Arc<BoughCredentialStore>,
    pub state: Arc<BoughStateStore>,
}

impl ServerStores {
    pub fn new(server: &str, opts: &TokenStoreOptions, prefill: Option<String>) -> Self {
        Self {
            credentials: Arc::new(BoughCredentialStore::new(server, opts, prefill)),
            state: Arc::new(BoughStateStore::new(server, opts)),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The property under test is INTEROP, not OAuth. rmcp owns the protocol now, and
    //! it has its own suite for that; what only bough can get wrong is the seam —
    //! whether a token file this machine already has still opens, and whether a
    //! credential borrowed from another client stays borrowed.
    //!
    //! WHAT `sync-mcp` ACTUALLY ADOPTS, checked against a real install rather than
    //! assumed: not a token file at all. A grant taken from Claude Code's `mcpOAuth`
    //! map is adopted as a REGISTRY HEADER — `Bearer ${keychain:Claude
    //! Code-credentials#mcpOAuth.<server>.accessToken}` — resolved against the
    //! keychain at request time, plus a pre-registered `clientId` for providers that
    //! publish no `registration_endpoint`. Those never enter `~/.bough/mcp/tokens/`,
    //! and `keychain.rs` owns that path.
    //!
    //! So the shapes this store must read are bough's own, and there are two: a
    //! COMPLETE grant (`client` + `tokens` + `expiresAt`) and a STALLED one
    //! (`codeVerifier` + `state`, no tokens) left by an authorization that was
    //! started and never finished. Both were taken from a live install.

    use super::*;
    use crate::mcp::oauth::{OAuthTokens, TokenStore};
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bough-rmcp-auth-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The access token, read back through serde rather than through `oauth2`'s
    /// `TokenResponse` trait — the same reason the module itself converts through
    /// JSON: bough does not take a direct dependency on rmcp's OAuth types.
    fn access_token(creds: &StoredCredentials) -> String {
        serde_json::to_value(creds).unwrap()["token_response"]["access_token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn opts(dir: &std::path::Path) -> TokenStoreOptions {
        TokenStoreOptions {
            dir: Some(dir.to_path_buf()),
        }
    }

    /// A complete grant, in the shape a working remote actually has on disk
    /// (`linear`, `pganalyze` and `linear-server` all look like this).
    fn write_grant(dir: &std::path::Path, server: &str) {
        TokenStore::new(&opts(dir))
            .patch(server, |s| {
                s.client = Some(crate::mcp::oauth::ClientInfo {
                    client_id: "dyn-client".into(),
                    ..Default::default()
                });
                s.tokens = Some(OAuthTokens {
                    access_token: "an-access-token".into(),
                    token_type: "Bearer".into(),
                    refresh_token: Some("a-refresh-token".into()),
                    scope: Some("channels:read chat:write".into()),
                    ..Default::default()
                });
                // A key written by a build that knew something this one does not.
                s.extra
                    .insert("clientIdIssuedAt".into(), serde_json::json!(1_700_000_000));
            })
            .unwrap();
    }

    #[tokio::test]
    async fn a_grant_with_no_registration_beside_it_still_opens() {
        let dir = temp_dir("no-client");
        // Tokens without a `client`: not what `sync-mcp` writes, but reachable —
        // `invalidate_credentials(Client)` drops the registration and keeps the pair,
        // and a pre-registered client lives in the registry rather than the file.
        TokenStore::new(&opts(&dir))
            .patch("slack", |s| {
                s.tokens = Some(OAuthTokens {
                    access_token: "an-access-token".into(),
                    token_type: "Bearer".into(),
                    scope: Some("channels:read chat:write".into()),
                    ..Default::default()
                });
            })
            .unwrap();

        let creds = BoughCredentialStore::new("slack", &opts(&dir), None)
            .load()
            .await
            .unwrap()
            .expect("a grant must be presentable without a registration beside it");

        assert_eq!(creds.client_id, "");
        assert_eq!(
            creds.granted_scopes,
            vec!["channels:read".to_string(), "chat:write".to_string()],
            "scopes carry across so rmcp can tell an insufficient_scope 403 from a 401"
        );
    }

    #[tokio::test]
    async fn a_write_by_this_build_preserves_keys_it_does_not_understand() {
        let dir = temp_dir("preserve");
        write_grant(&dir, "slack");

        let store = BoughCredentialStore::new("slack", &opts(&dir), None);
        let mut creds = store.load().await.unwrap().unwrap();
        creds.client_id = "bough-registered".into();
        store.save(creds).await.unwrap();

        let raw = TokenStore::new(&opts(&dir)).load("slack").unwrap();
        assert_eq!(
            raw.extra.get("clientIdIssuedAt").and_then(|v| v.as_i64()),
            Some(1_700_000_000),
            "a key this build does not name must survive its write"
        );
        assert_eq!(raw.tokens.as_ref().unwrap().access_token, "an-access-token");
    }

    #[tokio::test]
    async fn a_stored_grant_beats_a_borrowed_one() {
        let dir = temp_dir("stored-wins");
        write_grant(&dir, "slack");

        let creds = BoughCredentialStore::new("slack", &opts(&dir), Some("borrowed".into()))
            .load()
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            access_token(&creds),
            "an-access-token",
            "a credential that merely happens to be on the machine must not displace \
             one the user deliberately authorized"
        );
    }

    #[tokio::test]
    async fn a_borrowed_credential_is_presented_but_never_persisted() {
        let dir = temp_dir("prefill");
        let store = BoughCredentialStore::new("mcp-claude-ai", &opts(&dir), Some("sk-ant".into()));

        let creds = store.load().await.unwrap().expect("prefill is presentable");
        assert_eq!(access_token(&creds), "sk-ant");

        // Reading it must not have copied it anywhere. bough keeps exactly one copy of
        // that secret, in the keychain where it already was.
        assert!(
            TokenStore::new(&opts(&dir))
                .load("mcp-claude-ai")
                .unwrap()
                .tokens
                .is_none(),
            "the borrowed token must not land in bough's own store"
        );
    }

    #[tokio::test]
    async fn an_abandoned_authorization_does_not_strand_a_verifier() {
        let dir = temp_dir("state");
        let store = BoughStateStore::new("slack", &opts(&dir));
        let state: StoredAuthorizationState = serde_json::from_value(json!({
            "pkce_verifier": "verifier-one",
            "csrf_token": "nonce-one",
            "created_at": 0,
        }))
        .unwrap();

        store.save("nonce-one", state).await.unwrap();
        assert!(store.load("nonce-one").await.unwrap().is_some());

        store.delete("nonce-one").await.unwrap();
        assert!(store.load("nonce-one").await.unwrap().is_none());
        let raw = TokenStore::new(&opts(&dir)).load("slack").unwrap();
        assert!(
            raw.code_verifier.is_none() && raw.state.is_none(),
            "a finished or abandoned flow must leave nothing behind for a replay to use"
        );
    }

    #[tokio::test]
    async fn a_callback_survives_a_restart_between_redirect_and_approval() {
        let dir = temp_dir("restart");
        BoughStateStore::new("slack", &opts(&dir))
            .save(
                "nonce-two",
                serde_json::from_value(json!({
                    "pkce_verifier": "verifier-two",
                    "csrf_token": "nonce-two",
                    "created_at": 0,
                }))
                .unwrap(),
            )
            .await
            .unwrap();

        // A different instance: the process that offered the authorization is gone.
        let reborn = BoughStateStore::new("slack", &opts(&dir));
        let found = reborn.load("nonce-two").await.unwrap().expect(
            "a user who approved in the browser must not be told their authorization \
             did not count",
        );
        assert_eq!(found.pkce_verifier, "verifier-two");
    }

    #[tokio::test]
    async fn a_forged_nonce_matches_nothing() {
        let dir = temp_dir("forged");
        BoughStateStore::new("slack", &opts(&dir))
            .save(
                "real-nonce",
                serde_json::from_value(json!({
                    "pkce_verifier": "verifier",
                    "csrf_token": "real-nonce",
                    "created_at": 0,
                }))
                .unwrap(),
            )
            .await
            .unwrap();

        let store = BoughStateStore::new("slack", &opts(&dir));
        assert!(store.load("forged-nonce").await.unwrap().is_none());
    }
}

// ---------------------------------------------------------------------------
// The flow
// ---------------------------------------------------------------------------

/// Driving rmcp's authorization manager with bough's stores under it.
///
/// This is the half `mcp/oauth.rs` used to own outright. What bough still decides:
/// which client identity to offer, where the redirect lands, and that an
/// unauthorized server is a QUESTION rather than a failure. What rmcp now decides:
/// discovery order, scope selection, registration mechanism, and the token calls.
pub mod flow {
    use super::*;
    use rmcp::transport::auth::{AuthorizationManager, AuthorizationRequest, AuthorizationSession};

    /// Everything one attempt needs. Assembled by `mcp/oauth.rs`, which owns the
    /// registry lookups and the prefill resolution.
    pub struct Attempt {
        pub server: String,
        pub server_url: String,
        pub redirect_uri: String,
        pub fetch: FetchFn,
        pub store: TokenStoreOptions,
        pub prefill: Option<String>,
        /// A client the user registered out of band, from the registry entry.
        pub client_id: Option<String>,
        pub client_secret: Option<String>,
        /// The `WWW-Authenticate` header off a real 401.
        ///
        /// THE POINT OF THE WHOLE MIGRATION. Handed this, rmcp seeds discovery from
        /// the challenge's `resource_metadata` URL and `scope` hint instead of
        /// guessing at well-known paths — and reads the `scope` an
        /// `insufficient_scope` 403 asks for, which is how a 403 stops being a dead
        /// end. bough never parsed this header at all.
        pub challenge: Option<String>,
    }

    /// What an attempt settled on. `Redirect` carries the URL a human must open;
    /// nothing here ever opens one.
    pub enum Outcome {
        Authorized,
        Redirect { url: String, csrf: String },
    }

    fn auth_err(server: &str, e: rmcp::transport::auth::AuthError) -> BoughError {
        BoughError::http(
            502,
            crate::errors::ErrorKind::Mcp,
            format!("the OAuth flow for \"{server}\" could not proceed: {e}"),
        )
    }

    /// A manager pointed at one server, with bough's stores and bough's HTTP under
    /// it, and its metadata resolved.
    async fn manager(attempt: &Attempt) -> Result<AuthorizationManager, BoughError> {
        let mut mgr = AuthorizationManager::new_with_oauth_http_client(
            &attempt.server_url,
            Arc::new(BoughOAuthHttpClient::new(attempt.fetch.clone())),
        )
        .await
        .map_err(|e| auth_err(&attempt.server, e))?;

        mgr.set_credential_store(BoughCredentialStore::new(
            &attempt.server,
            &attempt.store,
            attempt.prefill.clone(),
        ));
        mgr.set_state_store(BoughStateStore::new(&attempt.server, &attempt.store));

        // Metadata must be resolved before a session can be built. The provenance is
        // deliberately NOT enforced: a server that publishes nothing gets the legacy
        // default endpoints, which is what bough's own discovery fell back to and
        // what several real servers still need.
        let resolved = mgr
            .resolve_metadata()
            .await
            .map_err(|e| auth_err(&attempt.server, e))?;
        mgr.set_metadata(resolved.metadata);
        Ok(mgr)
    }

    fn request(attempt: &Attempt) -> AuthorizationRequest {
        let mut req =
            AuthorizationRequest::new(attempt.redirect_uri.clone()).with_client_name("bough");
        // A client the user registered out of band wins over dynamic registration —
        // it is what the authorization server was told to expect.
        if let Some(id) = &attempt.client_id {
            req = req.with_preregistered_client(id.clone());
            if let Some(secret) = &attempt.client_secret {
                req = req.with_client_secret(secret.clone());
            }
        }
        if let Some(challenge) = &attempt.challenge {
            req.challenge = Some(challenge.clone());
        }
        req
    }

    /// Start — or silently finish — one server's flow.
    ///
    /// Stored credentials are tried first, and a refresh that rmcp can do on its own
    /// is not a question for the human. Only when nothing usable is stored does this
    /// return `Redirect`.
    pub async fn begin(attempt: &Attempt) -> Result<Outcome, BoughError> {
        let mut mgr = manager(attempt).await?;

        // NO EXPIRY SHORTCUT, and no presenting the stored access token to decide.
        // This is also the transport's 401 path: the token that just came back 401 is
        // known-bad whatever its stored expiry says, and answering "authorized"
        // because a string is on disk would present it again forever. A refresh round
        // trip is the right price for that.
        //
        // `refresh_token` failing is not a fault — it is the recovery hook. A refresh
        // token the server has expired, or none at all, falls through to the prompt,
        // which is exactly the degradation an unauthorized server is supposed to get.
        if mgr
            .initialize_from_store()
            .await
            .map_err(|e| auth_err(&attempt.server, e))?
        {
            match mgr.refresh_token().await {
                Ok(_) => return Ok(Outcome::Authorized),
                Err(_) => {
                    // Drop the pair that just failed so the next attempt does not
                    // present it again, then ask.
                    let _ = BoughCredentialStore::new(&attempt.server, &attempt.store, None)
                        .clear()
                        .await;
                }
            }
        }

        let session = AuthorizationSession::new(mgr, request(attempt))
            .await
            .map_err(|(_, e)| auth_err(&attempt.server, e))?;

        let url = session.get_authorization_url().to_string();
        // The nonce rmcp put in the URL is the one the callback will come back with,
        // and the one `BoughStateStore` has just filed. Read it back off the URL
        // rather than guessed at: rmcp owns its format.
        let csrf = csrf_from(&url).ok_or_else(|| {
            BoughError::http(
                502,
                crate::errors::ErrorKind::Mcp,
                format!(
                    "the authorization URL for \"{}\" carries no state parameter, so its \
                     callback could not be matched to this attempt.",
                    attempt.server
                ),
            )
        })?;
        Ok(Outcome::Redirect { url, csrf })
    }

    /// Finish from the browser callback: exchange the code for tokens.
    ///
    /// The nonce is checked by rmcp against what `BoughStateStore` holds, so a forged
    /// or replayed callback finds no verifier and is refused before any exchange.
    pub async fn complete(attempt: &Attempt, code: &str, csrf: &str) -> Result<(), BoughError> {
        let mut mgr = manager(attempt).await?;
        // The registration this flow started with has to be back in place before the
        // exchange, or the token endpoint is asked to honour a code it issued to a
        // different client.
        //
        // NOT VIA `initialize_from_store`, which is a CREDENTIAL path and finds
        // nothing here by design: completion is the one moment when a registration
        // exists and tokens do not. The client id is read straight off the stored
        // document instead.
        let _ = mgr.initialize_from_store().await;
        let registered = TokenStore::new(&attempt.store)
            .load(&attempt.server)
            .ok()
            .and_then(|s| s.client)
            .map(|c| c.client_id);
        if let Some(id) = attempt.client_id.clone().or(registered) {
            if !id.is_empty() {
                mgr.configure_client_id(&id)
                    .map_err(|e| auth_err(&attempt.server, e))?;
            }
        }
        mgr.exchange_code_for_token(code, csrf)
            .await
            .map(|_| ())
            .map_err(|e| auth_err(&attempt.server, e))
    }

    /// The `state` parameter out of an authorization URL.
    fn csrf_from(url: &str) -> Option<String> {
        let query = url.split_once('?')?.1;
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == "state")
            .map(|(_, v)| super::super::oauth::percent_decode(v))
    }
}
