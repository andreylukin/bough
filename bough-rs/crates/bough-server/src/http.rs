//! The HTTP primitives every handler needs (port of `src/server/http.ts`): the
//! handler shape, the route constructor, and the response helpers.
//!
//! THE INVARIANT THIS HOLDS: **nothing a handler module imports may import a
//! handler module back.** In TS that was a module-initialization-order hazard;
//! in Rust it is ordinary layering — this file stays the leaf, depending on
//! nothing inside `bough-server` (only `bough-core`).
//!
//! A handler returns `Result<Response, BoughError>`; the dispatcher in
//! `app.rs` owns the ONE catch that turns a `BoughError` into `{error}` with
//! its status. A panic in a handler is the "unexpected error" path — reported
//! and answered 500, never a dropped connection.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::Value;

use bough_core::errors::BoughError;
use bough_core::types::AppCtx;

// ---- the handler shape ------------------------------------------------------

/// The pattern's named groups, already narrowed to the ones that actually
/// matched — an optional group that did not match is ABSENT rather than
/// present-and-empty, so a handler can write `params.get("path")` and mean it.
pub type Params = HashMap<String, String>;

/// What a handler evaluates to. `Err(BoughError)` is a domain outcome the
/// dispatcher renders; a panic is a defect it reports and answers 500.
pub type HandlerResult = Result<Response, BoughError>;

/// Every endpoint is `(req, ctx, params)`.
pub type Handler =
    Arc<dyn Fn(Request, AppCtx, Params) -> BoxFuture<'static, HandlerResult> + Send + Sync>;

/// Wrap an async fn into the [`Handler`] shape.
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Request, AppCtx, Params) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = HandlerResult> + Send + 'static,
{
    Arc::new(move |req, ctx, params| Box::pin(f(req, ctx, params)))
}

// ---- the route shape --------------------------------------------------------

/// A compiled pathname pattern: `:name` = one segment, `:name*` = rest-of-path
/// (may be empty and is then absent from params). The URLPattern subset the TS
/// table actually uses — matched groups are used RAW (no percent-decoding);
/// artifacts decode per segment themselves.
#[derive(Clone, Debug)]
pub struct Pattern {
    /// The source string, for duplicate-detection and logs.
    pub pathname: String,
    segments: Vec<Seg>,
}

#[derive(Clone, Debug)]
enum Seg {
    Literal(String),
    Param(String),
    Rest(String),
}

impl Pattern {
    pub fn new(pathname: &str) -> Pattern {
        let segments = pathname
            .split('/')
            .skip(1)
            .map(|s| {
                if let Some(name) = s.strip_prefix(':') {
                    if let Some(name) = name.strip_suffix('*') {
                        Seg::Rest(name.to_string())
                    } else {
                        Seg::Param(name.to_string())
                    }
                } else {
                    Seg::Literal(s.to_string())
                }
            })
            .collect();
        Pattern { pathname: pathname.to_string(), segments }
    }

    /// Match a request pathname; `Some(params)` carries only the groups that
    /// participated.
    pub fn matches(&self, path: &str) -> Option<Params> {
        let parts: Vec<&str> = path.split('/').skip(1).collect();
        let mut params = Params::new();
        let mut i = 0;
        for (idx, seg) in self.segments.iter().enumerate() {
            match seg {
                Seg::Literal(lit) => {
                    if parts.get(i) != Some(&lit.as_str()) {
                        return None;
                    }
                    i += 1;
                }
                Seg::Param(name) => {
                    let part = *parts.get(i)?;
                    if part.is_empty() {
                        return None;
                    }
                    params.insert(name.clone(), part.to_string());
                    i += 1;
                }
                Seg::Rest(name) => {
                    // Only valid as the final segment; absorbs the remainder.
                    if idx != self.segments.len() - 1 {
                        return None;
                    }
                    let rest = &parts[i.min(parts.len())..];
                    if !rest.is_empty() {
                        params.insert(name.clone(), rest.join("/"));
                    }
                    return Some(params);
                }
            }
        }
        if i == parts.len() { Some(params) } else { None }
    }
}

/// One route table entry. Matched exactly against the request method, in table
/// order — first match wins.
#[derive(Clone)]
pub struct Route {
    pub method: &'static str,
    pub pattern: Pattern,
    pub handler: Handler,
}

