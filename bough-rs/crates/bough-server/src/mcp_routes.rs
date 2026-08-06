//! The MCP registry/grants/connections routes + OAuth (port of the route
//! surface over `src/mcp/status.ts` and `src/mcp/oauth.ts`, wave-1 stub).
//!
//! v1-STUB per server.md §8: `GET /mcp/servers` answers the empty four-key
//! document — `{registry, auth, active, connections}` is what
//! `prompt/mcp-status.md` promises the model, so the KEYS are fixed even
//! when everything in them is empty (this build loads no registry file yet).
//! Every mutation and the OAuth flow answer 400 "not yet ported". The real
//! subsystem lands in wave 3 (rows 3.1–3.4).

use serde_json::json;

use bough_core::errors::BoughError;

use crate::http::{handler, json as json_res, Handler};

/// Where the authorization server sends the user's browser back. Verbatim TS
/// `CALLBACK_PATH` — baked into registered redirect URIs, so the route must
/// exist on bough's own port even before the flow works.
pub const CALLBACK_PATH: &str = "/mcp/oauth/callback";

fn not_yet() -> BoughError {
    BoughError::bad_request("MCP is not yet ported in this build")
}

/// `GET /mcp/servers` — the whole state. Stub: the empty document, keys fixed.
pub fn get_mcp_servers() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Ok(json_res(
            &json!({
                "registry": { "servers": {} },
                "auth": {},
                "active": [],
                "connections": [],
            }),
            200,
        ))
    })
}

/// Every MCP mutation and OAuth verb: 400 "not yet ported".
pub fn mcp_not_yet() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Err::<axum::response::Response, _>(not_yet())
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use serde_json::json as j;

    #[tokio::test]
    async fn the_mcp_document_keeps_its_four_keys_even_when_empty() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/mcp/servers")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"registry": {"servers": {}}, "auth": {}, "active": [], "connections": []})
        );
    }

    #[tokio::test]
    async fn mutations_and_oauth_are_a_400_not_yet_ported() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        for (method, path) in [
            ("PUT", "/mcp/servers"),
            ("PUT", "/mcp/servers/gh"),
            ("DELETE", "/mcp/servers/gh"),
            ("POST", "/mcp/servers/gh/connect"),
            ("POST", "/mcp/servers/gh/tools/list_prs"),
            ("POST", "/mcp/servers/gh/restart"),
            ("POST", "/mcp/servers/gh/enable"),
            ("POST", "/mcp/servers/gh/disable"),
            ("GET", "/mcp/servers/gh/auth"),
            ("POST", "/mcp/servers/gh/auth"),
            ("DELETE", "/mcp/servers/gh/auth"),
            ("GET", super::CALLBACK_PATH),
        ] {
            let res = call.call(testutil::req(method, path, None)).await;
            assert_eq!(res.status(), 400, "{method} {path}");
            let body = testutil::body_json(res).await;
            assert_eq!(body["error"], "MCP is not yet ported in this build", "{method} {path}");
        }
    }
}
