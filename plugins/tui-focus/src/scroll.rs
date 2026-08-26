//! Invariant: an ANCHORED viewport does not move when new steps arrive (V3). That is the whole
//! point of the state machine: `Follow` is pinned to the bottom and stays pinned; `Anchored` is
//! pinned to a step and ignores everything appended after it until `End` or a scroll to the
//! bottom re-arms `Follow`.

/// Where the viewport is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scroll {
    /// Pinned to the bottom; new steps keep it pinned.
    Follow,
    /// Anchored to a step: new steps DO NOT move the viewport (V3).
    Anchored { top: usize, offset: u16 },
}

impl Scroll {
    /// PURE: a scroll input ⇒ the next state, clamped to the row count and the viewport height.
    pub fn scrolled(self, _delta: i32, _rows: usize, _height: u16) -> Scroll {
        todo!("WP-4")
    }

    /// PURE: what new rows do to this state. `Follow` follows; `Anchored` does not move.
    pub fn on_rows_appended(self, _added: usize) -> Scroll {
        todo!("WP-4")
    }
}
