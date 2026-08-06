//! What a NEW conversation runs on, stored in `~/.bough/model.json` (port of
//! `src/server/defaults.ts`).
//!
//! A sibling of `theme.rs`, deliberately: both are "exactly one per install,
//! not per session", both are a preference rather than data, and both live in
//! a JSON file rather than a schema whose table set is closed.
//!
//! FORGIVING ON READ, like the theme: a missing file is the ordinary state and
//! not a failure, and a file that cannot be parsed — or that a hand-edit
//! filled with the wrong types — degrades to "nothing pinned" rather than
//! taking the server down on the path that answers what model to use.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use bough_core::paths::model_settings_path;
use bough_core::types::Effort;

/// The stored pins. Field order (`model`, then `effort`) is the on-disk key
/// order the TS server writes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ModelDefaults {
    /// The frontier model a new conversation is created with. `None` = not pinned.
    pub model: Option<String>,
    /// The thinking depth it starts at. `None` = let the provider decide.
    pub effort: Option<Effort>,
}

/// The unpinned state.
pub const NO_DEFAULTS: ModelDefaults = ModelDefaults { model: None, effort: None };

/// The production location: `~/.bough/model.json`.
pub fn default_path() -> PathBuf {
    model_settings_path()
}

/// The stored defaults, or [`NO_DEFAULTS`] when nothing is pinned.
///
/// `path` is injected by tests so nothing here touches a real `~/.bough` — a
/// test that wrote the developer's own home directory would be a test that
/// changed their editor's model.
pub fn load_defaults(path: &Path) -> ModelDefaults {
    let Ok(raw) = std::fs::read_to_string(path) else { return NO_DEFAULTS };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else { return NO_DEFAULTS };
    let Some(obj) = value.as_object() else { return NO_DEFAULTS };
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    // An unknown effort is dropped INDEPENDENTLY: the model beside it survives.
    let effort = obj
        .get("effort")
        .cloned()
        .and_then(|v| serde_json::from_value::<Effort>(v).ok());
    ModelDefaults { model, effort }
}

/// Persist the defaults. Creates the parent directory on the first write.
///
/// Rebuilt rather than passed through, so the file holds exactly the two
/// validated keys and nothing a looser caller let ride along.
pub fn save_defaults(next: &ModelDefaults, path: &Path) -> ModelDefaults {
    let clean = ModelDefaults {
        model: next
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string),
        effort: next.effort,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::to_string_pretty(&clean).unwrap_or_else(|_| "{}".to_string());
    let _ = std::fs::write(path, body + "\n");
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every test injects its own path. Nothing here may touch a real `~/.bough`.
    fn scratch() -> PathBuf {
        std::env::temp_dir()
            .join(format!("bough-defaults-{}", uuid::Uuid::new_v4()))
            .join("model.json")
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn nothing_stored_is_the_ordinary_state_not_a_failure() {
        assert_eq!(load_defaults(&scratch()), NO_DEFAULTS);
    }

    #[test]
    fn a_saved_default_round_trips() {
        let path = scratch();
        save_defaults(
            &ModelDefaults { model: Some("claude-sonnet-5".into()), effort: Some(Effort::High) },
            &path,
        );
        assert_eq!(
            load_defaults(&path),
            ModelDefaults { model: Some("claude-sonnet-5".into()), effort: Some(Effort::High) }
        );
    }

    #[test]
    fn null_clears_a_pin_let_the_provider_decide_is_a_real_state() {
        let path = scratch();
        save_defaults(
            &ModelDefaults { model: Some("claude-sonnet-5".into()), effort: Some(Effort::High) },
            &path,
        );
        save_defaults(
            &ModelDefaults { model: Some("claude-sonnet-5".into()), effort: None },
            &path,
        );
        assert_eq!(
            load_defaults(&path),
            ModelDefaults { model: Some("claude-sonnet-5".into()), effort: None }
        );
    }

    #[test]
    fn a_hand_edited_file_degrades_to_unpinned_rather_than_failing() {
        // This runs on the path that answers WHICH MODEL TO USE. Taking the
        // server down because someone fat-fingered the JSON would be much
        // worse than falling back.
        for bad in ["{ not json", "null", "[]", "\"a string\"", r#"{"model": 42}"#] {
            let path = scratch();
            write(&path, bad);
            assert_eq!(load_defaults(&path), NO_DEFAULTS, "{bad}");
        }
    }

    #[test]
    fn an_unknown_effort_is_dropped_and_the_model_beside_it_survives() {
        let path = scratch();
        write(&path, r#"{"model": "claude-opus-5", "effort": "turbo"}"#);
        assert_eq!(
            load_defaults(&path),
            ModelDefaults { model: Some("claude-opus-5".into()), effort: None }
        );
    }

    #[test]
    fn blank_and_whitespace_only_models_read_as_unpinned() {
        // "" is what an empty picker row would send; it must not become a pin
        // on the empty string, which would resolve to no model at all.
        let path = scratch();
        write(&path, r#"{"model": "   ", "effort": "low"}"#);
        assert_eq!(
            load_defaults(&path),
            ModelDefaults { model: None, effort: Some(Effort::Low) }
        );
    }

    #[test]
    fn save_rebuilds_the_document_it_trims_and_holds_exactly_two_keys() {
        let path = scratch();
        save_defaults(
            &ModelDefaults { model: Some(" claude-opus-5 ".into()), effort: Some(Effort::Max) },
            &path,
        );
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut keys: Vec<&str> =
            stored.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["effort", "model"]);
        assert_eq!(stored["model"], "claude-opus-5");
        // Trailing newline, like the TS writer.
        assert!(std::fs::read_to_string(&path).unwrap().ends_with('\n'));
    }
}
