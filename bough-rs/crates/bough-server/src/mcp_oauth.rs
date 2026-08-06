//! The OAuth verbs and the callback bough hosts (routes over `bough_core::mcp::oauth`,
//! row 3.5).
//!
//! The domain logic — the token store, the provider, the flow, and every sentence a
//! failure says — lives in `bough_core::mcp::oauth`. This file is the four route
//! entries and nothing else, so the module that a browser lands on is reachable from
//! the table rather than merely written.
//!
//! The callback answers HTML in EVERY outcome, including its failures. Its audience
//! is a human in a browser tab: they cannot act on `{"error": …}` and they will not
//! see a status code, so the dispatcher's JSON envelope would be the wrong answer.
//! Every other verb throws `BoughError` and is rendered by the one catch, exactly
//! like every other handler.

use axum::body::Body;
use axum::response::Response;

use bough_core::mcp::oauth::{
    auth_status_route, begin_auth_route, clear_auth_route, oauth_callback_page, AuthFlowOptions,
    CompleteAuthOptions, RegistryAccess,
};

use crate::http::{handler, json, Handler};

/// Where the authorization server sends the user's browser back. Verbatim TS
/// `CALLBACK_PATH` — baked into registered redirect URIs, so the route must exist on
/// bough's own port.
pub const CALLBACK_PATH: &str = bough_core::mcp::oauth::CALLBACK_PATH;

/// `GET /mcp/servers/:name/auth` — is this server authorized, and where does the flow
/// return?
pub fn auth_status() -> Handler {
    handler(|_req, _ctx, params| async move {
        let name = params.get("name").cloned().unwrap_or_default();
        Ok(json(
            &auth_status_route(&name, &RegistryAccess::default())?,
            200,
        ))
    })
}

/// `POST /mcp/servers/:name/auth` — start the flow. This is what the mcp panel's `a`
/// calls. It returns the URL; it never opens a browser and never blocks waiting for
/// one, so a headless install behaves the same as a desktop one.
pub fn begin_auth() -> Handler {
    handler(|_req, _ctx, params| async move {
        let name = params.get("name").cloned().unwrap_or_default();
        Ok(json(
            &begin_auth_route(&name, &AuthFlowOptions::default()).await?,
            200,
        ))
    })
}

/// `DELETE /mcp/servers/:name/auth` — forget the tokens ("logout").
pub fn clear_auth() -> Handler {
    handler(|_req, _ctx, params| async move {
        let name = params.get("name").cloned().unwrap_or_default();
        Ok(json(&clear_auth_route(&name)?, 200))
    })
}

/// `GET /mcp/oauth/callback` — where the user's browser lands.
pub fn oauth_callback() -> Handler {
    handler(|req: axum::extract::Request, _ctx, _params| async move {
        let query = req.uri().query().unwrap_or("").to_string();
        let (status, html) = oauth_callback_page(&query, &CompleteAuthOptions::default()).await;
        Ok(Response::builder()
            .status(status)
            .header("content-type", "text/html; charset=utf-8")
            .body(Body::from(html))
            .expect("static response parts"))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;

    /// The browser must land on a REAL route, and it must get a page.
    #[tokio::test]
    async fn the_callback_answers_html_at_the_path_the_redirect_uri_names() {
        assert_eq!(super::CALLBACK_PATH, "/mcp/oauth/callback");
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());

        let res = call.call(testutil::get("/mcp/oauth/callback")).await;
        assert_eq!(res.status(), 400);
        assert!(
            res.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .contains("text/html"),
            "the audience is a human in a browser tab"
        );
        let body = testutil::body_text(res).await;
        assert!(body.contains("not a bough callback"), "{body}");

        let res = call
            .call(testutil::get("/mcp/oauth/callback?error=access_denied"))
            .await;
        assert_eq!(res.status(), 400);
        let body = testutil::body_text(res).await;
        assert!(
            body.contains("declined") && body.contains("access_denied"),
            "{body}"
        );
    }

    /// A stdio entry has no OAuth, and an unregistered name is a 404 — both through
    /// the real table, so a handler that is written but unwired fails here.
    #[tokio::test]
    async fn the_auth_verbs_answer_from_the_route_table() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/mcp/servers/nope/auth")).await;
        assert_eq!(
            res.status(),
            404,
            "an unregistered server is a 404, not 'not ported'"
        );
        let body = testutil::body_json(res).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap_or("")
                .contains("PUT /mcp/servers/nope"),
            "{body}"
        );

        // DELETE never needs a registry entry: forgetting nothing is still an answer.
        let res = call
            .call(testutil::req("DELETE", "/mcp/servers/nope/auth", None))
            .await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert_eq!(body["server"], "nope");
        assert_eq!(body["cleared"], false);
    }
}
