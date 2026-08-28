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
        if rows == 0 {
            return RowFocus { index: None };
        }
        let last = rows - 1;
        let index = match self.index {
            // Arriving from the composer: the newest row, whichever way the user pressed.
            None => last,
            Some(i) => {
                let next = i.min(last) as i64 + delta as i64;
                next.clamp(0, last as i64) as usize
            }
        };
        RowFocus { index: Some(index) }
    }

    /// The row a `FocusRequest { step }` names, so a search hit and the keyboard agree.
    pub fn on_step(rows: &[Row], step: &StepId) -> RowFocus {
        RowFocus {
            index: rows.iter().position(|r| r.step() == step),
        }
    }

    /// PURE: whether this row index should paint the focus indicator this frame.
    pub fn is_on(&self, index: usize) -> bool {
        self.index == Some(index)
    }

    /// The row whose lines contain `line` (an index into the frame's line list), from where each
    /// row's FIRST line landed. `None` below the last row's lines — a click on the empty tail of
    /// the transcript focuses nothing rather than the last row.
    pub fn row_at_line(row_lines: &[u16], line: usize, total_lines: usize) -> Option<usize> {
        if line >= total_lines {
            return None;
        }
        row_lines.iter().rposition(|&first| first as usize <= line)
    }
}

/// PURE: the indicator a focused row carries. Never colour alone (audit delight 3): a marker
/// glyph in the gutter column AND a `sel_bg` fill.
pub fn focus_marker() -> char {
    '▌'
}

#[cfg(test)]
mod tests {
    use super::RowFocus;

    #[test]
    fn a_line_maps_to_the_row_whose_span_holds_it() {
        // Rows starting at lines 0, 3 and 4; six lines in all.
        let starts = [0, 3, 4];
        assert_eq!(RowFocus::row_at_line(&starts, 0, 6), Some(0));
        assert_eq!(RowFocus::row_at_line(&starts, 2, 6), Some(0));
        assert_eq!(RowFocus::row_at_line(&starts, 3, 6), Some(1));
        assert_eq!(RowFocus::row_at_line(&starts, 5, 6), Some(2));
        // Past the last line: the empty tail, not the last row.
        assert_eq!(RowFocus::row_at_line(&starts, 6, 6), None);
        assert_eq!(RowFocus::row_at_line(&[], 0, 0), None);
    }
}
