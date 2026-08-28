//! V7 (WP-6): THIS tree has no event findings, the catalog is past the floor, and the committed
//! `docs/event-catalog.md` matches what the tree declares today.

use std::path::{Path, PathBuf};

use xtask::{check, event_count, scan, table, CATALOG_FLOOR, CATALOG_PATH, ROOTS};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/xtask has a workspace root above it")
        .to_path_buf()
}

fn tree() -> xtask::Catalog {
    let root = workspace_root();
    let roots: Vec<PathBuf> = ROOTS.iter().map(|r| root.join(r)).collect();
    let refs: Vec<&Path> = roots.iter().map(|p| p.as_path()).collect();
    scan(&refs).expect("the tree parses")
}

#[test]
fn this_tree_has_no_event_findings() {
    let findings = check(&tree());
    let rendered: Vec<String> = findings.iter().map(|f| f.to_string()).collect();
    assert!(
        findings.is_empty(),
        "event catalog findings:\n{}",
        rendered.join("\n")
    );
}

#[test]
fn the_catalog_is_past_the_thirty_event_floor() {
    let n = event_count(&tree());
    assert!(
        n >= CATALOG_FLOOR,
        "§15 item 7 asks for the gate past ~{CATALOG_FLOOR} events; the tree declares {n}"
    );
}

#[test]
fn the_committed_catalog_matches_the_tree() {
    let path = workspace_root().join(CATALOG_PATH);
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} — run `cargo xtask events --write`", path.display()));
    assert_eq!(
        committed,
        table(&tree()),
        "{} is stale — run `cargo xtask events --write`",
        path.display()
    );
}
