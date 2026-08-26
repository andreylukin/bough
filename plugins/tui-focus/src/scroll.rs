//! Invariant: an ANCHORED viewport does not move when new steps arrive (V3). That is the whole
//! point of the state machine: `Follow` is pinned to the bottom and stays pinned; `Anchored` is
//! pinned to a step and ignores everything appended after it until `End` or a scroll to the
//! bottom re-arms `Follow`.

/// Where the viewport is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Scroll {
    /// Pinned to the bottom; new steps keep it pinned. The state a fresh pane starts in.
    #[default]
    Follow,
    /// Anchored to a step: new steps DO NOT move the viewport (V3).
    Anchored { top: usize, offset: u16 },
}

impl Scroll {
    /// The first row of the viewport, given how much there is to show.
    pub fn top(self, rows: usize, height: u16) -> usize {
        let max = max_top(rows, height);
        match self {
            Scroll::Follow => max,
            Scroll::Anchored { top, .. } => top.min(max),
        }
    }

    /// PURE: a scroll input ⇒ the next state, clamped to the row count and the viewport height.
    ///
    /// Positive `delta` moves DOWN (toward the newest row). Landing at or past the last row
    /// re-arms `Follow` — that is the "scrolling to the bottom re-arms follow" half of §2.4, and
    /// it is why `End` needs no special case: it is a large positive delta.
    pub fn scrolled(self, delta: i32, rows: usize, height: u16) -> Scroll {
        let max = max_top(rows, height) as i64;
        let from = self.top(rows, height) as i64;
        let to = (from + delta as i64).clamp(0, max);
        if to >= max {
            // At the bottom IS following: an anchored-at-the-bottom viewport that then refused to
            // move with new rows would look frozen, which is the opposite of what anchoring is for.
            Scroll::Follow
        } else {
            Scroll::Anchored {
                top: to as usize,
                offset: 0,
            }
        }
    }

    /// PURE: what new rows do to this state. `Follow` follows; `Anchored` does not move.
    pub fn on_rows_appended(self, _added: usize) -> Scroll {
        // Deliberately total and deliberately boring: the count is irrelevant, because `Anchored`
        // stores an absolute row index and `Follow` recomputes its top from the row count at
        // render time. Appending changes neither.
        self
    }

    /// Anchor on a row index, as a `FocusRequest { step: Some(..) }` asks for.
    pub fn anchored_on(row: usize) -> Scroll {
        Scroll::Anchored {
            top: row,
            offset: 0,
        }
    }

    /// Whether the viewport is pinned to the newest row.
    pub fn is_following(self) -> bool {
        matches!(self, Scroll::Follow)
    }
}

fn max_top(rows: usize, height: u16) -> usize {
    rows.saturating_sub(height as usize)
}