/// Build one route entry — the one-line append shape the shared table is made of.
pub fn route(method: &'static str, pathname: &str, handler: Handler) -> Route {
    Route { method, pattern: Pattern::new(pathname), handler }
}

// ---- response helpers -------------------------------------------------------

/// A JSON response with the exact content type the TS server sends.
pub fn json(body: &impl serde::Serialize, status: u16) -> Response {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "null".to_string());
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .body(Body::from(text))
        .expect("static response parts")
}

/// The error envelope: `{"error": message}` — the shape every client reads.
pub fn error_response(status: u16, message: &str) -> Response {
    json(&serde_json::json!({ "error": message }), status)
}

// ---- body parsing -----------------------------------------------------------

/// Parse and validate a JSON request body.
///
/// A failed parse yields the 400 the dispatcher's one catch renders, so no
/// handler branches on validation. `fallback` stands in for an absent or
/// unparseable body: the default `None` (= JSON null) lets the shape decide —
/// an all-optional body would reject null, so such a route passes `Some({})`.
pub async fn parse_body<T: DeserializeOwned>(
    req: Request,
    fallback: Option<Value>,
) -> Result<T, BoughError> {
    let bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .map_err(|e| BoughError::bad_request(format!("invalid body: {e}")))?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(fallback.unwrap_or(Value::Null));
    serde_json::from_value(value)
        .map_err(|e| BoughError::bad_request(format!("invalid body: {e}")))
}

// ---- test fixtures (shared across this crate's handler tests) ---------------

