//! The panel phase's SWAP gate (§17, AGENTS.md): a row toggled off FROM THE PANEL'S OWN WRITE
//! PATH — the `ui` patch layer plus the launcher's recompose — rests INACTIVE, its dependent
//! parks PENDING, a bystander's fiber never moves, and toggling back is a LOAD in the SAME
//! fiber (a new uid would mean the panel wrote `plugin:` or `remove:` instead of `disabled:`).
//! No compile, no restart. The write goes through `bough_plugin_tui_panel::store`, the same
//! functions the `x` key calls, so this gate exercises the panel's real document and not a
//! hand-written imitation of it.

use crate::support;

use bough_kernel::FiberState;
use bough_plugin_hello::trace;
use bough_plugin_tui_panel::store;
use support::{boot_with, recompose, row, BASE};

/// A row nothing depends on, in its own realm, so it can neither satisfy `hello` nor be
/// satisfied by anything the toggle touches (the swap.rs bystander pattern).
const WITH_BYSTANDER: &str = "\
- id: bystander
  plugin: greeting-echo
  config: { suffix: \"-bystander\" }
  isolate: { greeting: bystander }
";

/// The panel's toggle, exactly as the `x` key performs it: read the file, apply the rule,
/// write-then-rename. `$BOUGH_HOME` is set by `boot_with`, so `ui_patch_path` points here.
fn panel_toggle(id: &str, effective_disabled: bool, known: &[&str]) {
    let path = bough_util::ui_patch_path();
    let known: std::collections::BTreeSet<String> = known.iter().map(|s| s.to_string()).collect();
    let entries = store::read(&path).expect("the ui layer parses");
    let next = store::toggled(&entries, id, effective_disabled, &known);
    store::write(&path, &next).expect("the ui layer writes");
}

const KNOWN: &[&str] = &["greeting.provider", "hello.greeter", "bystander"];

#[tokio::test]
async fn a_panel_toggle_disables_through_the_ui_layer_and_reenabling_keeps_the_fiber() {
    let _guard = trace::test_lock();
    let bundle = format!("{BASE}{WITH_BYSTANDER}");
    let (kernel, dir) = boot_with(&bundle).await;

    let provider_before = row(&kernel, "greeting.provider").uid.expect("uid");
    let bystander_before = row(&kernel, "bystander").uid.expect("uid");
    let fingerprint_before = kernel.snapshot().fingerprint;

    // x on an enabled row: the panel pins it off.
    panel_toggle("greeting.provider", false, KNOWN);
    assert!(bough_util::ui_patch_path().is_file());
    recompose(&kernel, &bundle, &dir)
        .await
        .expect("a panel toggle composes");

    let provider = row(&kernel, "greeting.provider");
    assert_eq!(provider.state, FiberState::Inactive);
    assert!(provider.disabled);
    // The dependent PARKS; it is not failed and not torn down (swap.rs:212 semantics).
    let hello = row(&kernel, "hello.greeter");
    assert_eq!(hello.state, FiberState::Pending);
    assert_eq!(hello.unmet, vec!["greeting".to_string()]);
    assert_eq!(
        row(&kernel, "bystander").uid.expect("uid"),
        bystander_before,
        "an unrelated row must not be disturbed by the toggle"
    );
    assert_ne!(
        kernel.snapshot().fingerprint,
        fingerprint_before,
        "a toggle must move the composition fingerprint"
    );
    // The provenance names the panel's layer: the column that explains "who turned this off".
    let comp = kernel.composition().expect("a composition is loaded");
    let disabled_by = comp
        .provenance
        .get(&bough_kernel::EntryId::new("greeting.provider"))
        .and_then(|p| p.fields.get("disabled"))
        .map(|l| l.to_string());
    assert_eq!(disabled_by.as_deref(), Some("ui"));

    // x again: the panel withdraws its pin; the row returns as a LOAD in the SAME fiber.
    panel_toggle("greeting.provider", true, KNOWN);
    assert!(
        !bough_util::ui_patch_path().exists(),
        "withdrawing the one pin must remove the file: the layer is a diff"
    );
    recompose(&kernel, &bundle, &dir)
        .await
        .expect("withdrawing the toggle composes");

    let provider_after = row(&kernel, "greeting.provider");
    assert_eq!(provider_after.state, FiberState::Active);
    assert_eq!(
        provider_after.uid.expect("uid"),
        provider_before,
        "re-enabling is a Load, not a Create: `disabled:` keeps the fiber"
    );
    assert_eq!(row(&kernel, "hello.greeter").state, FiberState::Active);

    kernel.shutdown().await;
}
