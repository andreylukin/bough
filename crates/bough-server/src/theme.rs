//! Theming: a NAMED PARTIAL palette over a fixed semantic token set,
//! persisted at `~/.bough/theme.json` and served over `GET`/`PUT`/`DELETE
//! /theme` (port of `src/server/theme.ts`).
//!
//! THE INVARIANT THIS HOLDS: **a theme is pure data, and the SERVER owns the
//! token set.** A theme never becomes code — the TUI fetches this document at
//! boot and paints the tokens it consumes as truecolor, which is what lets
//! the picker preview a palette live on cursor move. And the token set lives
//! here rather than in the frozen wire schema, because the thing a theme
//! author needs to be told is *which* token they misspelled and what the real
//! ones are — `validate_theme` below does exactly that, and it is the only
//! gate.
//!
//! WHY THE THEME IS PARTIAL, AND WHY THE DEFAULTS ARE SERVED ALONGSIDE IT:
//! `GET /theme` answers `{theme, defaults}` rather than one merged map — a
//! client that only ever saw the merge could not tell a token the user
//! *chose* from one it inherited, and "reset this token" would be
//! indistinguishable from "set it to the value it already has".
//!
//! A CORRUPT FILE IS THE DEFAULT PALETTE, NOT AN ERROR. `load_theme` answers
//! `None` for anything it cannot parse or validate: a hand-edited
//! `theme.json` with a trailing comma must not take the whole UI's colour
//! down with it. Writing is where validation bites; reading is where it
//! forgives.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use bough_core::errors::BoughError;
use bough_core::paths::theme_path;

use crate::http::{handler, json as json_res, parse_body, Handler};

// ---------------------------------------------------------------------------
// The token contract
// ---------------------------------------------------------------------------

/// The semantic tokens a theme may set. A FIXED contract, and deliberately
/// wider than what the TUI reads today: this is what `PUT /theme` validates
/// against and what a theme author is shown when they miss. Adding a token is
/// a compatible change (old themes simply do not set it); removing one is not.
pub const THEME_TOKENS: [&str; 18] = [
    "bg",
    "panel",
    "panel2",
    "panel3",
    "panelInset",
    "canvas",
    "border",
    "border2",
    "border3",
    "hairline",
    "text",
    "text2",
    "muted",
    "muted2",
    "green",
    "amber",
    "red",
    "blue",
];

