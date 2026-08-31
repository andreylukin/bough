//! V7, end to end: the shipped gate BINARY (`cargo xtask events`) lists every event in the real
//! tree with its declared dispatch mode, and a mismatch planted into a copy of the REAL tree
//! source (not a hand-written fixture) fails the gate.

use std::path::{Path, PathBuf};
use std::process::Command;

use xtask::scan::SiteKind;
use xtask::{check, scan, DispatchMode, ROOTS};

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

fn xtask_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
}

/// The binary itself — the thing `make events` runs — passes on this tree and says so.
#[test]
fn the_gate_binary_exits_zero_on_this_tree() {
    let out = xtask_bin()
        .args(["events", "--check"])
        .output()
        .expect("the xtask binary runs");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "`xtask events --check` failed:\n{err}"
    );
    assert!(
        err.contains("no findings"),
        "expected a clean summary, got:\n{err}"
    );
}

/// "lists EVERY event with its dispatch mode": every declaration the scan finds in the real tree
/// appears in the binary's printed table under the mode the source declares.
#[test]
fn the_binary_table_lists_every_event_with_its_mode() {
    let out = xtask_bin()
        .args(["events"])
        .output()
        .expect("the xtask binary runs");
    assert!(out.status.success());
    let table = String::from_utf8_lossy(&out.stdout).into_owned();

    let catalog = tree();
    assert!(!catalog.decls.is_empty(), "the tree declares events");
    for d in &catalog.decls {
        let row = format!(
            "| `{}` | {} | `{}` |",
            d.name,
            d.effective_mode().as_str(),
            d.ty
        );
        assert!(
            table.contains(&row),
            "the printed catalog is missing {row}\n(from {}:{})",
            d.file.display(),
            d.line
        );
        // and never under a mode it does not declare.
        for m in [
            DispatchMode::Emit,
            DispatchMode::Parallel,
            DispatchMode::Serial,
            DispatchMode::Waterfall,
        ] {
            if m == d.effective_mode() {
                continue;
            }
            let wrong = format!("| `{}` | {} | `{}` |", d.name, m.as_str(), d.ty);
            let also_declared = catalog
                .decls
                .iter()
                .any(|o| o.name == d.name && o.ty == d.ty && o.effective_mode() == m);
            if !also_declared {
                assert!(!table.contains(&wrong), "the catalog also prints {wrong}");
            }
        }
    }
}

/// Copy every `.rs` under `crates/` and `plugins/` into `dst`, preserving relative paths.
fn copy_tree(src_root: &Path, dst: &Path) {
    for r in ROOTS {
        copy_rs(&src_root.join(r), src_root, dst);
    }
}

fn copy_rs(dir: &Path, src_root: &Path, dst: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if name.starts_with('.') || name == "target" {
                continue;
            }
            copy_rs(&p, src_root, dst);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let rel = p.strip_prefix(src_root).expect("under the root");
            let out = dst.join(rel);
            std::fs::create_dir_all(out.parent().unwrap()).unwrap();
            std::fs::copy(&p, &out).unwrap();
        }
    }
}

fn scan_copy(dst: &Path) -> xtask::Catalog {
    let roots: Vec<PathBuf> = ROOTS.iter().map(|r| dst.join(r)).collect();
    let refs: Vec<&Path> = roots.iter().map(|p| p.as_path()).collect();
    scan(&refs).expect("the copied tree parses")
}

/// The teeth: take a REAL dispatch site out of this tree, flip its dispatch method to a mode the
/// event does not declare, and the gate must report exactly that file, line and type.
#[test]
fn a_mismatch_planted_in_a_copy_of_the_real_tree_fails_the_gate() {
    let root = workspace_root();
    let tmp = std::env::temp_dir().join(format!("xtask-v7-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    copy_tree(&root, &tmp);

    // The copy is the same tree, so it must be clean before we plant anything.
    let before = scan_copy(&tmp);
    assert!(
        check(&before).is_empty(),
        "the untouched copy already has findings: {:?}",
        check(&before)
    );

    // Pick a real emit site whose type declares emit and nothing else.
    let victim = before
        .sites
        .iter()
        .find(|s| {
            s.kind == SiteKind::Dispatch
                && s.mode == DispatchMode::Emit
                && before
                    .decls
                    .iter()
                    .filter(|d| d.ty == s.ty)
                    .all(|d| d.effective_mode() == DispatchMode::Emit)
                && before.decls.iter().any(|d| d.ty == s.ty)
        })
        .expect("this tree has at least one plain emit site")
        .clone();

    // Flip `emit::<Ty>` to `waterfall::<Ty>` at that line, in the copy only.
    let rel = victim.file.strip_prefix(&tmp).unwrap().to_path_buf();
    let path = tmp.join(&rel);
    let src = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = src.lines().map(String::from).collect();
    let needle = format!("emit::<{}", victim.ty);
    let line = &mut lines[victim.line - 1];
    assert!(
        line.contains(&needle),
        "expected {needle:?} at {}:{}, got {line:?}",
        rel.display(),
        victim.line
    );
    *line = line.replace(&needle, &format!("waterfall::<{}", victim.ty));
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let after = scan_copy(&tmp);
    let findings = check(&after);
    assert!(
        findings.iter().any(|f| matches!(
            f,
            xtask::Finding::DispatchModeDiffersFromDeclaration { site, .. }
                if site.ty == victim.ty && site.file == path && site.line == victim.line
        )),
        "planting a waterfall dispatch of {} (declared emit) at {}:{} did not fail the gate; findings: {:?}",
        victim.ty,
        rel.display(),
        victim.line,
        findings
    );

    std::fs::remove_dir_all(&tmp).ok();
}
