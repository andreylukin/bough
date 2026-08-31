//! V7's teeth (WP-6): a gate that never fails proves nothing. Each planted fixture is a mismatch
//! the compiler cannot catch, and each must fail the gate.
//!
//! The fixtures live under `tests/fixtures/`, which `scan()`'s walk skips on purpose: they must
//! never enter the tree's own catalog. They are fed to `scan_source` by hand here.

use std::path::{Path, PathBuf};

use xtask::scan::{scan_source, Catalog};
use xtask::{check, Finding};

fn findings_for(fixture: &str) -> Vec<Finding> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/planted")
        .join(fixture);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut c = Catalog {
        decls: Vec::new(),
        sites: Vec::new(),
    };
    scan_source(
        Path::new(&format!("plugins/planted/src/{fixture}")),
        &src,
        &mut c,
    )
    .expect("the fixture parses");
    check(&c)
}

#[test]
fn the_planted_mode_override_fails_the_gate() {
    let f = findings_for("mode_override.rs");
    assert!(
        f.iter()
            .any(|f| matches!(f, Finding::ModeOverrideDisagreesWithTrait { .. })),
        "{f:?}"
    );
}

#[test]
fn the_planted_duplicate_name_fails_the_gate() {
    let f = findings_for("duplicate_name.rs");
    assert!(
        f.iter()
            .any(|f| matches!(f, Finding::NameDeclaredTwiceWithDifferentModes { name, .. } if name == "fixture/ping")),
        "{f:?}"
    );
}

#[test]
fn the_planted_wrong_dispatch_fails_the_gate() {
    let f = findings_for("wrong_dispatch.rs");
    assert!(
        f.iter()
            .any(|f| matches!(f, Finding::DispatchModeDiffersFromDeclaration { .. })),
        "{f:?}"
    );
}

#[test]
fn the_clean_fixture_passes() {
    assert_eq!(findings_for("clean.rs"), vec![]);
}
