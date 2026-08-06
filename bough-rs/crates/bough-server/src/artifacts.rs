//! Serving artifacts: content types and the two routes (port of
//! `src/server/artifacts.ts`).
//!
//! The store itself — where artifacts live, and the confinement rules for
//! names and session ids — is `bough_core::hostfn::artifact`, because `hostfn`
//! may not reference the server crate and the confinement rules must exist
//! exactly once. This file is the HTTP half: it reads what the store resolved
//! and turns it into a `Response`.
//!
//! TRAVERSAL IS A 403, NOT A 404. "That path is not addressable" and "nothing
//! is there" are different facts, and collapsing them sends whoever is
//! debugging to the wrong place — a mistyped session id reads as a deleted
//! artifact.
//!
//! A 404 that a BROWSER asked for is an HTML page, not a JSON body. Artifact
//! links get opened by the audience artifacts exist for, who are not reading
//! `{"error":"not found"}` in a tab.
//!
//! Trust note, stated rather than implied: artifacts are agent-authored
//! HTML/JS served same-origin, so an opened artifact runs with this origin's
//! privileges. That is deliberate — explicit agent OUTPUT the user chooses to
//! open, not a containment boundary.
//!
//! WAVE-3 GAP (rows 2.7 note + 3.14): the TS server splices the comment
//! widget into every served HTML document at serve time. The comments
//! subsystem is not ported yet, so HTML serves RAW — the sanctioned interim
//! answer (server.md §8: "serve artifacts raw until comments port").
//! `inject_comment_layer` is where it goes when it lands.

use std::path::Path;

use axum::body::Body;
use axum::response::Response;

use bough_core::hostfn::artifact::{
    list_artifacts as store_list_artifacts, resolve_artifact_path, ArtifactStoreOptions,
};

use crate::http::{handler, json, Handler};

// ---------------------------------------------------------------------------
// Content types
// ---------------------------------------------------------------------------

/// The declared content type for a path, or octet-stream when nothing matches.
pub fn content_type_for(path: &str) -> &'static str {
    let ext = match path.rfind('.') {
        Some(dot) => path[dot..].to_ascii_lowercase(),
        None => return "application/octet-stream",
    };
    match ext.as_str() {
        ".html" | ".htm" => "text/html; charset=utf-8",
        ".js" | ".mjs" => "text/javascript; charset=utf-8",
        ".css" => "text/css; charset=utf-8",
        ".json" | ".map" => "application/json; charset=utf-8",
        ".svg" => "image/svg+xml",
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".ico" => "image/x-icon",
        ".woff" => "font/woff",
        ".woff2" => "font/woff2",
        ".txt" | ".md" => "text/plain; charset=utf-8",
        ".csv" => "text/csv; charset=utf-8",
        ".wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Agents publish HTML under a bare name (`my-explorer`) often enough to
/// matter, and an octet-stream response makes the browser download it instead
/// of rendering it. So for an EXTENSIONLESS file only, sniff the first bytes:
/// leading markup → HTML.
fn sniff_html(full: &Path) -> Option<&'static str> {
    use std::io::Read;
    let mut head = [0u8; 64];
    let mut file = std::fs::File::open(full).ok()?;
    let n = file.read(&mut head).ok()?;
    let text = String::from_utf8_lossy(&head[..n]);
    if text.trim_start().starts_with('<') {
        Some("text/html; charset=utf-8")
    } else {
        None
    }
}

