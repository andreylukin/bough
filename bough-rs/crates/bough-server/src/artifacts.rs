//! Artifact listing + hosting (port of `src/server/artifacts.ts`, wave-1
//! stub).
//!
//! v1-STUB per server.md §8: the `artifact()` host fn has not landed, so no
//! session has written anything under `~/.bough/artifacts/` through this
//! build — the listing's honest answer is empty, and every hosted-file path
//! is a 404. The listing keeps the TS contract's one surprise: it is
//! filesystem-backed and deliberately does NOT require a session row
//! (artifacts outlive database resets), so there is no 404 on the listing.
//!
//! The full port (directory walk, HTML sniffing, comment-widget injection,
//! per-segment percent-decoding, the 403 traversal answer) lands with the
//! artifact subsystem. Nothing is served from disk until then, so the
//! traversal surface does not exist yet either.

use serde_json::json;

use bough_core::errors::BoughError;

use crate::http::{handler, json as json_res, Handler};

/// `GET /sessions/:id/artifacts` — `{artifacts: []}`, no session-row check.
pub fn list_artifacts() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Ok(json_res(&json!({ "artifacts": [] }), 200))
    })
}

/// `GET /artifacts/:id/:path*` — the hosted file. Stub: nothing is hosted.
pub fn get_artifact() -> Handler {
    handler(|_req, _ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        let path = params.get("path").cloned().unwrap_or_default();
        Err::<axum::response::Response, _>(BoughError::not_found(format!(
            "no artifact {id}/{path} — artifact hosting is not yet ported in this build",
        )))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use serde_json::json as j;

    #[tokio::test]
    async fn the_listing_is_200_and_empty_even_for_a_session_with_no_row() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/sessions/never-stored/artifacts")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"artifacts": []}));
    }

    #[tokio::test]
    async fn a_hosted_file_path_is_a_404() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/artifacts/s1/report/index.html")).await;
        assert_eq!(res.status(), 404);
    }
}
