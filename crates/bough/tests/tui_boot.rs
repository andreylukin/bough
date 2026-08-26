//! §0.2's "an enabled row that never activates is a BOOT FAILURE", and the ORDER in which that
//! failure is reported (V8).
//!
//! `tui.never` is the deliberate vehicle: it declares an injection nobody provides, so it can
//! never activate however the tree is composed. What this file pins is the half `boot.rs` used to
//! get wrong — the report must be printed AFTER `kernel.shutdown()`, because a Phase-3 surface row
//! owns the alt screen and anything written before the restore is wiped by it.

mod support;

use bough_plugin_hello::trace;

/// A tree with `tui.never` in it: the shell provides `tui`, and `tui.never` additionally asks for
/// a key nothing in the catalog provides.
const NEVER: &str = "\
- id: ledger
  plugin: ledger-memory
  config: {}
- id: commands
  plugin: commands
  config: { prefix: \"/\", suggestions: true }
- id: agents
  plugin: agents
- id: tui
  plugin: tui-shell
  config:
    backend: headless
    size: [120, 40]
    frame_ms: 16
    tick_ms: 250
    theme: dark
    mouse: true
    osc52: true
    clipboard: false
    composer_max_lines: 6
- id: tui.never
  plugin: tui-never
  config: {}
";

#[tokio::test]
async fn a_row_that_never_activates_fails_the_boot_after_teardown() {
    let _guard = trace::test_lock();
    // `boot_with` asserts QUIESCENCE, not activation: a row that can never activate quiesces
    // perfectly happily as `Pending`, which is exactly the state this gate is about.
    let (kernel, _dir) = support::boot_with(NEVER).await;

    let snapshot = kernel.snapshot();
    let unresolved = snapshot.unresolved();
    assert!(
        unresolved.iter().any(|r| r.id.as_str() == "tui.never"),
        "`tui.never` must be unresolved: {unresolved:#?}"
    );
    assert!(
        bough::boot::assert_all_activated(&snapshot).is_err(),
        "an enabled row that never activates is a boot failure (§0.2)"
    );

    // The snapshot the report is rendered from is taken BEFORE shutdown, so the report survives
    // the teardown that now precedes it.
    kernel.shutdown().await;
    let report = bough::boot::describe_unresolved(&snapshot);
    assert!(report.contains("tui.never"), "{report}");
    assert!(
        report.contains("a_key_nobody_provides"),
        "the report must name the unmet key: {report}"
    );
}

/// The V8 ORDER, as BEHAVIOUR: the launcher tears the tree down and only then prints the
/// unresolved-row report. A report written before the restore is written INTO the alt screen and
/// wiped by it, so on the one path that most needs to be readable the failure would be invisible.
///
/// This used to be asserted by reading `boot.rs` as TEXT and comparing `str::find` offsets, which
/// would have passed on a branch that was dead or commented around and failed on a rename. Here
/// the real binary runs a tree with a probe row that marks its own unwind on stderr and a
/// `tui.never` row that can never activate: the marker and the report land on ONE ordered stream.
#[test]
fn the_unresolved_report_is_printed_after_the_teardown() {
    const MARKER: &str = "PROBE-UNWOUND";
    let home = tempfile::tempdir().expect("a temp home");
    let bundle = format!(
        "{NEVER}- id: tui.probe\n  plugin: tui-probe\n  config:\n    text: probe\n    \
         panic_key: \"\"\n    teardown_marker: \"{MARKER}\"\n"
    );
    std::fs::create_dir_all(home.path().join("bundles")).unwrap();
    std::fs::create_dir_all(home.path().join("profiles")).unwrap();
    std::fs::write(home.path().join("bundles/v8.yml"), bundle).unwrap();
    std::fs::write(
        home.path().join("profiles/tui.yml"),
        "name: tui\ninvariants: false\nbundles: [v8]\n",
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bough"))
        .args(["--profile", "tui", "--check", "--no-watch"])
        .arg("--root")
        .arg(home.path())
        .env("BOUGH_HOME", home.path())
        .env("HOME", home.path())
        .output()
        .expect("run bough --check");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "an enabled row that never activates is a boot failure: {stderr}"
    );
    let unwound = stderr
        .find(MARKER)
        .unwrap_or_else(|| panic!("the probe row must unwind before exit: {stderr}"));
    // The REPORT's own first line — not the row id, which the kernel's own WARN also carries.
    let report = stderr
        .find("enabled row(s) never activated")
        .unwrap_or_else(|| panic!("the report must be printed: {stderr}"));
    assert!(
        stderr[report..].contains("tui.never"),
        "the report must name the unresolved row: {stderr}"
    );
    assert!(
        unwound < report,
        "teardown must come BEFORE the report, or the alt-screen restore wipes it:\n{stderr}"
    );
    assert!(
        stderr.contains("a_key_nobody_provides"),
        "the report must name the unmet key: {stderr}"
    );
}
