//! `GET /theme` — the TUI's boot palette (port of `src/server/theme.ts`,
//! wave-1 subset).
//!
//! v1-STUB per PORT_PLAN row 1.31: the wave-1 carried stub is "theme =
//! FALLBACK palette (GET /theme serves `{theme:null, defaults}` statically)".
//! "No theme is set" is an ANSWER — it is the default palette — so this is a
//! legitimate state of the TS system, always 200. `PUT`/`DELETE /theme` land
//! with the theming port (wave 3); until then the routes simply do not exist
//! and the dispatcher's 405/404 semantics answer for them.
//!
//! `THEME_DEFAULTS` is ported verbatim — the 18-token semantic set and these
//! particular hexes are product surface (contrast-tuned; `tui/theme.ts`'s
//! FALLBACK mirrors the subset the TUI consumes and the two must not drift).

use serde_json::json;

use crate::http::{handler, json as json_res, Handler};

/// The built-in palette — the floor every partial theme falls through to.
/// Verbatim from TS `THEME_DEFAULTS` (18 tokens, fixed set).
pub fn theme_defaults() -> serde_json::Value {
    json!({
        "bg": "#0e1013",
        "panel": "#14161a",
        "panel2": "#161a1f",
        "panel3": "#191c21",
        "panelInset": "#1f2329",
        "canvas": "#111318",
        "border": "#5a616c",
        "border2": "#484e57",
        "border3": "#3c4149",
        "hairline": "#666d79",
        "text": "#e7e9ed",
        "text2": "#c9cdd4",
        "muted": "#9aa1ac",
        "muted2": "#7a828e",
        "green": "#4ec98f",
        "amber": "#d9b45f",
        "red": "#e2776e",
        "blue": "#5c88c9",
    })
}

/// `GET /theme` — `{theme, defaults}`, always 200. Stub: no theme is ever
/// stored, so `theme` is null and the defaults carry the whole palette.
pub fn get_theme() -> Handler {
    handler(|_req, _ctx, _params| async move {
        Ok(json_res(&json!({ "theme": null, "defaults": theme_defaults() }), 200))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;

    #[tokio::test]
    async fn get_theme_is_always_200_with_null_theme_and_the_full_default_palette() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/theme")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert!(body["theme"].is_null());
        let defaults = body["defaults"].as_object().unwrap();
        assert_eq!(defaults.len(), 18, "the fixed 18-token set");
        assert_eq!(defaults["bg"], "#0e1013");
        assert_eq!(defaults["muted2"], "#7a828e");
        assert_eq!(defaults["blue"], "#5c88c9");
    }
}
