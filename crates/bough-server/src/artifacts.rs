//! Serving artifacts: content types, the routes that host them, and the
//! version verbs (port of `src/server/artifacts.ts`, plus history).
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
//! THE COMMENT LAYER IS SPLICED IN AT SERVE TIME (row 3.14,
//! `inject_comment_layer`), into HTML documents and nothing else: the bytes on
//! disk stay exactly what the agent wrote, so a page the user saves or forwards
//! is the page — not the page plus an annotation toolbar pointed at a loopback
//! server that is not running. THE VERSION BAR ([`version_bar_widget`]) rides
//! the same rule, and shows itself only when there is more than one version to
//! move between.

use std::path::Path;

use axum::body::Body;
use axum::response::Response;

use bough_core::hostfn::artifact::{
    list_artifacts as store_list_artifacts, list_versions as store_list_versions, read_version,
    resolve_artifact_path, restore_version as store_restore_version, ArtifactStoreOptions,
};

use crate::http::{handler, json, parse_body, Handler};

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
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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

/// Splice the comment layer in before `</body>`, or append it when the document
/// has no body tag (a fragment still renders, and the layer still works).
///
/// THE INVARIANT: **every served HTML artifact gets the layer injected AT SERVE
/// TIME**, and nothing else does. Two consequences, both deliberate: the bytes
/// on disk stay exactly what the agent wrote (a page the user saves or forwards
/// is the page, not the page plus an annotation toolbar pointed at a loopback
/// server that is not running), and the layer only exists where it means
/// something, which is inside bough. It goes into HTML documents only —
/// injecting into the page's own CSS or JS, served through the same route,
/// would corrupt them and the layer would not work anyway.
pub fn inject_comment_layer(html: String) -> String {
    splice_before_body(html, crate::comments::comment_widget())
}

/// Splice `widget` in before `</body>`, or append it when the document has no
/// body tag (a fragment still renders, and the widget still works).
///
/// Shared by the comment layer and the version bar so a document served with
/// both still closes its own markup — a widget appended after `</html>` is a
/// page the browser reparses and a test cannot make assertions about.
fn splice_before_body(html: String, widget: &str) -> String {
    // Case-insensitive over the ORIGINAL bytes, never over a lowercased copy:
    // a lowercase mapping can change byte lengths, and an index from the copy
    // would then slice the original mid-codepoint.
    let tag = b"</body>";
    let bytes = html.as_bytes();
    let found = bytes
        .windows(tag.len())
        .rposition(|w| w.eq_ignore_ascii_case(tag))
        .filter(|idx| html.is_char_boundary(*idx));
    match found {
        Some(idx) => format!("{}{widget}{}", &html[..idx], &html[idx..]),
        None => format!("{html}{widget}"),
    }
}

/// Options for [`serve_artifact`], over the store's own.
#[derive(Clone, Default)]
pub struct ServeArtifactOptions {
    pub store: ArtifactStoreOptions,
    /// The request's `Accept` header — a browser gets the HTML 404, a client
    /// the JSON one.
    pub accept: Option<String>,
    /// `?v=<ts>` — serve the bytes this artifact had at that moment instead of
    /// the ones it has now. Absent = current, which is every link that existed
    /// before history did.
    pub version: Option<i64>,
}

