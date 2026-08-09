//! The charting libraries every artifact may link, served from
//! `/artifacts/_lib/<file>`.
//!
//! WHY A SERVED ROUTE AND NOT INLINED BYTES. The artifact bar is
//! "self-contained, renders offline, no CDN", and the obvious reading of that
//! is "inline everything". At 2MB of chart engine, inlining is not a bar, it is
//! a ban: the agent hand-rolls SVG instead, badly, and regenerates all of it on
//! every edit. Serving the engines from the same loopback origin keeps every
//! property the bar was protecting — no third party, no network, works with the
//! machine offline — while making a real chart a five-line script tag.
//!
//! THE BYTES ARE COMMITTED, NOT FETCHED. `scripts/build-chart-bundle.sh`
//! produces `js/lib/` and a human commits the result; `include_dir!` welds it
//! into the binary at compile time. So `cargo build` on a clean checkout needs
//! no node, no npm, and no network, and a released binary cannot be missing its
//! chart engine. Bumping a version is a reviewable diff, which is the point.
//!
//! `_lib` CANNOT COLLIDE WITH A SESSION. Session ids are uuids, so no session
//! ever owns this path; the route is registered ahead of
//! `/artifacts/:id/:path*` and shadows nothing real.
//!
//! Same trust posture as the artifacts themselves: this is agent-facing output
//! infrastructure on loopback, not a containment boundary.

use axum::body::Body;
use axum::response::Response;

use crate::http::{handler, Handler};

/// The committed bundle. `include_dir!` (not `include_str!`) because it is
/// several files and one of them is a build manifest — and because a missing
/// directory then fails the BUILD rather than 404-ing at runtime.
static LIB: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/js/lib");

/// What `/artifacts/_lib/` will serve, for the error message and for tests.
pub fn lib_files() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = LIB
        .files()
        .filter_map(|f| f.path().file_name().and_then(|n| n.to_str()))
        .collect();
    names.sort_unstable();
    names
}

/// Look up one bundled file. Flat by construction — a name with a separator in
/// it never matches, so there is no traversal to defend against.
fn lib_bytes(name: &str) -> Option<&'static [u8]> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }
    LIB.get_file(name).map(|f| f.contents())
}

/// `GET /artifacts/_lib/:file` — a vendored chart engine.
///
/// Cached, unlike artifacts. An artifact is overwritten in place and a stale
/// copy is indistinguishable from an agent that did nothing, so it is
/// `no-cache`; these bytes instead change only when the binary does, and
/// re-sending a megabyte of echarts on every chart page reload is pure waste.
/// An hour is short enough that an upgrade mid-session is not confusing.
pub fn get_lib_file() -> Handler {
    handler(|_req, _ctx, params| async move {
        let file =
            crate::artifacts::decode_segments(params.get("file").map(String::as_str).unwrap_or(""));
        let Some(bytes) = lib_bytes(&file) else {
            let available = lib_files().join(", ");
            return Ok(Response::builder()
                .status(404)
                .header("content-type", "application/json; charset=utf-8")
                .body(Body::from(
                    serde_json::json!({
                        "error": format!("no bundled library {file} — have: {available}"),
                    })
                    .to_string(),
                ))
                .expect("static response parts"));
        };
        Ok(Response::builder()
            .status(200)
            .header("content-type", crate::artifacts::content_type_for(&file))
            .header("cache-control", "public, max-age=3600")
            .body(Body::from(bytes))
            .expect("static response parts"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_ships_the_engines_an_artifact_is_told_to_import() {
        let files = lib_files();
        for expected in [
            "flint.js",
            "echarts.js",
            "echarts-gl.js",
            "VERSIONS",
            "LICENSES",
        ] {
            assert!(files.contains(&expected), "missing {expected} in {files:?}");
        }
    }

    /// Vendoring third-party bytes into an Apache-2.0 repo carries their terms
    /// with it, and minification strips the notices out of the JS itself. The
    /// build script regenerates LICENSES from the installed packages; if that
    /// step ever silently no-ops, this is what says so.
    #[test]
    fn every_vendored_package_has_its_licence_text_shipped() {
        let licences = String::from_utf8_lossy(lib_bytes("LICENSES").expect("LICENSES"));
        for pkg in ["flint-chart", "echarts", "echarts-gl", "claygl", "zrender"] {
            assert!(
                licences.contains(&format!("== {pkg} ")),
                "LICENSES has no section for {pkg} — rerun scripts/build-chart-bundle.sh"
            );
        }
    }

    /// The prompt tells the agent to `import { assembleECharts } from
    /// '/artifacts/_lib/flint.js'`. If the bundle stopped exporting it, every
    /// chart artifact would break in the browser and nothing in Rust would
    /// notice — so pin the name here.
    #[test]
    fn flint_bundle_exports_the_compiler_entry_point() {
        let js = std::str::from_utf8(lib_bytes("flint.js").expect("flint.js")).expect("utf-8");
        assert!(
            js.contains("assembleECharts"),
            "flint.js no longer mentions assembleECharts — rerun scripts/build-chart-bundle.sh"
        );
    }

    #[test]
    fn a_name_with_a_separator_is_simply_not_found() {
        assert!(lib_bytes("../../Cargo.toml").is_none());
        assert!(lib_bytes("").is_none());
    }

    /// The bar the whole feature exists to keep: these are served so that an
    /// artifact never reaches a CDN. A bundle that itself phones home would
    /// defeat that silently.
    #[test]
    fn the_vendored_engines_reference_no_cdn() {
        for name in ["flint.js", "echarts.js", "echarts-gl.js"] {
            let js = String::from_utf8_lossy(lib_bytes(name).expect(name));
            for banned in ["https://cdn.", "https://unpkg", "https://jsdelivr"] {
                assert!(!js.contains(banned), "{name} reaches out to {banned}");
            }
        }
    }
}
