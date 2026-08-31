//! Invariant under test: activation is SERVICE-DRIVEN (§0.2, §0.3). `hello` is PENDING until an
//! ACTIVE fiber provides `greeting`, unloads when that binding withdraws, and RELOADS when a
//! different fiber provides an equal value — because a dependent targets a binding identity, never
//! a value. Everything here is asserted against the fixture's ordered trace and the kernel's
//! structural snapshot, never a rendered string (AGENTS.md).
//!
//! Covers V1, V2 and the hello-side half of V3.

use std::sync::Arc;

use bough_kernel::{
    Catalog, Composer, Composition, ExprEnv, FiberState, Kernel, KernelOptions, LayerId, Patch,
    RowSnapshot,
};
use bough_plugin_hello::{trace, GreetingHandle, GreetingSink};

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

const PROVIDER: &str = "\
- id: greeting.provider
  plugin: greeting-echo
  config: { suffix: \"\" }
";

const CONSUMER: &str = "\
- id: hello.greeter
  plugin: hello
  config: { who: world }
";

fn compose(catalog: &Catalog, yaml: &str) -> Composition {
    let patch: Patch = serde_yaml::from_str(yaml).expect("test bundle parses");
    let mut composer = Composer::new(catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    composer.compose().expect("test bundle composes")
}

async fn boot(yaml: &str) -> Arc<Kernel> {
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let composition = compose(&catalog, yaml);
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: true,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;
    kernel
}

async fn update(kernel: &Kernel, yaml: &str) {
    let catalog = Catalog::from_inventory().expect("catalog");
    let composition = compose(&catalog, yaml);
    kernel.update(composition).await.expect("the tree updates");
    kernel.quiesce().await;
}

fn row(kernel: &Kernel, id: &str) -> RowSnapshot {
    fn find(rows: &[RowSnapshot], id: &str) -> Option<RowSnapshot> {
        for r in rows {
            if r.id.as_str() == id {
                return Some(r.clone());
            }
            if let Some(found) = find(&r.children, id) {
                return Some(found);
            }
        }
        None
    }
    let snapshot = kernel.snapshot();
    find(&snapshot.rows, id).unwrap_or_else(|| panic!("no row `{id}` in the tree"))
}

fn maybe_row(kernel: &Kernel, id: &str) -> Option<RowSnapshot> {
    kernel
        .snapshot()
        .rows
        .iter()
        .find(|r| r.id.as_str() == id)
        .cloned()
}

/// How many times `hello::apply` has run so far.
fn applies() -> usize {
    trace::global()
        .lines()
        .iter()
        .filter(|l| **l == ("hello", "apply"))
        .count()
}

/// Assert `first` appears in the trace strictly before `second`.
#[track_caller]
fn strictly_before(first: (&'static str, &'static str), second: (&'static str, &'static str)) {
    let t = trace::global();
    let lines = t.lines();
    let a = t
        .position(first)
        .unwrap_or_else(|| panic!("{first:?} never happened; trace: {lines:?}"));
    let b = t
        .position(second)
        .unwrap_or_else(|| panic!("{second:?} never happened; trace: {lines:?}"));
    assert!(a < b, "{first:?} must precede {second:?}; trace: {lines:?}");
}

// ---------------------------------------------------------------------------
// V1 — activation is service-driven
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hello_stays_pending_until_greeting_is_provided_by_an_active_fiber() {
    let _guard = trace::test_lock();
    let kernel = boot(CONSUMER).await;

    let hello = row(&kernel, "hello.greeter");
    assert_eq!(hello.state, FiberState::Pending);
    assert_eq!(hello.unmet, vec!["greeting".to_string()]);
    assert!(
        trace::global().position(("hello", "apply")).is_none(),
        "apply must not run while a required key is unresolved: {:?}",
        trace::global().lines()
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn hello_activates_when_the_provider_activates() {
    let _guard = trace::test_lock();
    let kernel = boot(CONSUMER).await;
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Pending);

    update(&kernel, &format!("{PROVIDER}{CONSUMER}")).await;

    let hello = row(&kernel, "hello.greeter");
    assert_eq!(hello.state, FiberState::Active);
    assert!(hello.unmet.is_empty());
    // The provider had to be ACTIVE first: nothing else could have satisfied the key.
    strictly_before(("greeting-echo", "apply"), ("hello", "apply"));
    // And hello bound against that provider.
    assert!(trace::global()
        .position(("hello", "greeting-echo"))
        .is_some());

    kernel.shutdown().await;
}

// ---------------------------------------------------------------------------
// V2 — withdraw, reload on a different fiber, in-place set is invisible
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hello_unloads_when_the_provider_withdraws() {
    let _guard = trace::test_lock();
    let kernel = boot(&format!("{PROVIDER}{CONSUMER}")).await;
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);

    // Drop the provider row entirely: the binding withdraws.
    update(&kernel, CONSUMER).await;

    let hello = row(&kernel, "hello.greeter");
    assert_eq!(hello.state, FiberState::Pending);
    assert_eq!(hello.unmet, vec!["greeting".to_string()]);
    // §0.3's mandated order: the dependent tears down before the provider unwinds its own effects.
    strictly_before(("hello", "unload"), ("greeting-echo", "unload"));

    kernel.shutdown().await;
}

#[tokio::test]
async fn hello_reloads_when_a_different_fiber_provides_an_equal_value() {
    let _guard = trace::test_lock();
    let kernel = boot(&format!("{PROVIDER}{CONSUMER}")).await;
    let applies_before = applies();

    // Same key, same greeting text, different plugin — so only the binding identity moves.
    let swapped = "\
- id: greeting.provider
  plugin: greeting-shout
  config: { suffix: \"\" }
";
    update(&kernel, &format!("{swapped}{CONSUMER}")).await;

    let hello = row(&kernel, "hello.greeter");
    assert_eq!(hello.state, FiberState::Active);
    // A reload is UNLOADING then LOADING, so `apply` ran a second time. Asserted on the trace
    // rather than on the FiberUid: §2.7 says a new uid marks a REBUILD (an id or plugin change),
    // while §4's SWAP sketch says a reload moves it too — the trace is unambiguous either way.
    assert_eq!(
        applies(),
        applies_before + 1,
        "the dependent must have reloaded; trace: {:?}",
        trace::global().lines()
    );
    assert!(trace::global()
        .position(("hello", "greeting-shout"))
        .is_some());
    strictly_before(("hello", "unload"), ("hello", "greeting-shout"));

    kernel.shutdown().await;
}

#[tokio::test]
async fn provider_in_place_set_is_not_observed_by_hello() {
    let _guard = trace::test_lock();
    let kernel = boot(&format!("{PROVIDER}{CONSUMER}")).await;
    let before = row(&kernel, "hello.greeter").uid.expect("uid");
    let applies_before = applies();

    // Overwrite the value in place. Same ProviderUid ⇒ nobody recomputes (§0.3).
    struct Loud;
    impl GreetingSink for Loud {
        fn greet(&self, who: &str) -> String {
            format!("HELLO, {who}!!")
        }
        fn provider(&self) -> &'static str {
            "greeting-echo"
        }
    }
    let slot = bough_plugin_hello::provider::last_slot().expect("the provider remembered its slot");
    slot.set(GreetingHandle(Arc::new(Loud)));
    kernel.quiesce().await;

    let after = row(&kernel, "hello.greeter");
    assert_eq!(after.state, FiberState::Active);
    assert_eq!(
        after.uid.expect("uid"),
        before,
        "an in-place set must not reload"
    );
    assert_eq!(
        applies(),
        applies_before,
        "apply must not have run again; trace: {:?}",
        trace::global().lines()
    );

    kernel.shutdown().await;
}

// ---------------------------------------------------------------------------
// V3 — LIFO unwind, cascading nested mounts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hello_effects_unwind_lifo_on_unload() {
    let _guard = trace::test_lock();
    let kernel = boot(&format!("{PROVIDER}{CONSUMER}")).await;
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);
    trace::global().clear();

    kernel.shutdown().await;

    // hello registered, in order: the unload marker, then effect-1, effect-2, effect-3. Effects
    // unwind LIFO within the fiber, so the marker it registered FIRST runs LAST.
    let hello_lines: Vec<&'static str> = trace::global()
        .lines()
        .into_iter()
        .filter(|(p, _)| *p == "hello")
        .map(|(_, m)| m)
        .collect();
    assert_eq!(
        hello_lines,
        vec!["effect-3", "effect-2", "effect-1", "unload"],
        "hello's inverses must unwind LIFO"
    );
}

#[tokio::test]
async fn unloading_a_parent_cascades_to_nested_mounts() {
    let _guard = trace::test_lock();
    let nested = "\
- id: hello.greeter
  plugin: hello
  config: { who: world, mount_child: true }
";
    let kernel = boot(&format!("{PROVIDER}{nested}")).await;
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);
    // The child mounted from `apply` is in the tree, under its parent.
    let child = row(&kernel, "hello.greeter.child");
    assert_eq!(child.state, FiberState::Active);
    trace::global().clear();

    // Drop the parent row. Children are effects of the parent, so the cascade is automatic.
    update(&kernel, PROVIDER).await;

    assert!(maybe_row(&kernel, "hello.greeter").is_none());
    assert!(
        trace::global()
            .position(("greeting-shout", "unload"))
            .is_some(),
        "the nested mount must have been unloaded with its parent; trace: {:?}",
        trace::global().lines()
    );
    strictly_before(("greeting-shout", "unload"), ("hello", "unload"));

    kernel.shutdown().await;
}
