//! The store against a real filesystem: the atomic-rename discipline and the reset path, under
//! a TempDir the way the launcher's own layer tests hold their $BOUGH_HOME.

use std::collections::BTreeSet;

use bough_plugin_tui_panel::store;

fn ids(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_full_toggle_cycle_leaves_no_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bough.ui.patch.yml");
    let known = ids(&["old-feed", "tui.search"]);

    // Pin old-feed on (it ships disabled).
    let e = store::read(&path).unwrap();
    let e = store::toggled(&e, "old-feed", true, &known);
    store::write(&path, &e).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("old-feed: { disabled: false }"), "{text}");

    // Withdraw the pin: the diff is empty again and the file is GONE — reset is deletion.
    let e = store::read(&path).unwrap();
    let e = store::toggled(&e, "old-feed", false, &known);
    store::write(&path, &e).unwrap();
    assert!(!path.exists());
}

#[test]
fn a_hand_edited_file_is_refused_and_left_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bough.ui.patch.yml");
    let foreign = "entries:\n  a: { disabled: true }\ninsert: []\n";
    std::fs::write(&path, foreign).unwrap();
    let err = store::read(&path).expect_err("foreign content");
    assert!(matches!(err, store::StoreError::Foreign { .. }), "{err}");
    // Refused means UNTOUCHED: the panel reports, the human decides.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), foreign);
}
