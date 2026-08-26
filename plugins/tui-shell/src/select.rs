//! Invariant: a selection is a rectangle of what is VISIBLE, extracted from the shell's LAST
//! RENDERED BUFFER and never from a pane (P3-D6). Every pane draws into that buffer, so the copied
//! text is exactly what is on screen — including a pane's own wrapping — and can never disagree
//! with it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// A drag in progress, or a finished one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    /// The normalised rectangle the anchor and head span. Inclusive of both cells, so a click and
    /// release on one cell selects that cell rather than nothing.
    pub fn rect(&self) -> Rect {
        let (x0, x1) = min_max(self.anchor.0, self.head.0);
        let (y0, y1) = min_max(self.anchor.1, self.head.1);
        Rect {
            x: x0,
            y: y0,
            width: x1 - x0 + 1,
            height: y1 - y0 + 1,
        }
    }

    /// Whether the drag has moved off its anchor cell.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

fn min_max(a: u16, b: u16) -> (u16, u16) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Block select out of the LAST RENDERED BUFFER, per-line trailing spaces trimmed, `\n` joined.
///
/// Cells outside the buffer are skipped rather than substituted: a selection that ran past the
/// edge of a smaller frame must not invent spaces that were never on screen.
pub fn text_from_buffer(buf: &Buffer, rect: Rect) -> String {
    let clipped = rect.intersection(buf.area);
    let mut out = String::new();
    for (i, y) in (clipped.y..clipped.y.saturating_add(clipped.height)).enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut line = String::new();
        for x in clipped.x..clipped.x.saturating_add(clipped.width) {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rect_is_normalised_whichever_way_the_drag_went() {
        let forward = Selection {
            anchor: (2, 1),
            head: (5, 3),
        };
        let backward = Selection {
            anchor: (5, 3),
            head: (2, 1),
        };
        assert_eq!(forward.rect(), backward.rect());
        assert_eq!(forward.rect(), Rect::new(2, 1, 4, 3));
    }

    #[test]
    fn a_one_cell_drag_selects_that_cell() {
        let s = Selection {
            anchor: (4, 4),
            head: (4, 4),
        };
        assert_eq!(s.rect(), Rect::new(4, 4, 1, 1));
        assert!(s.is_empty());
    }
}
