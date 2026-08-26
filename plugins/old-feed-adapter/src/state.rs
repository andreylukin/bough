//! Invariant: the watermarks live in the adapter's OWN sqlite file (P3-D13). The ledger is
//! append-only and its schema belongs to `ledger-sqlite`; a mutable collector watermark has no
//! business there, and a separate file dies with one `rm` when Phase 6 sets `disabled: true`.
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS feed_watermarks (
//!   source     TEXT PRIMARY KEY,   -- 'jungler.events' | 'jungler.nodes' | 'jungler.lane_story'
//!   last_row   INTEGER NOT NULL,
//!   last_at    INTEGER NOT NULL,
//!   updated_at INTEGER NOT NULL
//! );
//! ```

use std::path::Path;

use crate::OldFeedError;

/// One source's position in the old feed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Watermark {
    pub last_row: i64,
    pub last_at: i64,
}

/// The adapter's own store.
pub struct WatermarkStore {
    _private: (),
}

impl WatermarkStore {
    /// Open (creating the table if needed).
    pub fn open(_path: &Path) -> Result<WatermarkStore, OldFeedError> {
        todo!("WP-6")
    }

    /// Where a source stands. A source never swept reads as the default.
    pub fn get(&self, _source: &str) -> Result<Watermark, OldFeedError> {
        todo!("WP-6")
    }

    /// Advance a source. Written LAST, after the delivery it covers.
    pub fn set(
        &self,
        _source: &str,
        _mark: Watermark,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), OldFeedError> {
        todo!("WP-6")
    }
}