/// The version bar: the controls that make an artifact's history navigable.
///
/// Injected at serve time, next to the comment layer and for the same reasons
/// — the bytes on disk stay what the agent wrote, and the controls exist only
/// where they mean something. It renders NOTHING until it knows there is more
/// than one version, so a one-version artifact (most of them) is untouched
/// visually and the page the user opens is the page.
///
/// Stepping is a plain navigation to `?v=<ts>`, not a fetch that swaps the
/// document: the page being versioned is arbitrary agent-authored HTML with
/// its own scripts, and re-running it in a document that already ran another
/// version is a class of bug with no upside. Reload is the honest primitive.
#[allow(clippy::useless_format)]
pub fn version_bar_widget() -> String {
    // `format!` with no arguments, deliberately: the CSS below is full of
    // braces, and `{{`/`}}` escaping is what keeps it readable as CSS.
    format!(
        r#"<style>
#bough-versions {{ position: fixed; z-index: 2147483646; top: 12px; left: 50%;
  transform: translateX(-50%); display: none; align-items: center; gap: 10px;
  padding: 6px 8px 6px 12px; border-radius: 999px; border: 1px solid rgba(0,0,0,.12);
  background: rgba(252,252,251,.94); backdrop-filter: blur(6px);
  box-shadow: 0 2px 12px rgba(0,0,0,.14); color: #0b0b0b;
  font: 12px/1.4 ui-monospace, Menlo, monospace; }}
#bough-versions button {{ border: 0; border-radius: 999px; padding: 3px 9px; cursor: pointer;
  font: inherit; background: rgba(0,0,0,.06); color: inherit; }}
#bough-versions button:disabled {{ opacity: .35; cursor: default; }}
#bough-versions .bough-restore {{ background: #0b0b0b; color: #fcfcfb; }}
#bough-versions .bough-when {{ color: #52514e; }}
@media (prefers-color-scheme: dark) {{
  #bough-versions {{ background: rgba(26,26,25,.94); color: #f4f3ef; border-color: rgba(255,255,255,.14); }}
  #bough-versions button {{ background: rgba(255,255,255,.12); }}
  #bough-versions .bough-restore {{ background: #f4f3ef; color: #1a1a19; }}
  #bough-versions .bough-when {{ color: #c3c2b7; }}
}}
</style>
<div id="bough-versions" role="group" aria-label="artifact versions"></div>
<script>
(function () {{
  var parts = location.pathname.split("/").filter(Boolean);
  if (parts[0] !== "artifacts" || parts.length < 3) return;
  var session = decodeURIComponent(parts[1]);
  var name = parts.slice(2).map(decodeURIComponent).join("/");
  var bar = document.getElementById("bough-versions");
  var shown = new URLSearchParams(location.search).get("v");

  function ago(ts) {{
    var s = Math.max(0, (Date.now() - ts) / 1000);
    if (s < 60) return "just now";
    if (s < 3600) return Math.round(s / 60) + "m ago";
    if (s < 86400) return Math.round(s / 3600) + "h ago";
    return Math.round(s / 86400) + "d ago";
  }}
  function size(n) {{
    return n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(1) + " KB"
      : (n / 1048576).toFixed(1) + " MB";
  }}
  function go(v) {{
    location.href = location.pathname + (v.current ? "" : "?v=" + v.ts);
  }}

  fetch("/sessions/" + encodeURIComponent(session) + "/artifacts/versions?name="
        + encodeURIComponent(name))
    .then(function (r) {{ return r.ok ? r.json() : null; }})
    .then(function (data) {{
      var versions = (data && data.versions) || [];
      // One version is not a history, and a bar that says "1 of 1" is chrome
      // on a page that never changed.
      if (versions.length < 2) return;
      var at = shown
        ? versions.findIndex(function (v) {{ return String(v.ts) === shown; }})
        : versions.length - 1;
      if (at < 0) at = versions.length - 1;
      var v = versions[at];
      bar.style.display = "flex";
      bar.innerHTML = "";

      var back = document.createElement("button");
      back.textContent = "◀";
      back.title = "older version";
      back.disabled = at === 0;
      back.onclick = function () {{ go(versions[at - 1]); }};

      var label = document.createElement("span");
      label.textContent = "v" + v.version + " of " + versions.length;

      var when = document.createElement("span");
      when.className = "bough-when";
      when.textContent = ago(v.ts) + " · " + size(v.bytes);

      var fwd = document.createElement("button");
      fwd.textContent = "▶";
      fwd.title = "newer version";
      fwd.disabled = at === versions.length - 1;
      fwd.onclick = function () {{ go(versions[at + 1]); }};

      bar.appendChild(back);
      bar.appendChild(label);
      bar.appendChild(when);
      bar.appendChild(fwd);

      // Restoring the version already current would be a no-op that adds a
      // history entry, so the button is simply not there for it.
      if (!v.current) {{
        var restore = document.createElement("button");
        restore.className = "bough-restore";
        restore.textContent = "restore";
        restore.title = "publish these bytes again as the newest version";
        restore.onclick = function () {{
          restore.disabled = true;
          fetch("/sessions/" + encodeURIComponent(session) + "/artifacts/restore", {{
            method: "POST",
            headers: {{ "content-type": "application/json" }},
            body: JSON.stringify({{ name: name, ts: v.ts }})
          }}).then(function (r) {{
            if (r.ok) location.href = location.pathname;
            else restore.disabled = false;
          }}, function () {{ restore.disabled = false; }});
        }};
        bar.appendChild(restore);
      }}
    }})
    .catch(function () {{}});
}})();
</script>
"#
    )
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
        // An unknown `?v=` is a 404 rather than a silent fall-through to the
        // current bytes: a link to a version that no longer exists must not
        // quietly show a different document under the same URL.
        let bytes = match opts.version {
            Some(ts) => read_version(session_id, name, ts, &opts.store)?,
            None => std::fs::read(&full).ok()?,
        };
        if content_type.starts_with("text/html") {
            let html = String::from_utf8(bytes).ok()?;
            return Some(respond_no_cache(
                content_type,
                splice_before_body(inject_comment_layer(html), &version_bar_widget()),
            ));
        }
        Some(respond_no_cache(content_type, bytes))
    })();

    match served {
        Some(response) => response,
        None => {
            if opts
                .accept
                .as_deref()
                .is_some_and(|a| a.contains("text/html"))
            {
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

/// `GET /artifacts/:id/:path*` — the hosted file itself, optionally `?v=<ts>`.
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
        let version = raw_query_value(&req, "v").and_then(|v| v.parse::<i64>().ok());
        let id = decode_segments(params.get("id").map(String::as_str).unwrap_or(""));
        let path = decode_segments(params.get("path").map(String::as_str).unwrap_or(""));
        Ok(serve_artifact(
            &id,
            &path,
            &ServeArtifactOptions {
                store: ArtifactStoreOptions::default(),
                accept,
                version,
            },
        ))
    })
}