/// The built-in palette — the floor every partial theme falls through to.
/// Verbatim from TS `THEME_DEFAULTS`; the contrast rationale is why these
/// particular hexes: borders sit at least 3:1 against `bg` (`hairline` higher
/// still), `muted2` is TEXT rather than decoration and clears WCAG AA at
/// 4.91:1. `tui/theme`'s FALLBACK mirrors the subset the TUI consumes — the
/// two must not drift, and this one wins whenever the server is up.
pub fn theme_defaults() -> Value {
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

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`. The TUI renders truecolor;
/// nothing else.
fn is_hex(v: &str) -> bool {
    let Some(digits) = v.strip_prefix('#') else {
        return false;
    };
    matches!(digits.len(), 3 | 4 | 6 | 8) && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A theme: a display name and the tokens it overrides. Everything else
/// inherits. Serialized as `{name, colors}` — colors in token-table order so
/// the stored file reads like the contract.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: String,
    pub colors: HashMap<String, String>,
}

impl Theme {
    /// The wire/stored JSON, colors in `THEME_TOKENS` order.
    pub fn to_value(&self) -> Value {
        let mut colors = Map::new();
        for token in THEME_TOKENS {
            if let Some(v) = self.colors.get(token) {
                colors.insert(token.to_string(), json!(v));
            }
        }
        json!({ "name": self.name, "colors": colors })
    }
}

/// `PUT /theme` body. `colors` is deliberately an open map — the theme module
/// owns validation, so an unknown token gets a message that NAMES it rather
/// than a generic shape error.
#[derive(Deserialize)]
pub struct PutThemeBody {
    pub name: String,
    pub colors: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Validation (pure)
// ---------------------------------------------------------------------------

/// Validate a candidate theme, NAMING what is wrong.
///
/// Unknown tokens are collected rather than reported one at a time — a
/// hand-written palette usually misspells a family of them at once, and three
/// round-trips to learn three names is three times the work.
pub fn validate_theme(name: &str, colors: &HashMap<String, String>) -> Result<Theme, BoughError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(BoughError::bad_request("theme name is required"));
    }
    if name.chars().count() > 80 {
        return Err(BoughError::bad_request("theme name is over 80 characters"));
    }

    let mut unknown: Vec<&str> = colors
        .keys()
        .map(String::as_str)
        .filter(|k| !THEME_TOKENS.contains(k))
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        return Err(BoughError::bad_request(format!(
            "unknown theme token(s): {} — the token set is fixed: {}",
            unknown.join(", "),
            THEME_TOKENS.join(", ")
        )));
    }

    let mut bad: Vec<(&str, &str)> = colors
        .iter()
        .filter(|(_, v)| !is_hex(v))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    bad.sort_unstable();
    if !bad.is_empty() {
        return Err(BoughError::bad_request(format!(
            "colors must be hex (#rgb, #rrggbb or #rrggbbaa): {}",
            bad.iter()
                .map(|(k, v)| format!("{k}={}", serde_json::to_string(v).unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    // Rebuilt rather than passed through, so what is persisted contains
    // exactly the validated keys and nothing a looser parse let ride along.
    Ok(Theme {
        name: name.to_string(),
        colors: colors.clone(),
    })
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// The stored theme, or `None` when none is set — which is the ordinary
/// state, not a failure. A file that cannot be read, parsed or validated is
/// also `None`. `path` is passed by tests so nothing here touches a real
/// `~/.bough`.
pub fn load_theme(path: &Path) -> Option<Theme> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let name = value.get("name")?.as_str()?;
    let table = value.get("colors").and_then(Value::as_object);
    let mut clean = HashMap::new();
    for (k, v) in table.into_iter().flatten() {
        // Forgiving on READ: an unknown token or a bad hex in a hand-edited
        // file is dropped rather than discarding the whole palette with it.
        if let Some(v) = v.as_str() {
            if THEME_TOKENS.contains(&k.as_str()) && is_hex(v) {
                clean.insert(k.clone(), v.to_string());
            }
        }
    }
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(Theme {
        name: trimmed.to_string(),
        colors: clean,
    })
}

/// Persist a validated theme. Creates the data root if this is the first
/// write.
pub fn save_theme(theme: &Theme, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&theme.to_value()).unwrap_or_default();
    std::fs::write(path, text + "\n")
}

/// Remove the stored theme; the palette falls back to the defaults.
/// Removing a theme that was never set is a success.
pub fn clear_theme(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The served document for whatever is (or is not) stored.
pub fn theme_state(path: &Path) -> Value {
    json!({
        "theme": load_theme(path).map(|t| t.to_value()),
        "defaults": theme_defaults(),
    })
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

/// `GET /theme` — `{theme, defaults}`. Always 200: "no theme is set" is an
/// ANSWER — it is the default palette — and a 404 would make every client
/// branch on a condition that is the normal case.
pub fn get_theme() -> Handler {
    handler(|_req, _ctx, _params| async move { Ok(json_res(&theme_state(&theme_path()), 200)) })
}

/// `PUT /theme` — adopt a named partial palette. 200 with the new state; a
/// failed validation is a 400 and does not overwrite what is stored.
pub fn put_theme() -> Handler {
    handler(|req, _ctx, _params| async move {
        let body: PutThemeBody = parse_body(req, None).await?;
        let theme = validate_theme(&body.name, &body.colors)?;
        save_theme(&theme, &theme_path())
            .map_err(|e| BoughError::bad_request(format!("could not save theme: {e}")))?;
        Ok(json_res(
            &json!({ "theme": theme.to_value(), "defaults": theme_defaults() }),
            200,
        ))
    })
}

/// `DELETE /theme` — back to the built-in palette. Idempotent: deleting a
/// theme that was never set is a success, because the state the caller asked
/// for is the state they get.
pub fn delete_theme() -> Handler {
    handler(|_req, _ctx, _params| async move {
        clear_theme(&theme_path());
        Ok(json_res(
            &json!({ "theme": null, "defaults": theme_defaults() }),
            200,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use serde_json::json as j;
    use std::path::PathBuf;
    use std::sync::MutexGuard;

    fn colors(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ---- validation ---------------------------------------------------------

    #[test]
    fn an_unknown_token_is_rejected_with_a_message_naming_it_and_the_real_tokens() {
        let err = validate_theme(
            "Typo",
            &colors(&[("accent", "#ffffff"), ("forground", "#000000")]),
        )
        .unwrap_err();
        assert_eq!(err.status(), 400);
        let msg = err.to_string();
        // Both offenders in one answer: a palette usually misspells a family
        // of tokens at once.
        assert!(msg.contains("accent"), "{msg}");
        assert!(msg.contains("forground"), "{msg}");
        // …and the real set, so the fix does not need a docs lookup.
        assert!(msg.contains("green"), "{msg}");
        assert!(msg.contains("panelInset"), "{msg}");
    }

    #[test]
    fn a_non_hex_colour_is_rejected_naming_the_token_and_the_value() {
        let err = validate_theme("Bad", &colors(&[("green", "rebeccapurple")])).unwrap_err();
        assert_eq!(err.status(), 400);
        let msg = err.to_string();
        assert!(msg.contains("green"), "{msg}");
        assert!(msg.contains("rebeccapurple"), "{msg}");
    }

    #[test]
    fn every_hex_length_the_tui_can_paint_is_accepted_and_the_name_is_trimmed() {
        let theme = validate_theme(
            "  Spaced  ",
            &colors(&[
                ("green", "#abc"),
                ("amber", "#abcd"),
                ("red", "#aabbcc"),
                ("blue", "#aabbccdd"),
            ]),
        )
        .unwrap();
        assert_eq!(theme.name, "Spaced");
        let mut keys: Vec<&str> = theme.colors.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["amber", "blue", "green", "red"]);
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let err = validate_theme("   ", &HashMap::new()).unwrap_err();
        assert_eq!(err.status(), 400);
        assert_eq!(err.to_string(), "theme name is required");
    }

    #[test]
    fn theme_defaults_covers_every_token_and_every_default_is_hex() {
        // The defaults are the floor a partial theme falls through to; a
        // missing one would paint a token as terminal-default grey with
        // nothing to notice it by.
        let defaults = theme_defaults();
        let table = defaults.as_object().unwrap();
        for token in THEME_TOKENS {
            let value = table
                .get(token)
                .unwrap_or_else(|| panic!("{token} has no default"));
            assert!(
                is_hex(value.as_str().unwrap()),
                "{token} default is not hex"
            );
        }
        assert_eq!(table.len(), THEME_TOKENS.len());
    }

    // ---- persistence --------------------------------------------------------

    fn temp_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("bough-theme-{}", uuid::Uuid::new_v4()))
            .join("theme.json")
    }

    #[test]
    fn a_corrupt_theme_file_reads_as_no_theme_not_an_error() {
        let path = temp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json,").unwrap();
        assert_eq!(load_theme(&path), None);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_hand_edited_file_keeps_its_valid_tokens_and_drops_the_rest() {
        let path = temp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            j!({ "name": "Hand", "colors": { "green": "#123456", "nope": "#111111", "red": "zzz" } })
                .to_string(),
        )
        .unwrap();
        // Forgiving on READ, strict on WRITE: the palette survives, the junk
        // does not.
        assert_eq!(
            load_theme(&path),
            Some(Theme {
                name: "Hand".into(),
                colors: colors(&[("green", "#123456")])
            })
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn clear_theme_on_a_theme_that_was_never_set_is_a_success() {
        let path = temp_path();
        clear_theme(&path); // no file yet
        clear_theme(&path); // still none
        assert_eq!(load_theme(&path), None);
    }

    #[test]
    fn save_theme_creates_the_data_root_on_first_write() {
        let path = temp_path();
        save_theme(
            &Theme {
                name: "Fjord".into(),
                colors: colors(&[("green", "#5c88c9")]),
            },
            &path,
        )
        .unwrap();
        let stored: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            stored,
            j!({ "name": "Fjord", "colors": { "green": "#5c88c9" } })
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    // ---- the routes ---------------------------------------------------------
    //
    // The handlers read `theme_path()`, which follows `BOUGH_HOME` — set to a
    // fresh temp dir per test, serialized by a lock because the env is
    // process-global.

    /// The CRATE-wide lock (`http::testutil::home_lock`), not a module-local
    /// one: `BOUGH_HOME` is one variable, so one lock. Two module-local locks
    /// let a handler in one module read another module's temp home.
    fn env_lock() -> MutexGuard<'static, ()> {
        crate::http::testutil::home_lock()
    }

    struct HomeGuard {
        previous: Option<String>,
        home: PathBuf,
        _lock: MutexGuard<'static, ()>,
    }

    fn with_home() -> HomeGuard {
        let lock = env_lock();
        let previous = std::env::var("BOUGH_HOME").ok();
        let home = std::env::temp_dir().join(format!("bough-theme-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("BOUGH_HOME", &home);
        HomeGuard {
            previous,
            home,
            _lock: lock,
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => std::env::set_var("BOUGH_HOME", v),
                None => std::env::remove_var("BOUGH_HOME"),
            }
            std::fs::remove_dir_all(&self.home).ok();
        }
    }

    #[tokio::test]
    async fn get_theme_with_nothing_stored_is_200_with_the_defaults_not_a_404() {
        let _home = with_home();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/theme")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        assert!(body["theme"].is_null());
        let defaults = body["defaults"].as_object().unwrap();
        assert_eq!(defaults.len(), 18, "the fixed 18-token set");
        assert_eq!(defaults["green"], "#4ec98f");
    }

    #[tokio::test]
    async fn put_then_get_round_trips_a_partial_palette_with_defaults_kept_separate() {
        let _home = with_home();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let put = call
            .call(testutil::req(
                "PUT",
                "/theme",
                Some(j!({ "name": "Iris", "colors": { "green": "#9a7fd1" } })),
            ))
            .await;
        assert_eq!(put.status(), 200);
        let put_body = testutil::body_json(put).await;
        assert_eq!(
            put_body["theme"],
            j!({ "name": "Iris", "colors": { "green": "#9a7fd1" } })
        );

        let get = testutil::body_json(call.call(testutil::get("/theme")).await).await;
        // The whole point of serving both halves: the client can still tell
        // that `amber` is INHERITED rather than chosen, which a merged map
        // cannot express.
        assert!(get["theme"]["colors"].get("amber").is_none());
        assert_eq!(get["defaults"]["amber"], "#d9b45f");
        assert_eq!(
            get["theme"],
            j!({ "name": "Iris", "colors": { "green": "#9a7fd1" } })
        );
    }

    #[tokio::test]
    async fn put_with_an_unknown_token_is_a_400_naming_it_and_does_not_overwrite() {
        let _home = with_home();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        call.call(testutil::req(
            "PUT",
            "/theme",
            Some(j!({ "name": "Iris", "colors": { "green": "#9a7fd1" } })),
        ))
        .await;
        let bad = call
            .call(testutil::req(
                "PUT",
                "/theme",
                Some(j!({ "name": "Broken", "colors": { "forground": "#000000" } })),
            ))
            .await;
        assert_eq!(bad.status(), 400);
        let msg = testutil::body_json(bad).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.contains("forground"), "{msg}");
        // The stored theme is untouched: validation happens before the write.
        let get = testutil::body_json(call.call(testutil::get("/theme")).await).await;
        assert_eq!(get["theme"]["name"], "Iris");
    }

    #[tokio::test]
    async fn delete_theme_returns_to_the_built_in_palette_and_is_idempotent() {
        let _home = with_home();
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        call.call(testutil::req(
            "PUT",
            "/theme",
            Some(j!({ "name": "Iris", "colors": { "green": "#9a7fd1" } })),
        ))
        .await;
        let first = call.call(testutil::req("DELETE", "/theme", None)).await;
        assert_eq!(first.status(), 200);
        assert!(testutil::body_json(first).await["theme"].is_null());
        // Idempotent: the state the caller asked for is the state they get.
        let second = call.call(testutil::req("DELETE", "/theme", None)).await;
        assert_eq!(second.status(), 200);
        assert!(testutil::body_json(second).await["theme"].is_null());
        let get = testutil::body_json(call.call(testutil::get("/theme")).await).await;
        assert!(get["theme"].is_null());
    }
}
