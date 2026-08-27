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
    Anchored { top: usize },
}

impl Scroll {
    /// The first row of the viewport, given how much there is to show.
    pub fn top(self, rows: usize, height: u16) -> usize {
        let max = max_top(rows, height);
        match self {
            Scroll::Follow => max,
            Scroll::Anchored { top } => top.min(max),
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
            Scroll::Anchored { top: to as usize }
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
        Scroll::Anchored { top: row }
    }

    /// Whether the viewport is pinned to the newest row.
    pub fn is_following(self) -> bool {
        matches!(self, Scroll::Follow)
    }
}

fn max_top(rows: usize, height: u16) -> usize {
    rows.saturating_sub(height as usize)
}

// ---------------------------------------------------------------------------
// phase ux1 §2.2: follow + the unread affordance
// ---------------------------------------------------------------------------

/// Where the transcript is looking, and how much it has not shown. One per transcript pane.
///
/// "Auto-follow at the tail" and "`↓ N new` when detached" are the same state machine, so they
/// live in one type: an unread count that could disagree with the scroll state is the bug (B2).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Viewport {
    pub scroll: Scroll,
    pub unseen: usize,
}

impl Viewport {
    /// PURE: rows were appended. Following ⇒ nothing to count; anchored ⇒ `unseen += added`.
    pub fn on_rows_appended(&mut self, added: usize) {
        self.scroll = self.scroll.on_rows_appended(added);
        if !self.scroll.is_following() {
            self.unseen = self.unseen.saturating_add(added);
        }
    }

    /// PURE: a scroll input. Landing at the bottom re-arms `Follow` and zeroes `unseen`.
    pub fn scrolled(&mut self, delta: i32, rows: usize, height: u16) {
        self.scroll = self.scroll.scrolled(delta, rows, height);
        if self.scroll.is_following() {
            // Landing at the bottom IS having seen everything: an unread count that outlived the
            // scroll back to the tail is the disagreement this type exists to make impossible.
            self.unseen = 0;
        }
    }

    /// `End`, and what sending a message does: back to the tail, `unseen = 0` (B2).
    pub fn to_latest(&mut self) {
        self.scroll = Scroll::Follow;
        self.unseen = 0;
    }

    /// Anchor on a row (a search hit, a `FocusRequest { step }`).
    pub fn anchor_on(&mut self, row: usize) {
        self.scroll = Scroll::anchored_on(row);
        // Arriving at a hit is not reading the tail: what came in while detached is still unseen,
        // and a jump that silently zeroed the badge would hide it.
    }

    /// PURE: the affordance text, or `None` while following. `"↓ 3 new"`.
    pub fn badge(&self) -> Option<String> {
        (!self.is_following() && self.unseen > 0).then(|| format!("\u{2193} {} new", self.unseen))
    }

    pub fn top(&self, rows: usize, height: u16) -> usize {
        self.scroll.top(rows, height)
    }

    pub fn is_following(&self) -> bool {
        self.scroll.is_following()
    }
}
