//! §0.2 runtime invariant for `bough-plugin-tui-panel`:
//!
//! **Every document the panel writes to the ui layer parses as the one shape (`entries:
//! { <row-id>: { disabled: <bool> } }`), and every id it names was in the composed tree at
//! write time.** The panel is the only writer of that file (a human edit is the user patch's
//! business, one layer down), so a document violating this is the panel lying to the composer —
//! and the composer would only ever tell the user with a warning at the NEXT recompose, which
//! is too late for a surface whose whole claim is that it shows what is true now.
//!
//! The recorder is a crate-level slot rather than a service binding, for the same reason
//! `tui-preview`'s is: `toggle` is synchronous, and the check must not have to reach back into
//! a pane that may already be disposed.

use std::collections::BTreeSet;
use std::path::Path;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

/// The invariant's name, as a violation reports it.
pub const NAME: &str = "every_written_ui_patch_is_disabled_only_and_names_known_rows";

static LAST: Mutex<Option<(String, BTreeSet<String>)>> = Mutex::new(None);

/// What the last write put on disk, and the row ids the composed tree held at that moment.
/// Recorded from `PanelPane::toggle`; allocation-only, no I/O.
pub fn record(doc: String, known_ids: BTreeSet<String>) {
    *LAST.lock() = Some((doc, known_ids));
}

/// The recorded write, if there is one.
pub fn written() -> Option<(String, BTreeSet<String>)> {
    LAST.lock().clone()
}

/// Forget the recorded write. The row's disposal path: a disabled row leaves nothing behind.
pub fn forget() {
    *LAST.lock() = None;
}

/// PURE: the check over one recorded write.
pub fn check_write(written: Option<(&str, &BTreeSet<String>)>) -> Result<(), String> {
    let Some((doc, known)) = written else {
        return Ok(());
    };
    let entries = crate::store::parse(Path::new("(recorded write)"), Some(doc))
        .map_err(|e| format!("the panel wrote a document it would itself refuse: {e}"))?;
    for id in entries.keys() {
        if !known.contains(id) {
            return Err(format!(
                "the panel wrote `{id}`, which was not in the composed tree at write time"
            ));
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let held = written();
    check_write(held.as_ref().map(|(d, k)| (d.as_str(), k))).map_err(|detail| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_write_yet_is_not_a_violation() {
        assert!(check_write(None).is_ok());
    }

    #[test]
    fn the_one_shape_over_known_ids_passes() {
        let k = known(&["old-feed"]);
        assert!(check_write(Some(("entries:\n  old-feed: { disabled: true }\n", &k))).is_ok());
    }

    #[test]
    fn a_document_carrying_more_than_disabled_is_reported() {
        let k = known(&["a"]);
        let err = check_write(Some((
            "entries:\n  a: { disabled: true, config: {} }\n",
            &k,
        )))
        .expect_err("more than disabled");
        assert!(err.contains("refuse"), "{err}");
    }

    #[test]
    fn an_unknown_id_is_reported() {
        let k = known(&["a"]);
        let err = check_write(Some(("entries:\n  gone: { disabled: true }\n", &k)))
            .expect_err("unknown id");
        assert!(err.contains("gone"), "{err}");
    }

    #[test]
    fn the_recorded_write_is_forgotten_on_disposal() {
        record("entries:\n".into(), known(&[]));
        assert!(written().is_some());
        forget();
        assert!(written().is_none());
    }
}
