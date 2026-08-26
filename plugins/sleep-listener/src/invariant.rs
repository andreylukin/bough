//! §0.2 runtime invariant for `bough-plugin-sleep-listener`:
//!
//! **The row is ACTIVE on every platform.** A row that failed to activate because the platform is
//! not macOS is the violation, so the check's real content is "this fiber got as far as choosing a
//! source, and the source it chose is one this platform can have".
//!
//! `noop` is a legal choice ON macOS too: `enabled: false` is one, and so is an IOKit registration
//! that returned no port with `source: auto` (the row degrades and warns rather than refusing to
//! boot — §13 makes TUI-launch catch-up the reliable baseline anyway).

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

/// The source one activation chose.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub kind: &'static str,
}

static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// Record the chosen source. Called by `apply`, once.
pub fn record(obs: Obs) {
    let mut seen = SEEN.lock();
    seen.retain(|o| o.fiber != obs.fiber);
    seen.push(obs);
}

/// Forget this fiber's choice, as an inverse of `apply`.
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| o.fiber != fiber);
}

/// What every activation chose.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Drop the record. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
}

/// PURE: is this what an activation on this platform may look like?
pub fn check_choice(kind: Option<&str>, is_macos: bool) -> Result<(), String> {
    let Some(kind) = kind else {
        return Err(
            "the row never chose a source; an enabled row that does not activate is a boot \
             failure (§0.2)"
                .to_string(),
        );
    };
    match (kind, is_macos) {
        ("iokit" | "nsworkspace", true) => Ok(()),
        ("noop", _) => Ok(()),
        (k, true) => Err(format!("`{k}` is not a source this row knows")),
        (k, false) => Err(format!(
            "`{k}` cannot exist off macOS; the row must have chosen `noop`"
        )),
    }
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "the_row_is_active_with_a_source_this_platform_can_have",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let fiber = ctx.fiber_uid();
    let mine = seen();
    let kind = mine.iter().find(|o| o.fiber == fiber).map(|o| o.kind);
    check_choice(kind, cfg!(target_os = "macos")).map_err(|detail| InvariantViolation {
        invariant: "the_row_is_active_with_a_source_this_platform_can_have",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_that_never_activated_is_the_violation() {
        let detail = check_choice(None, true).expect_err("must be reported");
        assert!(detail.contains("boot failure"), "{detail}");
    }

    #[test]
    fn the_macos_sources_are_legal_on_macos_only() {
        assert_eq!(check_choice(Some("iokit"), true), Ok(()));
        assert_eq!(check_choice(Some("nsworkspace"), true), Ok(()));
        assert!(check_choice(Some("iokit"), false).is_err());
    }

    #[test]
    fn noop_is_legal_everywhere() {
        assert_eq!(check_choice(Some("noop"), true), Ok(()));
        assert_eq!(check_choice(Some("noop"), false), Ok(()));
    }

    #[test]
    fn a_source_this_row_does_not_know_is_reported() {
        assert!(check_choice(Some("test"), true).is_err());
    }

    #[test]
    fn a_reload_replaces_its_predecessors_choice() {
        clear();
        let core = bough_kernel::KernelCore::new();
        let f = core.new_fiber_uid();
        record(Obs {
            fiber: f,
            kind: "iokit",
        });
        record(Obs {
            fiber: f,
            kind: "noop",
        });
        let mine: Vec<Obs> = seen().into_iter().filter(|o| o.fiber == f).collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].kind, "noop");
        clear();
    }
}
