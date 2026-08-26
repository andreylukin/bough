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
    /// The normalised rectangle the anchor and head span.
    pub fn rect(&self) -> Rect {
        todo!("WP-2")
    }
}

/// Block select out of the LAST RENDERED BUFFER, per-line trailing spaces trimmed, `\n` joined.
pub fn text_from_buffer(_buf: &Buffer, _rect: Rect) -> String {
    todo!("WP-2")
}
