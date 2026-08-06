//! `GET /skills`, `GET /skills/:name` — what is installed (port of
//! `src/server/skills.ts`).
//!
//! v1-STUB per server.md §8: `{skills: [], sources: []}` — the honest answer
//! of an install with nothing discovered (the SKILL.md walk lands with the
//! skills subsystem, wave 2). The 404 keeps the TS shape: it explains what a
//! skill IS and that nothing is installed, because "why is my skill not
//! listed" is the question behind every miss.

use serde_json::json;

use bough_core::errors::BoughError;

use crate::http::{handler, json as json_res, Handler};

/// `GET /skills` — every installed skill. Stub: none discovered.
pub fn list_skills() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Ok(json_res(&json!({ "skills": [], "sources": [] }), 200))
    })
}

/// `GET /skills/:name` — one skill, body included. Stub: nothing installed,
/// so every name is a 404 that says so.
pub fn get_skill() -> Handler {
    handler(|_req, _ctx, params| async move {
        let name = params.get("name").cloned().unwrap_or_default();
        Err::<axum::response::Response, _>(BoughError::not_found(format!(
            "no skill \"{name}\". A skill is a folder <dir>/{name}/SKILL.md in one of the \
             skill source directories. Nothing is installed.",
        )))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use serde_json::json as j;

    #[tokio::test]
    async fn the_skills_listing_is_200_with_empty_skills_and_sources() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/skills")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"skills": [], "sources": []}));
    }

    #[tokio::test]
    async fn an_unknown_skill_is_a_404_naming_it() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/skills/wayfinder")).await;
        assert_eq!(res.status(), 404);
        let body = testutil::body_json(res).await;
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("no skill \"wayfinder\""), "{msg}");
        assert!(msg.contains("SKILL.md"), "{msg}");
    }
}
