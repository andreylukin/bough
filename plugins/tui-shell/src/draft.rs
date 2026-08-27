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
    pub fn on_key(&mut self, now: DateTime<Utc>) -> bool {
        let _ = (now, &self.window, &self.last_key);
        todo!("WP-2")
    }

    pub fn reset(&mut self) {
        self.last_key = None;
    }
}

/// Sent-message recall (M20). Bounded, deduped against the immediately previous entry.
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
            cap,
        }
    }

    pub fn push(&mut self, text: &str) {
        let _ = (text, &self.items, self.cap);
        todo!("WP-2")
    }

    /// Up: the previous entry, holding the live draft so Down restores it.
    pub fn prev(&mut self, draft: &str) -> Option<String> {
        let _ = (draft, &self.cursor, &self.held);
        todo!("WP-2")
    }

    pub fn next(&mut self) -> Option<String> {
        todo!("WP-2")
    }

    pub fn reset(&mut self) {
        self.cursor = None;
        self.held = None;
    }
}

/// PURE: readline's kill-to-line-start. `Ctrl+U` deleted ONE character in 8 of 10 walks.
/// Returns the new line and the new caret offset.
pub fn kill_to_line_start(line: &str, caret: usize) -> (String, usize) {
    let _ = (line, caret);
    todo!("WP-2")
}
