//! Invariant: a roving row focus is VISIBLE whenever it exists (phase ux1 §2.1, B6). `None` is
//! the only state that draws nothing, and it is the state of a pane that has never had the
//! keyboard. Every function here is PURE.

use bough_plugin_ledger::StepId;

use crate::rows::Row;

/// The roving row focus inside a transcript pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowFocus {
    pub index: Option<usize>,
}

impl RowFocus {
    /// PURE: move by `delta` over `rows`, clamping. From `None`, a move in EITHER direction lands
    /// on the LAST row: a keyboard user arriving from the composer is at the bottom of the
    /// conversation, not the top.
    pub fn moved(self, delta: i32, rows: usize) -> RowFocus {
        let _ = (self, delta, rows);
        todo!("WP-1")
    }

    /// The row a `FocusRequest { step }` names, so a search hit and the keyboard agree.
    pub fn on_step(rows: &[Row], step: &StepId) -> RowFocus {
        let _ = (rows, step);
        todo!("WP-1")
    }

    /// PURE: whether this row index should paint the focus indicator this frame.
    pub fn is_on(&self, index: usize) -> bool {
        self.index == Some(index)
    }
}

/// PURE: the indicator a focused row carries. Never colour alone (audit delight 3): a marker
/// glyph in the gutter column AND a `sel_bg` fill.
pub fn focus_marker() -> char {
    '▌'
}
