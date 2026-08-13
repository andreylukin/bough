//! The last tool list each server advertised, remembered across sessions.
//!
//! WHY THIS EXISTS. `prompt_mcp_servers` is pure and never connects — the
//! prompt is assembled on the turn's critical path — so a granted server that
//! nothing had called yet used to render as "granted, not connected yet … run
//! `bough mcp test <name>` to list its tools". The model did exactly that,
//! and the field database says what it cost: `bough mcp test slack` ran in 28
//! distinct sessions, `notion` in 25, `linear-server` in 23 — once per
//! session, every session, before any work began. The catalog was knowable
//! and we asked the model to go and buy it again.
//!
//! So a successful connection writes its tool names here, and assembly reads
//! them. That keeps assembly pure (this is a file read, not a spawn) while
//! making the FIRST turn of a fresh session as informed as the second.
//!
//! WHAT IT IS NOT. It is not the live answer and must never be rendered as
//! one: a tool can vanish between the connection that wrote this and the call
//! that reads it. Every surface that shows it says where it came from, and
//! `bough mcp` still answers from live state only (`status.rs` holds that
//! invariant). This file is a hint that makes the first call well-aimed, not
//! a cache anything trusts.
//!
//! TOOL NAMES ONLY. No descriptions, no arguments, no credentials — a server
//! name and the words it answers to.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::mcp_catalog_path;

/// One server's remembered catalog.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CachedCatalog {
    pub tools: Vec<String>,
    /// When the connection that advertised them was established.
    pub at: i64,
}

/// Where the document lives. A test points this at a temp file and gets a
/// hermetic catalog, the same shape `McpConfigOptions` uses for the registry.
#[derive(Clone, Default)]
pub struct CatalogOptions {
    pub file: Option<PathBuf>,
}

fn catalog_file(opts: &CatalogOptions) -> PathBuf {
    opts.file.clone().unwrap_or_else(mcp_catalog_path)
}

type Document = BTreeMap<String, CachedCatalog>;

/// The whole document, or empty. A missing, unreadable or corrupt file is an
/// empty catalog and never an error: this is a hint, and a turn must not fail
/// because a hint could not be read.
pub fn load_catalog(opts: &CatalogOptions) -> Document {
    std::fs::read_to_string(catalog_file(opts))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// What `server` last advertised, if anything ever did.
pub fn cached_catalog(server: &str, opts: &CatalogOptions) -> Option<CachedCatalog> {
    load_catalog(opts)
        .remove(server)
        .filter(|c| !c.tools.is_empty())
}

/// Record what a live connection advertised. Silent on every failure — a
/// connection that worked must not be reported as broken because a hint file
/// was not writable.
pub fn remember_catalog(server: &str, tools: &[String], at: i64, opts: &CatalogOptions) {
    if tools.is_empty() {
        return;
    }
    let mut doc = load_catalog(opts);
    doc.insert(
        server.to_string(),
        CachedCatalog {
            tools: tools.to_vec(),
            at,
        },
    );
    let path = catalog_file(opts);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&doc) {
        let _ = std::fs::write(&path, format!("{text}\n"));
    }
}

/// Drop a server's entry — deregistering one must not leave its tools being
/// advertised to every future session.
pub fn forget_catalog(server: &str, opts: &CatalogOptions) {
    let mut doc = load_catalog(opts);
    if doc.remove(server).is_none() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(&doc) {
        let _ = std::fs::write(catalog_file(opts), format!("{text}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> CatalogOptions {
        let dir = std::env::temp_dir().join(format!("bough-mcp-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        CatalogOptions {
            file: Some(dir.join("mcp-catalog.json")),
        }
    }

    #[test]
    fn a_remembered_catalog_reads_back_with_its_timestamp() {
        let opts = temp();
        remember_catalog("slack", &["slack_send_message".into()], 1_700, &opts);
        let hit = cached_catalog("slack", &opts).unwrap();
        assert_eq!(hit.tools, vec!["slack_send_message".to_string()]);
        assert_eq!(hit.at, 1_700);
    }

    #[test]
    fn a_missing_file_is_an_empty_catalog_and_never_an_error() {
        let opts = CatalogOptions {
            file: Some("/nonexistent/dir/mcp-catalog.json".into()),
        };
        assert!(cached_catalog("slack", &opts).is_none());
        // Writing into an unwritable path stays silent.
        remember_catalog("slack", &["x".into()], 1, &opts);
    }

    #[test]
    fn a_corrupt_document_is_an_empty_catalog_rather_than_a_failed_turn() {
        let opts = temp();
        std::fs::write(opts.file.clone().unwrap(), "{not json").unwrap();
        assert!(cached_catalog("slack", &opts).is_none());
    }

    #[test]
    fn remembering_one_server_leaves_the_others_alone() {
        let opts = temp();
        remember_catalog("slack", &["a".into()], 1, &opts);
        remember_catalog("notion", &["b".into()], 2, &opts);
        assert_eq!(cached_catalog("slack", &opts).unwrap().tools, vec!["a"]);
        assert_eq!(cached_catalog("notion", &opts).unwrap().tools, vec!["b"]);
    }

    #[test]
    fn an_empty_tool_list_is_not_worth_remembering() {
        let opts = temp();
        remember_catalog("slack", &[], 1, &opts);
        assert!(cached_catalog("slack", &opts).is_none());
    }

    #[test]
    fn forgetting_a_server_stops_it_being_advertised() {
        let opts = temp();
        remember_catalog("slack", &["a".into()], 1, &opts);
        forget_catalog("slack", &opts);
        assert!(cached_catalog("slack", &opts).is_none());
    }
}