#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::{Arc, Mutex, RwLock};

    use axum::body::Body;
    use axum::extract::Request;
    use axum::response::Response;

    use bough_core::bus::Bus;
    use bough_core::db::sqlite_db::{DbOptions, SqliteDb};
    use bough_core::schema::events::BoughEvent;
    use bough_core::schema::parts::{Message, Session};
    use bough_core::turn::queue::TurnRegistry;
    use bough_core::types::{system_clock, AppCtx, HostState, SharedDb, TurnStarter};

    pub struct Fixture {
        pub ctx: AppCtx,
        /// Every event the bus published, in order.
        pub events: Arc<Mutex<Vec<BoughEvent>>>,
        /// What the recording turn starter was handed.
        pub started: Arc<Mutex<Vec<(Session, Message)>>>,
    }

    struct RecordingStarter(Arc<Mutex<Vec<(Session, Message)>>>);
    impl TurnStarter for RecordingStarter {
        fn start_turn(&self, _ctx: &AppCtx, session: &Session, message: &Message) {
            self.0.lock().unwrap().push((session.clone(), message.clone()));
        }
    }

    struct PanickingStarter(&'static str);
    impl TurnStarter for PanickingStarter {
        fn start_turn(&self, _ctx: &AppCtx, _session: &Session, _message: &Message) {
            panic!("{}", self.0);
        }
    }

    /// A fabricated ctx: real bus with a collector subscribed, in-memory
    /// database, recording turn starter, temp model-defaults path. No socket,
    /// no `~/.bough`.
    pub fn fixture() -> Fixture {
        let f = fixture_bare();
        let starter: Arc<dyn TurnStarter> = Arc::new(RecordingStarter(f.started.clone()));
        *f.ctx.starter.write().unwrap() = Some(starter);
        f
    }

    /// Like [`fixture`] but with NO turn starter wired (the M1 shape).
    pub fn fixture_bare() -> Fixture {
        let db: SharedDb =
            Arc::new(Mutex::new(SqliteDb::new(":memory:", DbOptions::default()).unwrap()));
        let bus = Arc::new(Bus::new(system_clock()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e: &BoughEvent| sink.lock().unwrap().push(e.clone())));
        let started = Arc::new(Mutex::new(Vec::new()));
        let ctx = AppCtx {
            db,
            bus,
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now: system_clock(),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: Arc::new(TurnRegistry::new()),
            // A path that does not exist, so tests read the install default as
            // "unpinned" whatever the developer has actually pinned.
            model_defaults_path: Some(
                std::env::temp_dir()
                    .join(format!("bough-test-{}", uuid::Uuid::new_v4()))
                    .join("model.json"),
            ),
        };
        Fixture { ctx, events, started }
    }

    /// Install a starter that panics with `msg` — the "throwing starter" case.
    pub fn install_panicking_starter(ctx: &AppCtx, msg: &'static str) {
        *ctx.starter.write().unwrap() = Some(Arc::new(PanickingStarter(msg)));
    }

    pub fn get(path: &str) -> Request {
        Request::builder().method("GET").uri(path).body(Body::empty()).unwrap()
    }

    pub fn req(method: &str, path: &str, body: Option<serde_json::Value>) -> Request {
        let b = match body {
            Some(v) => Body::from(serde_json::to_string(&v).unwrap()),
            None => Body::empty(),
        };
        Request::builder().method(method).uri(path).body(b).unwrap()
    }

    pub async fn body_json(res: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    pub async fn body_text(res: Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, Debug)]
    struct BodyShape {
        text: String,
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct AllOptional {
        #[serde(default)]
        paths: Option<Vec<String>>,
    }

    #[tokio::test]
    async fn json_carries_the_json_content_type_and_the_status_it_was_given() {
        let res = json(&serde_json::json!({"a": 1}), 200);
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "application/json; charset=utf-8"
        );
        assert_eq!(testutil::body_json(res).await, serde_json::json!({"a": 1}));
        assert_eq!(json(&serde_json::json!({"a": 1}), 201).status(), 201);
    }

    #[tokio::test]
    async fn error_response_is_the_one_envelope_every_client_reads() {
        let res = error_response(404, "no session x");
        assert_eq!(res.status(), 404);
        assert_eq!(
            testutil::body_json(res).await,
            serde_json::json!({"error": "no session x"})
        );
    }

    #[tokio::test]
    async fn parse_body_validates_and_a_bad_body_becomes_a_catchable_400() {
        let ok: BodyShape =
            parse_body(testutil::req("POST", "/m", Some(serde_json::json!({"text": "a"}))), None)
                .await
                .unwrap();
        assert_eq!(ok.text, "a");

        let bad = parse_body::<BodyShape>(
            testutil::req("POST", "/m", Some(serde_json::json!({"text": 42}))),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(bad.status(), 400);
        assert!(bad.to_string().starts_with("invalid body"), "{bad}");
    }

    #[tokio::test]
    async fn parse_body_fallback_stands_in_for_an_absent_or_unparseable_body() {
        // Default fallback null: a required-field shape rejects it.
        let strict = parse_body::<BodyShape>(testutil::req("POST", "/m", None), None).await;
        assert_eq!(strict.unwrap_err().status(), 400);
        // An all-optional shape passes `{}` so "no body" means "no options".
        let lenient: AllOptional =
            parse_body(testutil::req("POST", "/m", None), Some(serde_json::json!({})))
                .await
                .unwrap();
        assert_eq!(lenient, AllOptional { paths: None });
    }

    #[test]
    fn route_compiles_its_pathname_and_keeps_the_method_verbatim() {
        let r = route("POST", "/sessions/:id/messages", handler(|_r, _c, _p| async {
            Ok(json(&serde_json::json!({}), 200))
        }));
        assert_eq!(r.method, "POST");
        assert!(r.pattern.matches("/sessions/abc/messages").is_some());
        assert!(r.pattern.matches("/sessions/abc").is_none());
    }

    #[test]
    fn pattern_extracts_named_groups() {
        let p = Pattern::new("/sessions/:id/jobs/:jobId");
        let params = p.matches("/sessions/abc/jobs/bg_1").unwrap();
        assert_eq!(params.get("id").unwrap(), "abc");
        assert_eq!(params.get("jobId").unwrap(), "bg_1");
    }

    #[test]
    fn pattern_omits_an_optional_rest_group_that_did_not_match() {
        let p = Pattern::new("/artifacts/:id/:path*");
        let bare = p.matches("/artifacts/s1").unwrap();
        assert!(!bare.contains_key("path"));
        assert_eq!(bare.get("id").unwrap(), "s1");
        let deep = p.matches("/artifacts/s1/deep/page.html").unwrap();
        assert_eq!(deep.get("path").unwrap(), "deep/page.html");
    }

    #[test]
    fn pattern_root_matches_only_root() {
        let p = Pattern::new("/");
        assert!(p.matches("/").is_some());
        assert!(p.matches("/sessions").is_none());
    }
}
