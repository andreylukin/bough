//! Invariant: the `ui` patch layer file only ever holds the ONE shape the panel writes —
//! `entries: { <row-id>: { disabled: <bool> } }` — and the panel refuses to touch a file holding
//! anything else. The layer is a DIFF, not a state dump: an entry exists only while the panel's
//! setting differs from what the other layers said, ids absent from the composed tree are pruned
//! on the next write, and an empty diff is an absent file. Writes are write-then-rename, so the
//! watcher never composes a half-written document.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What the panel knows about the file: id → the `disabled` it pins.
pub type UiEntries = BTreeMap<String, bool>;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum StoreError {
    /// The file exists but was not (only) written by the panel. The panel must not clobber a
    /// human's work: the error names the file so the fix (edit or delete it) is the reader's.
    #[error("{path} holds content the panel did not write ({detail}); not touching it")]
    Foreign { path: String, detail: String },
    #[error("{path}: {detail}")]
    Io { path: String, detail: String },
}

/// Parse the file's content. Absent (`None`) and empty are an empty diff; anything but the one
/// shape is [`StoreError::Foreign`].
pub fn parse(path: &Path, text: Option<&str>) -> Result<UiEntries, StoreError> {
    let foreign = |detail: &str| StoreError::Foreign {
        path: path.display().to_string(),
        detail: detail.to_string(),
    };
    let Some(text) = text else {
        return Ok(UiEntries::new());
    };
    if text.trim().is_empty() {
        return Ok(UiEntries::new());
    }
    let doc: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| foreign(&format!("not YAML: {e}")))?;
    let mut out = UiEntries::new();
    let serde_yaml::Value::Mapping(m) = doc else {
        if matches!(doc, serde_yaml::Value::Null) {
            return Ok(out);
        }
        return Err(foreign("the document is not a mapping"));
    };
    for (k, v) in &m {
        match k.as_str() {
            Some("entries") => {}
            Some(other) => return Err(foreign(&format!("unexpected key `{other}`"))),
            None => return Err(foreign("a non-string top-level key")),
        }
        let entries = match v {
            serde_yaml::Value::Mapping(entries) => entries,
            // `entries:` with nothing under it — what the file looks like mid-edit by hand.
            serde_yaml::Value::Null => continue,
            _ => return Err(foreign("`entries` is not a mapping")),
        };
        for (id, body) in entries {
            let Some(id) = id.as_str() else {
                return Err(foreign("a non-string row id"));
            };
            let serde_yaml::Value::Mapping(body) = body else {
                return Err(foreign(&format!("`{id}` does not hold a mapping")));
            };
            if body.len() != 1 {
                return Err(foreign(&format!("`{id}` holds more than `disabled`")));
            }
            let disabled = body
                .get(serde_yaml::Value::String("disabled".into()))
                .and_then(serde_yaml::Value::as_bool)
                .ok_or_else(|| {
                    foreign(&format!(
                        "`{id}` holds something besides `disabled: <bool>`"
                    ))
                })?;
            out.insert(id.to_string(), disabled);
        }
    }
    Ok(out)
}

/// The toggle rule, PURE. If the panel already has an opinion on `id`, withdraw it (the other
/// layers' value returns); otherwise pin the opposite of what stands. Self-correcting when a
/// lower layer moved underneath: the next render shows what actually won, and the next press
/// pins. Ids no longer in the composed tree are pruned in the same pass.
pub fn toggled(
    entries: &UiEntries,
    id: &str,
    effective_disabled: bool,
    known_ids: &BTreeSet<String>,
) -> UiEntries {
    let mut next: UiEntries = entries
        .iter()
        .filter(|(k, _)| known_ids.contains(*k))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    if next.remove(id).is_none() {
        next.insert(id.to_string(), !effective_disabled);
    }
    next
}

/// Render the one shape. Deterministic: `UiEntries` is a `BTreeMap`.
pub fn render(entries: &UiEntries) -> String {
    let mut out = String::from(
        "# Written by the bough panel (`x` on a row). One shape only:\n\
         # entries: { <row-id>: { disabled: <bool> } }. Delete the file to reset every toggle.\n\
         entries:\n",
    );
    for (id, disabled) in entries {
        out.push_str(&format!("  {id}: {{ disabled: {disabled} }}\n"));
    }
    out
}

