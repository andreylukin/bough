//! Invariant under test (V8): reading a service key the row never declared in `inject:` fails at
//! the POINT OF USE, and the error names both the key and the plugin. The check is against the
//! effective inject set, not against the store, so it fails identically whether or not anything
//! happens to be bound.
//!
//! This runs through the real catalog path — `hello` is mounted by the kernel from a bundle row —
//! because a fabricated context would prove nothing about how a plugin actually reads a key.

use std::time::Duration;

use bough_kernel::{Catalog, Composer, ExprEnv, FiberState, Kernel, KernelOptions, LayerId, Patch};
use bough_plugin_hello::trace;

const TREE: &str = "\
- id: greeting.provider
  plugin: greeting-echo
  config: { suffix: \"\" }
- id: hello.greeter
  plugin: hello
  config: { who: world, read_undeclared: ledger }
";

#[tokio::test]
async fn hello_reading_undeclared_key_names_key_and_plugin() {
    let _guard = trace::test_lock();

    let catalog = Catalog::from_inventory().expect("catalog");
    let patch: Patch = serde_yaml::from_str(TREE).expect("bundle parses");
    let mut composer = Composer::new(&catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    let composition = composer.compose().expect("bundle composes");

    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: false,
            reconcile_debounce: Duration::from_millis(0),
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;

    // The read failed, so `apply` returned Err and the fiber is FAILED — not PENDING: the key it
    // asked for is unrelated to the key it declared, so nothing was ever unresolved.
    let hello = kernel
        .snapshot()
        .rows
        .into_iter()
        .find(|r| r.id.as_str() == "hello.greeter")
        .expect("the row is in the tree");
    assert_eq!(hello.state, FiberState::Failed);

    // The message is normative (§2.11 of the Phase 0 plan), verbatim but for the row id.
    let errors = trace::errors();
    let detail = errors.first().expect("hello recorded the error it raised");
    assert_eq!(
        detail,
        "plugin `hello` (row `hello.greeter`) read service `ledger` without declaring it in inject",
    );

    kernel.shutdown().await;
}
