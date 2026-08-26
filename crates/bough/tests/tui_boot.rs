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

#[tokio::test]
async fn the_unresolved_report_is_printed_after_the_teardown() {
    // Not a behavioural assertion about stderr — it is the SOURCE ORDER of the launcher's failure
    // path, which is the thing that regressed and the thing V8 depends on. `boot.rs` is read from
    // disk so a future edit that puts the print back in front of the shutdown fails here rather
    // than only in a PTY script.
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/boot.rs"),
    )
    .expect("boot.rs is readable");
    let failure = src
        .split("if !quiesced || assert_all_activated(&snapshot).is_err() {")
        .nth(1)
        .expect("the failure branch is still spelled this way");
    let branch = failure
        .split("return Ok(ExitCode::FAILURE);")
        .next()
        .expect("the branch ends with the failure return");
    let shutdown = branch
        .find("kernel.shutdown().await;")
        .expect("the failure branch tears down");
    let print = branch
        .find("eprint!(\"{}\", describe_unresolved(&snapshot));")
        .expect("the failure branch prints the report");
    assert!(
        shutdown < print,
        "the report must be printed AFTER the teardown, or the alt-screen restore wipes it"
    );

    // And the snapshot it prints is taken before the failure branch, so it is still readable
    // after the teardown that branch now runs first.
    let taken = src
        .find("let snapshot = kernel.snapshot();")
        .expect("the snapshot is taken");
    let branch_at = src
        .find("if !quiesced || assert_all_activated(&snapshot).is_err() {")
        .expect("the failure branch is still spelled this way");
    assert!(
        taken < branch_at,
        "the snapshot must be taken before the failure branch"
    );
}
