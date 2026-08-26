//! §0.3 loader: an entry may carry `include:`, an external YAML file grafted in. §0.2: a missing
//! referent is never silently skipped.
//!
//! These drive the real binary, because the bug this pins was precisely that `include:` worked in
//! the kernel's unit tests and was dead on the production path.

mod support;

use support::TempDir;

fn dump(dir: &TempDir) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_bough"))
        .args(["--profile", "tui", "--dump-config"])
        .env("BOUGH_HOME", dir.path())
        .output()
        .expect("the launcher runs")
}

#[test]
fn a_user_patch_include_is_grafted_into_the_composed_tree() {
    let dir = TempDir::new("include-ok");
    std::fs::write(
        dir.path().join("extra.yml"),
        "- id: grafted.row\n  plugin: greeting-echo\n  config: { suffix: \"-grafted\" }\n  isolate: { greeting: grafted }\n",
    )
    .unwrap();
    support::write_patch(
        &dir,
        "insert:\n  - entry:\n      id: include.host\n      plugin: greeting-echo\n      config: { suffix: \"-host\" }\n      isolate: { greeting: host }\n      include: extra.yml\n",
    );

    let out = dump(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "--dump-config must exit 0: {stderr}");
    let text = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        text.contains("grafted.row"),
        "the included row must appear in the composed tree:\n{text}"
    );
    assert!(
        !text.contains("extra.yml"),
        "the include was consumed at parse time, not carried into the tree:\n{text}"
    );
}

#[test]
fn a_missing_include_is_an_error_not_a_skipped_row() {
    let dir = TempDir::new("include-missing");
    support::write_patch(
        &dir,
        "insert:\n  - entry:\n      id: include.host\n      plugin: greeting-echo\n      config: { suffix: \"\" }\n      include: definitely-not-here.yml\n",
    );

    let out = dump(&dir);
    assert!(
        !out.status.success(),
        "a missing include must fail, not compose cleanly"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("definitely-not-here.yml"),
        "the error must name the missing file: {stderr}"
    );
}
