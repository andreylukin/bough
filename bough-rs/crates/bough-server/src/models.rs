//! `GET /models` — the picker's catalog (port of `src/server/models.ts`,
//! wave-1 subset).
//!
//! v1 per PORT_PLAN row 1.31 + ARCHITECTURE §8: **static table only, no
//! discovery** — the static `MODELS` table is the designed fallback the TS
//! server itself answers with when discovery finds no key or times out, so
//! serving it alone is a legitimate TS answer, not a lie. The discovery race
//! (2.5s deadline, single flight, TTL cache) lands with the openai_compat
//! port (wave 2).

use serde_json::json;

use bough_core::llm::routing::MODELS;

use crate::http::{handler, json as json_res, Handler};

/// `GET /models` — `{models: [ModelRow]}`, the static table.
pub fn get_models() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Ok(json_res(&json!({ "models": &*MODELS }), 200))
    })
}

#[cfg(test)]
mod tests {
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;

    #[tokio::test]
    async fn get_models_serves_the_static_table_with_ts_field_names() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/models")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        let models = body["models"].as_array().unwrap();
        assert!(!models.is_empty());
        assert_eq!(models[0]["id"], "claude-opus-4-8");
        assert_eq!(models[0]["label"], "Opus 4.8");
        assert_eq!(models[0]["provider"], "anthropic");
        // The Workers AI rows survive with their `@cf/` ids intact.
        assert!(models.iter().any(|m| m["id"] == "@cf/zai-org/glm-5.2"));
    }
}
