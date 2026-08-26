//! V9: the invariant runner reports a planted violation under the `dev` profile and is not
//! created at all under `tui`. It REPORTS and never acts — the violating row keeps running, so a
//! false positive can never take a tree down (§0.2).
//!
//! The violation is real, not simulated: `plant_violation: true` makes `hello` emit a second
//! `hello/greeted` payload carrying the seq it already used, which is exactly what
//! `plugins/hello/src/invariant.rs` polices.

mod support;

use bough_kernel::FiberState;
use bough_plugin_hello::trace;
use support::{boot_with_profile, row};

/// The base composition with the violation planted in the consumer row.
const PLANTED: &str = "\
- id: greeting.provider
  plugin: greeting-echo
  config: { suffix: \"\" }
- id: hello.greeter
  plugin: hello
  config:
    who: world
    plant_violation: true
";

#[tokio::test]
async fn planted_violation_is_reported_in_the_dev_profile() {
    let _guard = trace::test_lock();
    assert!(
        support::profile_runs_invariants("dev"),
        "profiles/dev.yml must turn the runner on, or this test proves nothing"
    );

    let (kernel, _dir) = boot_with_profile(PLANTED, "dev").await;

    // Precondition: the stream the invariant polices was actually produced, and it does violate.
    // Without this, a green `violations()` and a fixture that emitted nothing look the same.
    let stream = bough_plugin_hello::invariant::seen();
    assert!(
        stream.len() >= 2,
        "the fixture must have emitted hello/greeted: {stream:?}"
    );
    assert!(
        bough_plugin_hello::invariant::evaluate(&stream).is_err(),
        "the planted stream must itself violate the invariant: {stream:?}"
    );

    let violations = kernel.violations();
    let v = violations
        .iter()
        .find(|v| v.invariant == "greeted_seq_is_monotonic")
        .unwrap_or_else(|| panic!("the planted violation was not reported: {violations:?}"));
    assert_eq!(v.plugin, "hello");
    assert_eq!(v.entry.as_str(), "hello.greeter");
    assert!(
        v.detail.contains("strictly increasing"),
        "the report must state the invariant: {}",
        v.detail
    );

    // A report, never an unload.
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);

    kernel.shutdown().await;
}

#[tokio::test]
async fn invariant_runner_is_silent_in_the_tui_profile() {
    let _guard = trace::test_lock();
    assert!(
        !support::profile_runs_invariants("tui"),
        "profiles/tui.yml must leave the runner off, or this test proves nothing"
    );

    // The very same tree, with the very same planted violation.
    let (kernel, _dir) = boot_with_profile(PLANTED, "tui").await;

    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);
    assert!(
        kernel.violations().is_empty(),
        "the runner is not created under `tui`, so nothing can be recorded: {:?}",
        kernel.violations()
    );

    kernel.shutdown().await;
}

/// Negative control: the same `dev` profile, the same runner, an unplanted tree. Without this,
/// a runner that reports unconditionally would pass the two tests above.
#[tokio::test]
async fn a_clean_tree_reports_nothing_in_the_dev_profile() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_with_profile(support::BASE, "dev").await;

    let stream = bough_plugin_hello::invariant::seen();
    assert!(
        stream.len() >= 2,
        "the fixture must still have emitted hello/greeted: {stream:?}"
    );
    assert!(
        bough_plugin_hello::invariant::evaluate(&stream).is_ok(),
        "the unplanted stream must satisfy the invariant: {stream:?}"
    );
    assert!(
        kernel.violations().is_empty(),
        "the runner reported on a clean tree: {:?}",
        kernel.violations()
    );

    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);
    kernel.shutdown().await;
}
