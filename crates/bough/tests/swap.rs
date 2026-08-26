//! The Phase 0 exit gate (§17, SWAP). A live patch edit replaces the provider row with a second
//! plugin providing the same key: `hello` reloads against it, the old provider leaks nothing,
//! `--dump-config` reflects it — and a second edit disabling the provider leaves `hello` PENDING
//! with nothing else disturbed. No recompile, no restart.
//!
//! The two providers emit EQUAL greeting text, so nothing here can be explained by the value
//! changing; what moves is the binding identity (§0.3).

mod support;

use bough_kernel::FiberState;
use bough_plugin_hello::{trace, Greeting};
use support::{boot_with, recompose, row, write_patch, BASE};

/// The user patch that performs the swap.
const SWAP: &str = "\
entries:
  greeting.provider:
    plugin: greeting-shout
";

/// A row nothing depends on, so the edit has a bystander whose fiber must not move. It is a
/// second provider in its OWN realm, so it cannot satisfy `hello` and cannot be satisfied by
/// anything that changes here.
const WITH_BYSTANDER: &str = "\
- id: bystander
  plugin: greeting-echo
  config: { suffix: \"-bystander\" }
  isolate: { greeting: bystander }
";

#[tokio::test]
async fn patch_swaps_the_provider_row_and_hello_reloads_against_it() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_with(BASE).await;

    let hello_before = row(&kernel, "hello.greeter");
    assert_eq!(hello_before.state, FiberState::Active);
    assert!(trace::global()
        .position(("hello", "greeting-echo"))
        .is_some());
    let fingerprint_before = kernel.snapshot().fingerprint;
    trace::global().clear();

    write_patch(&dir, SWAP);
    recompose(&kernel, BASE, &dir)
        .await
        .expect("the swap composes");

    let hello_after = row(&kernel, "hello.greeter");
    assert_eq!(hello_after.state, FiberState::Active);
    // A RELOAD keeps the fiber: REQUIREMENTS §0.3 makes a new uid the mark of a REBUILD, and only
    // a `plugin`/`id` change rebuilds. `hello.greeter`'s own row did not change here — its target
    // did — so it goes round the lifecycle again in the same fiber. The reload itself is asserted
    // below, on the trace: the old provider unloaded and `hello::apply` ran again against the new
    // one.
    assert_eq!(
        hello_after.uid.expect("uid"),
        hello_before.uid.expect("uid"),
        "a reload is the same fiber; only a plugin/id change rebuilds"
    );
    assert_eq!(
        row(&kernel, "greeting.provider").plugin.as_deref(),
        Some("greeting-shout")
    );
    // hello bound against the shout fiber this time.
    assert!(trace::global()
        .position(("hello", "greeting-shout"))
        .is_some());
    // The outgoing provider stopped providing before the new activation ran.
    let lines = trace::global().lines();
    let echo_unload = trace::global()
        .position(("greeting-echo", "unload"))
        .unwrap_or_else(|| panic!("the echo provider never unloaded; trace: {lines:?}"));
    let hello_apply = trace::global()
        .position(("hello", "apply"))
        .unwrap_or_else(|| panic!("hello never re-applied; trace: {lines:?}"));
    assert!(
        echo_unload < hello_apply,
        "the old provider must be gone before the dependent re-applies; trace: {lines:?}"
    );
    assert_ne!(
        kernel.snapshot().fingerprint,
        fingerprint_before,
        "a swapped row must move the composition fingerprint"
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn swapped_out_provider_leaves_no_listeners_and_no_bindings() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_with(BASE).await;
    let echo_uid = row(&kernel, "greeting.provider").uid.expect("uid");

    write_patch(&dir, SWAP);
    recompose(&kernel, BASE, &dir)
        .await
        .expect("the swap composes");
    let after_swap = trace::global().lines().len();

    // Exactly one row provides `greeting`, and it is not the fiber that was swapped out.
    let snapshot = kernel.snapshot();
    let providers: Vec<_> = snapshot
        .rows
        .iter()
        .filter(|r| r.provides.contains(&"greeting"))
        .collect();
    assert_eq!(providers.len(), 1, "exactly one live greeting binding");
    assert_ne!(providers[0].uid.expect("uid"), echo_uid);

    // The live store agrees: the binding behind `greeting` belongs to the shout plugin.
    let live = kernel
        .root()
        .peek_live::<Greeting>()
        .expect("greeting is bound");
    assert_eq!(live.0.provider(), "greeting-shout");

    // Nothing owned by the retired fiber runs any more. The kernel exposes no listener registry
    // to enumerate, so what is asserted is the observable consequence: after the swap settled,
    // the echo plugin contributes no further line to the trace, through a further recompose.
    recompose(&kernel, BASE, &dir)
        .await
        .expect("a no-op recompose");
    let tail = &trace::global().lines()[after_swap..];
    assert!(
        !tail.iter().any(|(p, _)| *p == "greeting-echo"),
        "the swapped-out provider is still doing work: {tail:?}"
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn dump_config_reflects_the_swapped_row() {
    let _guard = trace::test_lock();
    // The dump must come from the real binary and the real embedded bundles: the whole claim of
    // V6 is that the dump is what boots, and an in-process re-composition could not show that.
    let dir = support::TempDir::new("dump-swap");
    write_patch(&dir, SWAP);

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bough"))
        .args(["--profile", "tui", "--dump-config"])
        .env("BOUGH_HOME", dir.path())
        .output()
        .expect("the launcher runs");
    assert!(
        out.status.success(),
        "--dump-config must exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dump = String::from_utf8(out.stdout).expect("the dump is utf-8");
    assert!(
        dump.contains("greeting-shout"),
        "the dump must show the patched plugin:\n{dump}"
    );
    assert!(
        !dump.contains("greeting-echo"),
        "the dump must not still show the replaced plugin:\n{dump}"
    );
    // And it must say which layer wrote it.
    assert!(
        dump.contains("user"),
        "the dump must annotate the layer:\n{dump}"
    );
}

#[tokio::test]
async fn disabling_the_provider_leaves_hello_pending_and_the_rest_unchanged() {
    let _guard = trace::test_lock();
    let bundle = format!("{BASE}{WITH_BYSTANDER}");
    let (kernel, dir) = boot_with(&bundle).await;
    let bystander_before = row(&kernel, "bystander").uid.expect("uid");

    write_patch(
        &dir,
        "\
entries:
  greeting.provider:
    disabled: true
",
    );
    recompose(&kernel, &bundle, &dir)
        .await
        .expect("disabling a row composes");

    let provider = row(&kernel, "greeting.provider");
    assert_eq!(provider.state, FiberState::Inactive);
    assert!(provider.disabled);

    let hello = row(&kernel, "hello.greeter");
    assert_eq!(hello.state, FiberState::Pending);
    assert_eq!(hello.unmet, vec!["greeting".to_string()]);

    assert_eq!(
        row(&kernel, "bystander").uid.expect("uid"),
        bystander_before,
        "an unrelated row must not be disturbed by the edit"
    );

    kernel.shutdown().await;
}
