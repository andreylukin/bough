//! V7: a bad patch never disturbs what is running. A candidate tree that fails to compose leaves
//! the LAST GOOD TREE untouched and broadcasts `config-update-failed` (§0.3); a patch naming a row
//! id that no layer ever created is a WARNING, not an error (§0.2), and the rest of the patch
//! still applies.
//!
//! Misconfiguration must fail loud — and "loud" means an error a human can act on, not a dead
//! tree.

use crate::support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bough_kernel::{ComposeError, ComposeWarning, FiberState};
use bough_plugin_hello::trace;
use support::{boot_with, compose_layers, recompose, row, write_patch, BASE};

/// Count `config-update-failed` broadcasts for the life of the returned handle.
async fn count_failures(kernel: &bough_kernel::Kernel) -> Arc<AtomicUsize> {
    let seen = Arc::new(AtomicUsize::new(0));
    let sink = seen.clone();
    kernel
        .root()
        .on::<bough_kernel::event::ConfigUpdateFailed, _, _>(move |_| {
            let sink = sink.clone();
            async move {
                sink.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await
        .expect("the root context accepts a listener");
    seen
}

#[tokio::test]
async fn invalid_config_leaves_last_good_tree_and_broadcasts_failure() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_with(BASE).await;
    let failures = count_failures(&kernel).await;

    let good = kernel.snapshot();
    let hello_uid = row(&kernel, "hello.greeter").uid.expect("uid");

    // `who` is required and has no default: this candidate cannot be parsed by hello's schema.
    // `config:` REPLACES the row's config wholesale (§0.5), so the field is genuinely gone.
    write_patch(
        &dir,
        "\
entries:
  hello.greeter:
    config: { log_level: debug }
",
    );
    let err = recompose(&kernel, BASE, &dir)
        .await
        .expect_err("a config the plugin's schema rejects must fail the candidate");
    match &err {
        ComposeError::BadConfig { entry, plugin, .. } => {
            assert_eq!(entry.as_str(), "hello.greeter");
            assert_eq!(plugin, "hello");
        }
        other => panic!("expected BadConfig naming the row, got: {other}"),
    }

    // The last good tree is still running, unchanged.
    let now = kernel.snapshot();
    assert_eq!(now.fingerprint, good.fingerprint);
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);
    assert_eq!(row(&kernel, "hello.greeter").uid.expect("uid"), hello_uid);
    assert_eq!(
        failures.load(Ordering::SeqCst),
        1,
        "the rejection must be broadcast, not swallowed"
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn unknown_plugin_name_leaves_last_good_tree() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_with(BASE).await;
    let good = kernel.snapshot();

    write_patch(
        &dir,
        "\
entries:
  greeting.provider:
    plugin: greeting-whisper
",
    );
    let err = recompose(&kernel, BASE, &dir)
        .await
        .expect_err("a plugin that is not in the catalog must fail the candidate");
    match &err {
        ComposeError::UnknownPlugin { entry, plugin, .. } => {
            assert_eq!(entry.as_str(), "greeting.provider");
            assert_eq!(plugin, "greeting-whisper");
        }
        other => panic!("expected UnknownPlugin naming the row, got: {other}"),
    }

    assert_eq!(kernel.snapshot().fingerprint, good.fingerprint);
    assert_eq!(
        row(&kernel, "greeting.provider").plugin.as_deref(),
        Some("greeting-echo"),
        "the running row must be the one that was there before the bad patch"
    );
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);

    kernel.shutdown().await;
}

#[tokio::test]
async fn patch_naming_absent_row_id_is_a_warning_and_the_tree_still_updates() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_with(BASE).await;
    let before = kernel.snapshot().fingerprint;

    // One row that exists and one that never did, in the same layer.
    write_patch(
        &dir,
        "\
entries:
  no.such.row:
    config: { anything: true }
  hello.greeter:
    config: { who: phase-zero }
",
    );

    // The warning is on the composition, so this test composes directly rather than through the
    // Result-shaped `recompose` helper.
    let catalog = bough_kernel::Catalog::from_inventory().expect("catalog");
    let candidate = compose_layers(&catalog, BASE, &dir)
        .expect("an absent row id is a warning, never an error");
    assert!(
        candidate.warnings.iter().any(|w| match w {
            ComposeWarning::AbsentRowId { id, .. } => id.as_str() == "no.such.row",
        }),
        "the absent row id must be reported as a warning: {:?}",
        candidate.warnings
    );
    drop(candidate);

    // And the rest of the patch takes effect.
    recompose(&kernel, BASE, &dir)
        .await
        .expect("the tree still updates");
    assert_ne!(kernel.snapshot().fingerprint, before);
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);

    kernel.shutdown().await;
}