/// Read the file from disk. Absent is an empty diff, not an error.
pub fn read(path: &Path) -> Result<UiEntries, StoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, Some(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UiEntries::new()),
        Err(e) => Err(StoreError::Io {
            path: path.display().to_string(),
            detail: e.to_string(),
        }),
    }
}

/// Write the diff: rename over the old file so the watcher never sees a torn document; an empty
/// diff REMOVES the file (reset is deletion, and the watch fires on the remove like any change).
pub fn write(path: &Path, entries: &UiEntries) -> Result<(), StoreError> {
    let io = |detail: String| StoreError::Io {
        path: path.display().to_string(),
        detail,
    };
    if entries.is_empty() {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(e.to_string())),
        };
    }
    let dir = path
        .parent()
        .ok_or_else(|| io("no parent directory".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| io(e.to_string()))?;
    // The tmp name is stable: two concurrent writers is not a case this file has (one panel per
    // process, one process per $BOUGH_HOME under the home lock), and a leftover is overwritten.
    let tmp = path.with_extension("yml.tmp");
    std::fs::write(&tmp, render(entries)).map_err(|e| io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| io(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ids(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn p() -> PathBuf {
        PathBuf::from("/x/bough.ui.patch.yml")
    }

    #[test]
    fn an_absent_or_empty_file_is_an_empty_diff() {
        assert_eq!(parse(&p(), None).unwrap(), UiEntries::new());
        assert_eq!(parse(&p(), Some("")).unwrap(), UiEntries::new());
        assert_eq!(parse(&p(), Some("entries:\n")).unwrap(), UiEntries::new());
    }

    #[test]
    fn the_one_shape_round_trips() {
        let mut e = UiEntries::new();
        e.insert("old-feed".into(), false);
        e.insert("collect.slack".into(), true);
        let text = render(&e);
        assert_eq!(parse(&p(), Some(&text)).unwrap(), e);
    }

    #[test]
    fn foreign_content_is_refused_not_clobbered() {
        for text in [
            "entries:\n  a: { disabled: true, plugin: x }\n",
            "entries:\n  a: { config: {} }\n",
            "entries: []\n",
            "insert: []\n",
            "just a string",
        ] {
            let err = parse(&p(), Some(text)).expect_err(text);
            assert!(matches!(err, StoreError::Foreign { .. }), "{text}: {err}");
        }
    }

    #[test]
    fn toggling_with_no_opinion_pins_the_opposite_of_what_stands() {
        let next = toggled(&UiEntries::new(), "old-feed", true, &ids(&["old-feed"]));
        assert_eq!(next.get("old-feed"), Some(&false));
        let next = toggled(
            &UiEntries::new(),
            "tui.search",
            false,
            &ids(&["tui.search"]),
        );
        assert_eq!(next.get("tui.search"), Some(&true));
    }

    #[test]
    fn toggling_an_existing_opinion_withdraws_it() {
        let mut e = UiEntries::new();
        e.insert("old-feed".into(), false);
        let next = toggled(&e, "old-feed", false, &ids(&["old-feed"]));
        assert!(next.is_empty(), "{next:?}");
    }

    #[test]
    fn ids_gone_from_the_tree_are_pruned_on_the_next_write() {
        let mut e = UiEntries::new();
        e.insert("gone.row".into(), true);
        e.insert("still.here".into(), true);
        let next = toggled(&e, "other", false, &ids(&["still.here", "other"]));
        assert!(!next.contains_key("gone.row"), "{next:?}");
        assert!(next.contains_key("still.here"));
    }

    #[test]
    fn write_renames_and_an_empty_diff_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bough.ui.patch.yml");
        let mut e = UiEntries::new();
        e.insert("a".into(), true);
        write(&path, &e).unwrap();
        assert_eq!(read(&path).unwrap(), e);
        assert!(
            !path.with_extension("yml.tmp").exists(),
            "tmp file left behind"
        );
        write(&path, &UiEntries::new()).unwrap();
        assert!(!path.exists(), "an empty diff must remove the file");
        // Removing an already-absent file is not an error (double reset).
        write(&path, &UiEntries::new()).unwrap();
    }
}