fn basename(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Serve
// ---------------------------------------------------------------------------

/// The browser-facing 404. Self-contained, no external anything — the same
/// bar the artifacts themselves are held to.
pub const NOT_FOUND_PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>This page isn't here</title>
<style>
:root { color-scheme: light dark; }
body { margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
  background: #fcfcfb; color: #0b0b0b; font: 14px/1.55 system-ui, sans-serif; }
main { max-width: 34em; padding: 32px 28px; }
.eyebrow { font: 600 11.5px ui-monospace, Menlo, monospace; text-transform: uppercase;
  letter-spacing: 0.08em; color: #52514e; margin: 0 0 14px; }
h1 { font-size: 21px; font-weight: 650; letter-spacing: -0.01em; margin: 0 0 10px; }
p { margin: 0; color: #52514e; }
@media (prefers-color-scheme: dark) {
  body { background: #1a1a19; color: #f4f3ef; }
  .eyebrow, p { color: #c3c2b7; }
}
</style>
<main>
<p class="eyebrow">404 &middot; not found</p>
<h1>This page isn't here</h1>
<p>It may have moved or been replaced. Ask bough to share it again.</p>
</main>
"#;

/// Where the comment layer goes when the comments subsystem lands (row 3.14).
/// Until then: identity — the bytes on disk are the bytes on the wire.
fn inject_comment_layer(html: String) -> String {
    html
}

/// Options for [`serve_artifact`], over the store's own.
#[derive(Clone, Default)]
pub struct ServeArtifactOptions {
    pub store: ArtifactStoreOptions,
    /// The request's `Accept` header — a browser gets the HTML 404, a client
    /// the JSON one.
    pub accept: Option<String>,
}

fn respond(status: u16, content_type: &str, body: impl Into<Body>) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(body.into())
        .expect("static response parts")
}

fn respond_no_cache(content_type: &str, body: impl Into<Body>) -> Response {
    Response::builder()
        .status(200)
        .header("content-type", content_type)
        .header("cache-control", "no-cache")
        .body(body.into())
        .expect("static response parts")
}

/// Serve one artifact file.
///
/// `no-cache` because artifacts are overwritten in place: a cached stale page
/// is indistinguishable from an agent that did nothing, and republishing is
/// the normal way a program iterates.
pub fn serve_artifact(session_id: &str, name: &str, opts: &ServeArtifactOptions) -> Response {
    let full = match resolve_artifact_path(session_id, name, &opts.store) {
        Ok(full) => full,
        // Traversal is a different fact from absence, and says so.
        Err(_) => return respond(403, "text/plain; charset=utf-8", "forbidden"),
    };

    let served = (|| -> Option<Response> {
        let meta = std::fs::metadata(&full).ok()?;
        if !meta.is_file() {
            return None; // a directory is a 404, never a listing
        }
        let mut content_type = content_type_for(&full.to_string_lossy());
        if content_type == "application/octet-stream" && !basename(&full).contains('.') {
            if let Some(sniffed) = sniff_html(&full) {
                content_type = sniffed;
            }
        }
        if content_type.starts_with("text/html") {
            let html = std::fs::read_to_string(&full).ok()?;
            return Some(respond_no_cache(content_type, inject_comment_layer(html)));
        }
        let bytes = std::fs::read(&full).ok()?;
        Some(respond_no_cache(content_type, bytes))
    })();

    match served {
        Some(response) => response,
        None => {
            if opts.accept.as_deref().is_some_and(|a| a.contains("text/html")) {
                respond(404, "text/html; charset=utf-8", NOT_FOUND_PAGE)
            } else {
                respond(
                    404,
                    "application/json; charset=utf-8",
                    serde_json::json!({
                        "error": format!("no artifact {name} for session {session_id}")
                    })
                    .to_string(),
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

/// Percent-decode a matched path, segment by segment.
///
/// The router hands back the raw pathname and the store encodes each segment
/// when it builds `url`, so a name with a space round-trips only if it is
/// decoded here. Per segment, not whole: decoding the whole string would turn
/// an encoded `%2F` inside one segment into a real separator, which is a
/// traversal primitive. A malformed escape decodes to itself and then fails
/// confinement or the stat, rather than erroring out of a handler.
pub fn decode_segments(path: &str) -> String {
    path.split('/')
        .map(|seg| decode_component(seg).unwrap_or_else(|| seg.to_string()))
        .collect::<Vec<_>>()
        .join("/")
}

/// `decodeURIComponent` for one segment; `None` on a malformed escape or
/// invalid UTF-8 (the caller keeps the raw segment).
fn decode_component(seg: &str) -> Option<String> {
    let bytes = seg.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `GET /sessions/:id/artifacts` — what this session has published.
///
/// Answered from the filesystem, so it is correct for a session whose row is
/// gone and for artifacts published by a previous process. It deliberately
/// does NOT check that the session exists: the artifacts outlive the row, and
/// 404-ing here would hide files that are demonstrably on disk.
pub fn list_artifacts() -> Handler {
    handler(|_req, _ctx, params| async move {
        let id = decode_segments(params.get("id").map(String::as_str).unwrap_or(""));
        let artifacts = store_list_artifacts(&id, &ArtifactStoreOptions::default());
        Ok(json(&serde_json::json!({ "artifacts": artifacts }), 200))
    })
}

/// `GET /artifacts/:id/:path*` — the hosted file itself.
///
/// Same origin as the API on purpose: a link the agent prints is a link the
/// user's browser opens with no extra machinery.
pub fn get_artifact() -> Handler {
    handler(|req, _ctx, params| async move {
        let accept = req
            .headers()
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let id = decode_segments(params.get("id").map(String::as_str).unwrap_or(""));
        let path = decode_segments(params.get("path").map(String::as_str).unwrap_or(""));
        Ok(serve_artifact(&id, &path, &ServeArtifactOptions {
            store: ArtifactStoreOptions::default(),
            accept,
        }))
    })
}

// ---------------------------------------------------------------------------
// Tests — port of the HTTP half of src/server/artifacts.test.ts (the store
// half lives with the store in bough-core). The comments-sidecar AC (never
// listed/served) ports with the comments subsystem in wave 3.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::hostfn::artifact::publish_artifact;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new() -> TmpDir {
            let dir =
                std::env::temp_dir().join(format!("bough-artifacts-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            TmpDir(dir)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store(root: &std::path::Path) -> ServeArtifactOptions {
        ServeArtifactOptions {
            store: ArtifactStoreOptions {
                root: Some(root.to_path_buf()),
                base_url: Some("http://127.0.0.1:4321".into()),
            },
            accept: None,
        }
    }

    /// `BOUGH_HOME` is process-global and the route handlers read the default
    /// paths per call, so the env-touching tests serialize on one lock and
    /// restore on drop (these tests run on tokio's current-thread runtime, so
    /// holding the guard across awaits is sound — the future never migrates).
    struct HomeGuard {
        _lock: MutexGuard<'static, ()>,
        _tmp: TmpDir,
        previous: Option<String>,
    }
    impl HomeGuard {
        fn new() -> HomeGuard {
            static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let lock = LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let tmp = TmpDir::new();
            let previous = std::env::var("BOUGH_HOME").ok();
            std::env::set_var("BOUGH_HOME", &tmp.0);
            HomeGuard { _lock: lock, _tmp: tmp, previous }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var("BOUGH_HOME", v),
                None => std::env::remove_var("BOUGH_HOME"),
            }
        }
    }

    async fn body_text(res: axum::response::Response) -> String {
        testutil::body_text(res).await
    }

    fn header<'a>(res: &'a axum::response::Response, name: &str) -> &'a str {
        res.headers().get(name).map(|v| v.to_str().unwrap()).unwrap_or("")
    }

    // ---- AC 1: traversal is blocked, at the route, as a 403 -----------------

    #[test]
    fn one_session_cannot_read_anothers_artifacts_through_serve() {
        let tmp = TmpDir::new();
        publish_artifact("victim", "secret.html", "<b>secret</b>", &store(&tmp.0).store).unwrap();
        let res = serve_artifact("attacker", "../victim/secret.html", &store(&tmp.0));
        assert_eq!(res.status(), 403);
    }

    #[tokio::test]
    async fn the_artifact_route_rejects_an_escaping_path_with_403_not_404() {
        // Handlers read the default paths, so the whole test runs under a
        // temp BOUGH_HOME.
        let _home = HomeGuard::new();
        publish_artifact("s1", "index.html", "<h1>hi</h1>", &ArtifactStoreOptions::default())
            .unwrap();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        for path in [
            "/artifacts/s1/..%2F..%2Fetc%2Fpasswd",
            "/artifacts/..%2F..%2Fetc/passwd",
            "/artifacts/s1/../../../etc/passwd",
        ] {
            let res = call.call(testutil::get(path)).await;
            let status = res.status().as_u16();
            assert!(status == 403 || status == 404, "{path}: {status}");
            let body = body_text(res).await;
            assert!(!body.contains("root:"), "{path}: {body}");
        }

        // The 403 is reachable through the router for a well-formed escaping id.
        let res = call.call(testutil::get("/artifacts/..%2Fevil/index.html")).await;
        assert_eq!(res.status(), 403);
        assert_eq!(body_text(res).await, "forbidden");
    }

    // ---- AC 2: listing survives a database reset ----------------------------

    #[tokio::test]
    async fn ac_list_artifacts_survives_a_database_reset_no_row_required() {
        let _home = HomeGuard::new();
        let opts = ArtifactStoreOptions::default();
        publish_artifact("ghost", "index.html", "<h1>still here</h1>", &opts).unwrap();
        publish_artifact("ghost", "assets/app.js", "console.log(1)", &opts).unwrap();

        // A brand-new, empty database: nothing knows this session ever existed.
        let fx = testutil::fixture();
        assert!(fx.ctx.db.lock().unwrap().get_session("ghost").unwrap().is_none());
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let res = call.call(testutil::get("/sessions/ghost/artifacts")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        let mut listed: Vec<String> = body["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_string())
            .collect();
        listed.sort();
        assert_eq!(listed, vec!["assets/app.js", "index.html"]);

        let served = call.call(testutil::get("/artifacts/ghost/index.html")).await;
        assert_eq!(served.status(), 200);
        assert!(body_text(served).await.contains("still here"));
    }

    // ---- serving ------------------------------------------------------------

    #[tokio::test]
    async fn serve_artifact_sets_the_content_type_and_never_caches() {
        let tmp = TmpDir::new();
        let opts = store(&tmp.0);
        publish_artifact("s3", "page.html", "<!doctype html><title>x</title>", &opts.store)
            .unwrap();
        publish_artifact("s3", "app.js", "console.log(1)", &opts.store).unwrap();
        publish_artifact("s3", "data.csv", "a,b", &opts.store).unwrap();

        let html = serve_artifact("s3", "page.html", &opts);
        assert_eq!(html.status(), 200);
        assert_eq!(header(&html, "content-type"), "text/html; charset=utf-8");
        assert_eq!(header(&html, "cache-control"), "no-cache");
        assert!(body_text(html).await.contains("<title>x</title>"));

        let js = serve_artifact("s3", "app.js", &opts);
        assert_eq!(header(&js, "content-type"), "text/javascript; charset=utf-8");
        assert_eq!(body_text(js).await, "console.log(1)"); // untouched: no layer in a script

        let csv = serve_artifact("s3", "data.csv", &opts);
        assert_eq!(header(&csv, "content-type"), "text/csv; charset=utf-8");
    }

    #[tokio::test]
    async fn serve_artifact_sniffs_an_extensionless_html_file_so_it_renders() {
        let tmp = TmpDir::new();
        let opts = store(&tmp.0);
        publish_artifact("s7", "my-explorer", "<!doctype html>\n<title>x</title>", &opts.store)
            .unwrap();
        publish_artifact("s7", "notes", "just text", &opts.store).unwrap();

        let html = serve_artifact("s7", "my-explorer", &opts);
        assert_eq!(header(&html, "content-type"), "text/html; charset=utf-8");
        // NOTE: the TS test also asserts the injected comment widget
        // ("bgh-cmt-toggle") here — that assertion ports with the comments
        // subsystem (row 3.14); HTML currently serves raw by design.

        let plain = serve_artifact("s7", "notes", &opts);
        assert_eq!(header(&plain, "content-type"), "application/octet-stream");
    }

    #[tokio::test]
    async fn a_missing_artifact_is_json_for_a_client_and_a_page_for_a_browser() {
        let tmp = TmpDir::new();
        let api = serve_artifact("s5", "nope.html", &store(&tmp.0));
        assert_eq!(api.status(), 404);
        assert_eq!(header(&api, "content-type"), "application/json; charset=utf-8");
        let body = testutil::body_json(api).await;
        assert!(body["error"].as_str().unwrap().contains("nope.html"));

        let browser = serve_artifact(
            "s5",
            "nope.html",
            &ServeArtifactOptions {
                accept: Some("text/html,application/xhtml+xml".into()),
                ..store(&tmp.0)
            },
        );
        assert_eq!(browser.status(), 404);
        assert_eq!(header(&browser, "content-type"), "text/html; charset=utf-8");
        assert_eq!(body_text(browser).await, NOT_FOUND_PAGE);
    }

    #[test]
    fn the_404_page_is_self_contained_no_external_network_references() {
        let lowered = NOT_FOUND_PAGE.to_ascii_lowercase();
        assert!(!lowered.contains("src=\"http"));
        assert!(!lowered.contains("href=\"http"));
        for banned in ["cdn.", "googleapis", "unpkg", "jsdelivr"] {
            assert!(!lowered.contains(banned), "{banned}");
        }
    }

    #[tokio::test]
    async fn a_directory_is_a_404_not_a_directory_listing() {
        let tmp = TmpDir::new();
        publish_artifact("s6", "assets/app.js", "x", &store(&tmp.0).store).unwrap();
        let res = serve_artifact("s6", "assets", &store(&tmp.0));
        assert_eq!(res.status(), 404);
    }

    #[tokio::test]
    async fn a_percent_encoded_name_round_trips_through_the_route() {
        let _home = HomeGuard::new();
        let art = publish_artifact(
            "s8",
            "my report.html",
            "<html><body>ok</body></html>",
            &ArtifactStoreOptions::default(),
        )
        .unwrap();
        assert_eq!(art.url, "/artifacts/s8/my%20report.html");

        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get(&art.url)).await;
        assert_eq!(res.status(), 200);
        assert!(body_text(res).await.contains("ok"));
    }

    #[test]
    fn decode_segments_is_per_segment_and_keeps_malformed_escapes() {
        assert_eq!(decode_segments("my%20report.html"), "my report.html");
        // An encoded %2F inside a segment decodes to a real slash CHARACTER in
        // the string — but only per segment, after the router already split;
        // it can never create a new segment boundary upstream of confinement.
        assert_eq!(decode_segments("a%2Fb"), "a/b");
        // Malformed escapes decode to themselves.
        assert_eq!(decode_segments("bad%GG/esc%2"), "bad%GG/esc%2");
    }
}
