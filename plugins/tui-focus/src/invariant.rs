//! §0.2 runtime invariant for `bough-plugin-tui-focus`:
//!
//! **No step is rendered twice: the live tail and the durable rows never overlap.** The tee and
//! the `ledger/step` listener race by construction, so the one thing that must hold over time is
//! that their outputs are disjoint at every frame (P3-D12).
//!
//! The recorder is a process-wide slot holding what the LAST frame drew. `render` is synchronous
//! and cannot await a check, and the property is about what reached the screen.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

use crate::rows::trailing_durable;
use crate::{LiveText, Row};

static LAST_FRAME: Mutex<Option<(Vec<Row>, LiveText)>> = Mutex::new(None);

/// Record what this frame drew. Called from `FocusPane::render`.
pub fn record_frame(rows: &[Row], live: &LiveText) {
    *LAST_FRAME.lock() = Some((rows.to_vec(), live.clone()));
}

/// Forget the recorded frame. The row's disposal path: registrations are effects, and a disabled
/// row must leave nothing behind (§0.2) — including the frame its last render recorded.
pub fn forget() {
    *LAST_FRAME.lock() = None;
}

/// What the last frame drew, for the check and for tests.
pub fn last_frame() -> Option<(Vec<Row>, LiveText)> {
    LAST_FRAME.lock().clone()
}

/// PURE: the check, over the rows and the live tail of one frame.
///
/// Two ways a step could reach the screen twice, and both are checked:
///
/// 1. **Two rows for one step.** The `tool/call` + `tool/result` fold is the only place two steps
///    become one row; nothing may make one step into two rows.
/// 2. **The tail overlapping the durable text.** `trailing_text` renders exactly one of the two,
///    chosen by length — which is only safe while one is a prefix of the other. If they diverged,
///    whichever was picked would be showing text the other half had already shown.
pub fn check_frame(rows: &[Row], live: &LiveText) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        // Every FOLDED step, not only the anchor: since P5-D14 a joined `Text` row carries the
        // whole group, and a step that reached two groups would be on screen twice with two
        // different anchors to point at.
        for step in row.parts() {
            if !seen.insert(step.clone()) {
                return Err(format!(
                    "step `{step}` produced TWO rows: the live tail and the durable rows must be \
                     disjoint, and so must the rows among themselves"
                ));
            }
        }
    }
    if live.text.is_empty() {
        return Ok(());
    }
    let durable = trailing_durable(rows);
    if durable.is_empty() {
        return Ok(());
    }
    if live.text.starts_with(&durable) || durable.starts_with(&live.text) {
        return Ok(());
    }
    Err(format!(
        "the live tail ({} bytes) is not prefix-related to the trailing durable text ({} bytes): \
         P3-D12's length rule would then render bytes the other half has already shown",
        live.text.len(),
        durable.len()
    ))
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "the_live_tail_and_the_durable_rows_never_overlap",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let Some((rows, live)) = last_frame() else {
        // Nothing has been drawn yet. There is no frame to be wrong about.
        return Ok(());
    };
    check_frame(&rows, &live).map_err(|detail| InvariantViolation {
        invariant: "the_live_tail_and_the_durable_rows_never_overlap",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{StepId, WakeId};

    fn text(step: &str, s: &str) -> Row {
        Row::Text {
            step: StepId::new(step),
            parts: vec![StepId::new(step)],
            wake: WakeId::new("w1"),
            index: 0,
            text: s.into(),
        }
    }

    #[test]
    fn a_step_drawn_twice_and_a_diverged_tail_are_both_violations() {
        let live = LiveText {
            agent: None,
            text: "Hello wor".into(),
        };
        // The handover state: the durable flush is a prefix of what streamed.
        check_frame(&[text("s1", "Hello")], &live).unwrap();
        // The settled state: nothing live.
        check_frame(&[text("s1", "Hello world")], &LiveText::default()).unwrap();

        let dup = check_frame(&[text("s1", "a"), text("s1", "b")], &LiveText::default())
            .expect_err("one step, two rows must be a violation");
        assert!(dup.contains("s1"), "{dup}");

        let diverged = check_frame(&[text("s1", "Goodbye")], &live)
            .expect_err("a tail unrelated to the durable text must be a violation");
        assert!(diverged.contains("prefix-related"), "{diverged}");
    }
}
