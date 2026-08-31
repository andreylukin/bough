//! §0.2 runtime invariant for `bough-plugin-tui-title`:
//!
//! **The last title written names the lane it was written for, and no byte of it can escape the
//! OSC string.** The tab is chrome read from ACROSS the room — a title naming a lane the keyboard
//! left is the same lie as a spinner on an idle turn, and a control byte in it is worse: the
//! terminal would execute the remainder as input.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

use crate::sanitize;

/// What the row last wrote: the lane it derived the title from, and the title itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    pub lane: Option<String>,
    pub title: String,
}

static LAST: Mutex<Option<Written>> = Mutex::new(None);

/// Record what is about to be written. Returns `false` when it matches the previous write, in
/// which case the caller skips the terminal: `tui/focus` fires for pane hops that move no lane,
/// and an attach client should not receive a title byte per keystroke.
pub fn record(lane: Option<String>, title: String) -> bool {
    let next = Written { lane, title };
    let mut last = LAST.lock();
    if last.as_ref() == Some(&next) {
        return false;
    }
    *last = Some(next);
    true
}

/// The last write, for the check and for tests.
pub fn last() -> Option<Written> {
    LAST.lock().clone()
}

/// Forget it. The record is per-process and this row owns it: unloading must leave the tree as if
/// the row had never mounted (§0.2), so a reload is never deduped against its predecessor's write.
pub fn forget() {
    *LAST.lock() = None;
}

/// PURE: the check, over what the row last wrote.
pub fn check_written(w: &Written) -> Result<(), String> {
    if w.title.chars().any(|c| c.is_control()) {
        return Err(format!(
            "the written title {:?} holds a control character: the terminal would end the OSC \
             string there and take the rest as input",
            w.title
        ));
    }
    if let Some(lane) = &w.lane {
        let name = sanitize(lane);
        if !w.title.contains(&name) {
            return Err(format!(
                "the written title {:?} does not name the focused lane {name:?}: `validate` \
                 requires `{{lane}}` in the format precisely so this cannot happen",
                w.title
            ));
        }
    }
    Ok(())
}

/// The specs this row contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "the_tab_title_names_the_focused_lane_and_never_escapes_its_sequence",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let Some(written) = last() else {
        // Nothing written yet is not a violation: the row records on its very first refresh, but
        // the check may run between activation and it.
        return Ok(());
    };
    check_written(&written).map_err(|detail| InvariantViolation {
        invariant: "the_tab_title_names_the_focused_lane_and_never_escapes_its_sequence",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_naming_the_lane_holds() {
        check_written(&Written {
            lane: Some("sol".into()),
            title: "sol · bough".into(),
        })
        .unwrap();
    }

    #[test]
    fn a_title_missing_the_lane_is_a_violation() {
        let err = check_written(&Written {
            lane: Some("sol".into()),
            title: "terra · bough".into(),
        })
        .expect_err("the tab named a lane the keyboard left");
        assert!(err.contains("sol"), "the violation names the lane: {err}");
    }

    #[test]
    fn a_control_byte_in_the_title_is_a_violation() {
        assert!(check_written(&Written {
            lane: None,
            title: "bough\x1b".into(),
        })
        .is_err());
    }

    #[test]
    fn an_identical_write_is_deduped_and_a_moved_one_is_not() {
        forget();
        assert!(record(Some("sol".into()), "sol".into()));
        assert!(!record(Some("sol".into()), "sol".into()));
        assert!(record(Some("terra".into()), "terra".into()));
        forget();
        assert!(last().is_none());
    }
}
