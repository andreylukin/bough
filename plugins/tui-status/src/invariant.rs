//! §0.2 runtime invariant for `bough-plugin-tui-status`:
//!
//! **The status line is exactly one terminal row, it never exceeds the width it was given, and
//! every number on it comes from a ledgered fact.** A value the ledger has not recorded (an
//! unknown model price, a projection with no header yet) renders as `—` rather than as a plausible
//! zero — the line is the most-read chrome in the product, so a fabricated number there is the
//! most expensive lie the surface can tell (§16, phase ux1 §2.5).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;
use ratatui::text::Line;

use crate::status::{StatusView, UNKNOWN};

/// What the last rendered frame put on the line: its width budget, the cells it used, how many
/// rows it drew, and the view it drew from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub width: u16,
    pub cells: usize,
    pub rows: usize,
    pub text: String,
    pub view: StatusView,
}

static LAST_FRAME: Mutex<Option<Frame>> = Mutex::new(None);

/// Record what this frame drew. Called from `StatusPane::render`, which is synchronous and cannot
/// await a check — so the frame is recorded and the quiesce check reads it.
pub fn record_frame(view: &StatusView, line: &Line<'static>, width: u16) {
    let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
    *LAST_FRAME.lock() = Some(Frame {
        width,
        cells: text.chars().count(),
        rows: text.lines().count().max(1),
        text,
        view: view.clone(),
    });
}

/// What the last frame drew, for the check and for tests.
pub fn last_frame() -> Option<Frame> {
    LAST_FRAME.lock().clone()
}

/// PURE: the check, over what the line rendered.
pub fn check_frame(f: &Frame) -> Result<(), String> {
    if f.rows != 1 {
        return Err(format!(
            "the status line drew {} rows: it is ONE row by construction, and a second one paints \
             over the transcript's baseline (M9)",
            f.rows
        ));
    }
    if f.cells > f.width as usize {
        return Err(format!(
            "the status line used {} cells of {}: `fields` drops in a fixed order precisely so \
             this cannot happen (M9)",
            f.cells, f.width
        ));
    }
    // The honesty half: an unknown cost must have reached the screen as `—`, never as a number.
    if f.view.cost_usd.is_none() && f.text.contains('$') {
        return Err(format!(
            "the status line drew a cost ({:?}) while the ledger holds no `usage/round` with a \
             price: an unknown number renders as `{UNKNOWN}` (§16, M24)",
            f.text
        ));
    }
    Ok(())
}

/// The specs this row contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "the_status_line_is_one_row_and_never_invents_a_number",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let Some(frame) = last_frame() else {
        // Nothing drawn yet is not a violation: the row may have activated before the first paint.
        return Ok(());
    };
    check_frame(&frame).map_err(|detail| InvariantViolation {
        invariant: "the_status_line_is_one_row_and_never_invents_a_number",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(cells: usize, width: u16) -> Frame {
        Frame {
            width,
            cells,
            rows: 1,
            text: "x".repeat(cells),
            view: StatusView::default(),
        }
    }

    #[test]
    fn a_line_that_fits_holds_and_one_that_overflows_says_by_how_much() {
        check_frame(&frame(40, 80)).unwrap();
        let err = check_frame(&frame(90, 80)).expect_err("an overflowing line is a violation");
        assert!(
            err.contains("90"),
            "the violation names the overflow: {err}"
        );
    }

    #[test]
    fn a_second_row_is_a_violation() {
        let mut f = frame(10, 80);
        f.rows = 2;
        assert!(check_frame(&f).is_err());
    }

    #[test]
    fn a_price_with_no_ledgered_cost_behind_it_is_a_violation() {
        let mut f = frame(0, 80);
        f.text = "bough 0.1 · $0.00".into();
        f.cells = f.text.chars().count();
        let err = check_frame(&f).expect_err("an invented number is the bug M24 reported");
        assert!(err.contains(UNKNOWN));
    }
}
