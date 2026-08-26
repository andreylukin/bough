//! Invariant: the watermark is this row's OWN state, in this row's OWN sqlite file, and it is
//! written AFTER the delivery it covers. It is an optimisation, never the correctness argument —
//! the ref guard is (see `guard.rs`).

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::CollectError;

/// Per (source, key) watermark. The `old-feed-adapter` shape.
pub struct WatermarkStore {
    // rusqlite::Connection; never held across an await.
}

/// One source's position.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Watermark {
    pub last_row: i64,
    pub last_at: Option<DateTime<Utc>>,
    /// An opaque provider cursor (Linear's `endCursor`), when the source has one.
    pub cursor: Option<String>,
}

impl WatermarkStore {
    /// Open, creating the schema if absent. WP-2.
    pub fn open(path: &Path) -> Result<WatermarkStore, CollectError> {
        let _ = path;
        todo!("WP-2")
    }
    /// The stored watermark for a source; `Watermark::default()` when there is none. WP-2.
    pub fn get(&self, source: &str) -> Result<Watermark, CollectError> {
        let _ = source;
        todo!("WP-2")
    }
    /// Advance a source's watermark. The clock is INJECTED. WP-2.
    pub fn set(
        &self,
        source: &str,
        mark: Watermark,
        now: DateTime<Utc>,
    ) -> Result<(), CollectError> {
        let _ = (source, mark, now);
        todo!("WP-2")
    }
}
