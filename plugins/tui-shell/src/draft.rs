//! Invariant: nothing the user typed is deleted by anything except an explicit clear (phase ux1
//! §2.3, V3). Every function here either preserves the draft or is the explicit clear.

use std::collections::VecDeque;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// A newline burst that is really a paste (B4). A terminal that does not advertise bracketed
/// paste delivers a paste as N key events in microseconds; a human cannot type two newlines
/// `burst_ms` apart.
pub struct PasteBurst {
    window: Duration,
    last_key: Option<DateTime<Utc>>,
}

impl PasteBurst {
    pub fn new(window: Duration) -> PasteBurst {
        PasteBurst {
            window,
            last_key: None,
        }
    }

    /// PURE in `now`: record a key. Returns whether the Enter that just arrived is part of a
    /// burst and must be treated as a NEWLINE rather than a send.
    ///
    /// The decision is made on the GAP TO THE PREVIOUS KEY, not on a count: a paste's keys arrive
    /// in microseconds, and a human's do not. The first key of a session is never a burst.
    pub fn on_key(&mut self, now: DateTime<Utc>) -> bool {
        let burst = match self.last_key {
            Some(prev) => now
                .signed_duration_since(prev)
                .to_std()
                .map(|gap| gap < self.window)
                // A clock that went backwards is not evidence of a paste.
                .unwrap_or(false),
            None => false,
        };
        self.last_key = Some(now);
        burst
    }

    pub fn reset(&mut self) {
        self.last_key = None;
    }
}

/// Sent-message recall (M20). Bounded, deduped against the immediately previous entry.
///
/// The cursor is an index INTO `items`; `held` is the live draft the user was in the middle of
/// when they first pressed Up, so walking back down hands it back untouched (V3).
pub struct SentHistory {
    items: VecDeque<String>,
    cursor: Option<usize>,
    held: Option<String>,
    cap: usize,
}

impl SentHistory {
    pub fn new(cap: usize) -> SentHistory {
        SentHistory {
            items: VecDeque::new(),
            cursor: None,
            held: None,
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, text: &str) {
        self.reset();
        if text.is_empty() {
            return;
        }
        if self.items.back().map(String::as_str) == Some(text) {
            return;
        }
        self.items.push_back(text.to_string());
        while self.items.len() > self.cap {
            self.items.pop_front();
        }
    }

    /// Up: the previous entry, holding the live draft so Down restores it.
    pub fn prev(&mut self, draft: &str) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        let idx = match self.cursor {
            None => {
                self.held = Some(draft.to_string());
                self.items.len()
            }
            Some(i) => i,
        };
        if idx == 0 {
            // Already at the oldest: stay there rather than wrap.
            return None;
        }
        self.cursor = Some(idx - 1);
        self.items.get(idx - 1).cloned()
    }

    /// Down: the next entry, and past the newest, the held live draft. Named for the direction
    /// of the walk, not for `Iterator` — a history that ends by handing back a draft is not one.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<String> {
        let i = self.cursor?;
        if i + 1 < self.items.len() {
            self.cursor = Some(i + 1);
            return self.items.get(i + 1).cloned();
        }
        self.cursor = None;
        Some(self.held.take().unwrap_or_default())
    }

    /// Whether a walk is in progress (Down is meaningful).
    pub fn is_walking(&self) -> bool {
        self.cursor.is_some()
    }

    pub fn reset(&mut self) {
        self.cursor = None;
        self.held = None;
    }
}

/// PURE: readline's kill-to-line-start. `Ctrl+U` deleted ONE character in 8 of 10 walks.
/// Returns the new line and the new caret offset. `caret` and the offset are CHARACTER offsets.
pub fn kill_to_line_start(line: &str, caret: usize) -> (String, usize) {
    let kept: String = line.chars().skip(caret).collect();
    (kept, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_700_000_000_000 + ms).unwrap()
    }

    #[test]
    fn keys_inside_the_window_are_a_burst_and_the_first_key_never_is() {
        let mut b = PasteBurst::new(Duration::from_millis(20));
        assert!(!b.on_key(t(0)), "the first key of a session stands alone");
        assert!(b.on_key(t(1)), "1ms later: a paste");
        assert!(b.on_key(t(3)));
        assert!(!b.on_key(t(500)), "half a second later: a human");
        assert!(!b.on_key(t(520)), "exactly the window is not inside it");
    }

    #[test]
    fn reset_forgets_the_previous_key() {
        let mut b = PasteBurst::new(Duration::from_millis(20));
        b.on_key(t(0));
        b.reset();
        assert!(!b.on_key(t(1)));
    }

    #[test]
    fn history_walks_back_and_forward_and_hands_the_live_draft_back() {
        let mut h = SentHistory::new(8);
        h.push("one");
        h.push("two");

        assert_eq!(h.prev("live"), Some("two".to_string()));
        assert_eq!(h.prev("live"), Some("one".to_string()));
        assert_eq!(h.prev("live"), None, "the oldest does not wrap");
        assert_eq!(h.next(), Some("two".to_string()));
        assert_eq!(
            h.next(),
            Some("live".to_string()),
            "the draft the user was writing came back"
        );
        assert_eq!(h.next(), None, "and past it there is nothing");
    }

    #[test]
    fn history_dedupes_the_immediately_previous_entry_and_is_bounded() {
        let mut h = SentHistory::new(2);
        h.push("a");
        h.push("a");
        h.push("b");
        h.push("c");
        assert_eq!(h.prev(""), Some("c".to_string()));
        assert_eq!(h.prev(""), Some("b".to_string()));
        assert_eq!(h.prev(""), None, "`a` fell off the end of a cap of two");
    }

    #[test]
    fn an_empty_history_never_moves() {
        let mut h = SentHistory::new(4);
        assert_eq!(h.prev("live"), None);
        assert_eq!(h.next(), None);
    }

    #[test]
    fn kill_to_line_start_keeps_everything_after_the_caret() {
        assert_eq!(
            kill_to_line_start("abcdefgh", 8),
            (String::new(), 0),
            "at the end of the line the whole line goes"
        );
        assert_eq!(kill_to_line_start("abcdef", 3), ("def".to_string(), 0));
        assert_eq!(kill_to_line_start("abc", 0), ("abc".to_string(), 0));
        assert_eq!(kill_to_line_start("héllo", 2), ("llo".to_string(), 0));
    }
}