/// One raw `?key=value`, undecoded — callers decode with the rule that fits
/// what they asked for (a name is path-shaped, a timestamp is an integer).
fn raw_query_value(req: &axum::extract::Request, key: &str) -> Option<String> {
    req.uri().query()?.split('&').find_map(|kv| {
        kv.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .map(String::from)
    })
}

/// `GET /sessions/:id/artifacts/versions?name=<name>` — one artifact's
/// history, oldest first, the live file included as the newest entry.
///
/// Filesystem-backed like the listing, and for the same reason: the history
/// has to survive a database reset, because the bytes do.
pub fn list_artifact_versions() -> Handler {
    handler(|req, _ctx, params| async move {
        let id = decode_segments(params.get("id").map(String::as_str).unwrap_or(""));
        let name = decode_segments(&raw_query_value(&req, "name").unwrap_or_default());
        let versions = store_list_versions(&id, &name, &ArtifactStoreOptions::default());
        Ok(json(&serde_json::json!({ "versions": versions }), 200))
    })
}

/// `POST /sessions/:id/artifacts/restore` `{name, ts}` — bring a version back.
///
/// A restore is a PUBLISH, so this route creates history rather than rewinding
/// it: the response is the artifact as it now stands, and the version that was
/// current a moment ago is still there to restore in turn. No session check —
/// artifacts outlive their row, and so does the ability to fix one.
pub fn restore_artifact_version() -> Handler {
    handler(|req, _ctx, params| async move {
        let id = decode_segments(params.get("id").map(String::as_str).unwrap_or(""));
        let body: RestoreBody = parse_body(req, None).await?;
        let artifact =
            store_restore_version(&id, &body.name, body.ts, &ArtifactStoreOptions::default())?;
        Ok(json(&artifact, 200))
    })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestoreBody {
    name: String,
    ts: i64,
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
    use std::sync::MutexGuard;

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
                root: Some(root.join("live")),
                versions_root: Some(root.join("versions")),
                base_url: Some("http://127.0.0.1:4321".into()),
            },
            accept: None,
            version: None,
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
            // The CRATE-wide lock (`http::testutil::home_lock`), not a
            // module-local one: `BOUGH_HOME` is one variable, so one lock.
            let lock = testutil::home_lock();
            let tmp = TmpDir::new();
            let previous = std::env::var("BOUGH_HOME").ok();
            std::env::set_var("BOUGH_HOME", &tmp.0);
            HomeGuard {
                _lock: lock,
                _tmp: tmp,
                previous,
            }
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
        res.headers()
            .get(name)
            .map(|v| v.to_str().unwrap())
            .unwrap_or("")
    }

    // ---- AC 1: traversal is blocked, at the route, as a 403 -----------------

    #[test]
    fn one_session_cannot_read_anothers_artifacts_through_serve() {
        let tmp = TmpDir::new();
        publish_artifact(
            "victim",
            "secret.html",
            "<b>secret</b>",
            &store(&tmp.0).store,
        )
        .unwrap();
        let res = serve_artifact("attacker", "../victim/secret.html", &store(&tmp.0));
        assert_eq!(res.status(), 403);
    }

    #[tokio::test]
    async fn the_artifact_route_rejects_an_escaping_path_with_403_not_404() {
        // Handlers read the default paths, so the whole test runs under a
        // temp BOUGH_HOME.
        let _home = HomeGuard::new();
        publish_artifact(
            "s1",
            "index.html",
            "<h1>hi</h1>",
            &ArtifactStoreOptions::default(),
        )
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
        let res = call
            .call(testutil::get("/artifacts/..%2Fevil/index.html"))
            .await;
        assert_eq!(res.status(), 403);
        assert_eq!(body_text(res).await, "forbidden");
    }

    // ---- history: versions, ?v=, restore ------------------------------------

    /// The whole round trip a user makes: republish, step back with `?v=`,
    /// restore, and find the version they left still there.
    #[tokio::test]
    async fn versions_list_serve_and_restore_through_the_routes() {
        let _home = HomeGuard::new();
        let opts = ArtifactStoreOptions::default();
        publish_artifact("s1", "report.html", "<body>v1</body>", &opts).unwrap();
        // Distinct mtimes: the ids are timestamps, and a test publishes faster
        // than a millisecond.
        step_mtime("s1", "report.html", 1_000, &opts);
        publish_artifact("s1", "report.html", "<body>v2</body>", &opts).unwrap();
        step_mtime("s1", "report.html", 2_000, &opts);

        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let res = call
            .call(testutil::get(
                "/sessions/s1/artifacts/versions?name=report.html",
            ))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        let versions = body["versions"].as_array().unwrap().clone();
        assert_eq!(versions.len(), 2, "{versions:?}");
        assert_eq!(versions[0]["current"], serde_json::json!(false));
        assert_eq!(versions[1]["current"], serde_json::json!(true));
        let old_ts = versions[0]["ts"].as_i64().unwrap();

        // `?v=` serves the OLD bytes at the SAME url, and still carries both
        // injected layers.
        let served = call
            .call(testutil::get(&format!(
                "/artifacts/s1/report.html?v={old_ts}"
            )))
            .await;
        assert_eq!(served.status(), 200);
        let html = body_text(served).await;
        assert!(html.contains("v1"), "{html}");
        assert!(html.contains("bough-versions"), "the bar rides the page");
        assert!(html.contains("bgh-cmt-toggle"), "so does the comment layer");

        // No `?v=` is still the current one — old links do not change meaning.
        let current = body_text(call.call(testutil::get("/artifacts/s1/report.html")).await).await;
        assert!(current.contains("v2"), "{current}");

        // A version that does not exist is a 404, never a quiet fall-through
        // to different bytes under the same URL.
        let missing = call
            .call(testutil::get("/artifacts/s1/report.html?v=424242"))
            .await;
        assert_eq!(missing.status(), 404);

        // Restore appends, so v2 survives it and can be restored back.
        let restored = call
            .call(testutil::req(
                "POST",
                "/sessions/s1/artifacts/restore",
                Some(serde_json::json!({ "name": "report.html", "ts": old_ts })),
            ))
            .await;
        assert_eq!(restored.status(), 200);
        let now = body_text(call.call(testutil::get("/artifacts/s1/report.html")).await).await;
        assert!(now.contains("v1"), "{now}");

        let after = testutil::body_json(
            call.call(testutil::get(
                "/sessions/s1/artifacts/versions?name=report.html",
            ))
            .await,
        )
        .await;
        assert_eq!(after["versions"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn restoring_a_version_that_does_not_exist_is_a_404() {
        let _home = HomeGuard::new();
        publish_artifact(
            "s1",
            "p.html",
            "<body>only</body>",
            &ArtifactStoreOptions::default(),
        )
        .unwrap();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                "/sessions/s1/artifacts/restore",
                Some(serde_json::json!({ "name": "p.html", "ts": 1 })),
            ))
            .await;
        assert_eq!(res.status(), 404);
    }

    // ---- AC 2: listing survives a database reset ----------------------------

    /// Push a published file's mtime to a known millisecond — the version ids
    /// ARE mtimes, and two publishes in one test share one.
    fn step_mtime(session: &str, name: &str, ts: i64, opts: &ArtifactStoreOptions) {
        let path = resolve_artifact_path(session, name, opts).unwrap();
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ts as u64);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[tokio::test]
    async fn ac_list_artifacts_survives_a_database_reset_no_row_required() {
        let _home = HomeGuard::new();
        let opts = ArtifactStoreOptions::default();
        publish_artifact("ghost", "index.html", "<h1>still here</h1>", &opts).unwrap();
        publish_artifact("ghost", "assets/app.js", "console.log(1)", &opts).unwrap();

        // A brand-new, empty database: nothing knows this session ever existed.
        let fx = testutil::fixture();
        assert!(fx
            .ctx
            .db
            .lock()
            .unwrap()
            .get_session("ghost")
            .unwrap()
            .is_none());
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

        let served = call
            .call(testutil::get("/artifacts/ghost/index.html"))
            .await;
        assert_eq!(served.status(), 200);
        assert!(body_text(served).await.contains("still here"));
    }

    // ---- serving ------------------------------------------------------------

    #[tokio::test]
    async fn serve_artifact_sets_the_content_type_and_never_caches() {
        let tmp = TmpDir::new();
        let opts = store(&tmp.0);
        publish_artifact(
            "s3",
            "page.html",
            "<!doctype html><title>x</title>",
            &opts.store,
        )
        .unwrap();
        publish_artifact("s3", "app.js", "console.log(1)", &opts.store).unwrap();
        publish_artifact("s3", "data.csv", "a,b", &opts.store).unwrap();

        let html = serve_artifact("s3", "page.html", &opts);
        assert_eq!(html.status(), 200);
        assert_eq!(header(&html, "content-type"), "text/html; charset=utf-8");
        assert_eq!(header(&html, "cache-control"), "no-cache");
        assert!(body_text(html).await.contains("<title>x</title>"));

        let js = serve_artifact("s3", "app.js", &opts);
        assert_eq!(
            header(&js, "content-type"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(body_text(js).await, "console.log(1)"); // untouched: no layer in a script

        let csv = serve_artifact("s3", "data.csv", &opts);
        assert_eq!(header(&csv, "content-type"), "text/csv; charset=utf-8");
    }

    #[tokio::test]
    async fn serve_artifact_sniffs_an_extensionless_html_file_so_it_renders() {
        let tmp = TmpDir::new();
        let opts = store(&tmp.0);
        publish_artifact(
            "s7",
            "my-explorer",
            "<!doctype html>\n<title>x</title>",
            &opts.store,
        )
        .unwrap();
        publish_artifact("s7", "notes", "just text", &opts.store).unwrap();

        let html = serve_artifact("s7", "my-explorer", &opts);
        assert_eq!(header(&html, "content-type"), "text/html; charset=utf-8");
        // …and a sniffed HTML document gets the comment layer, exactly like a
        // named `.html` one.
        assert!(body_text(html).await.contains("bgh-cmt-toggle"));

        let plain = serve_artifact("s7", "notes", &opts);
        assert_eq!(header(&plain, "content-type"), "application/octet-stream");
    }

    #[tokio::test]
    async fn a_missing_artifact_is_json_for_a_client_and_a_page_for_a_browser() {
        let tmp = TmpDir::new();
        let api = serve_artifact("s5", "nope.html", &store(&tmp.0));
        assert_eq!(api.status(), 404);
        assert_eq!(
            header(&api, "content-type"),
            "application/json; charset=utf-8"
        );
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

    // ---- the comment layer (row 3.14) ---------------------------------------

    #[tokio::test]
    async fn every_served_html_document_carries_the_comment_layer_and_nothing_else_does() {
        let tmp = TmpDir::new();
        let opts = store(&tmp.0);
        publish_artifact(
            "s9",
            "page.html",
            "<!doctype html><html><body><h1>hi</h1></body></html>",
            &opts.store,
        )
        .unwrap();
        publish_artifact("s9", "app.js", "console.log(1)", &opts.store).unwrap();
        publish_artifact("s9", "style.css", "body{}", &opts.store).unwrap();

        let html = body_text(serve_artifact("s9", "page.html", &opts)).await;
        assert!(
            html.contains("bgh-cmt-toggle"),
            "the layer must be in a served HTML document"
        );
        // Spliced BEFORE `</body>`, so the page's own markup still closes.
        let widget_at = html.find("bgh-cmt-toggle").unwrap();
        assert!(widget_at < html.rfind("</body>").unwrap());
        assert!(html.starts_with("<!doctype html><html><body><h1>hi</h1>"));
        assert!(html.trim_end().ends_with("</body></html>"));

        // Injecting into the page's own CSS or JS — served through the same
        // route — would corrupt them, and the layer would not work anyway.
        assert_eq!(
            body_text(serve_artifact("s9", "app.js", &opts)).await,
            "console.log(1)"
        );
        assert_eq!(
            body_text(serve_artifact("s9", "style.css", &opts)).await,
            "body{}"
        );

        // The bytes on DISK are untouched: what the agent wrote is what a user
        // who saves or forwards the file gets.
        assert_eq!(
            std::fs::read_to_string(tmp.0.join("live").join("s9").join("page.html")).unwrap(),
            "<!doctype html><html><body><h1>hi</h1></body></html>"
        );
    }

    #[test]
    fn a_fragment_with_no_body_tag_still_gets_the_layer_appended() {
        let out = inject_comment_layer("<h1>just a fragment</h1>".to_string());
        assert!(out.starts_with("<h1>just a fragment</h1>"));
        assert!(out.contains("bgh-cmt-toggle"));
        // An uppercase closing tag is the same closing tag.
        let cased = inject_comment_layer("<BODY>x</BODY>".to_string());
        assert!(cased.find("bgh-cmt-toggle").unwrap() < cased.rfind("</BODY>").unwrap());
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
