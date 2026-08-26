//! Invariant: the jungler side is read DEFENSIVELY and NEVER fails the boot. Required columns are
//! `id` and a timestamp; every other column is optional and read as NULL. An absent, unreadable or
//! column-short source is DISABLED with one logged line — never a panic, never a boot failure
//! (§14, V7). `~/.jungler/jungler.db` does not exist on this machine, so the shape below is the
//! CONTRACT this adapter reads and the fixture in its tests is authoritative for it:
//!
//! ```sql
//! events     (id INTEGER PK, at INTEGER, kind TEXT, subject TEXT, body TEXT, ref TEXT, url TEXT, lane TEXT)
//! nodes      (id INTEGER PK, kind TEXT, title TEXT, summary TEXT, updated_at INTEGER, lane TEXT)
//! lane_story (id INTEGER PK, lane TEXT, ord INTEGER, heading TEXT, body TEXT, updated_at INTEGER)
//! ```

use std::path::Path;

/// What a probe of the jungler db found.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedProbe {
    Present {
        tables: Vec<String>,
        missing_columns: Vec<String>,
    },
    Missing,
    Unreadable(String),
}

/// Reads `sqlite_master`. NEVER an error: an absent or unreadable jungler db means the jungler
/// half is disabled, one line is logged, and the row still ACTIVATES (§14, V7).
pub fn probe(_path: &Path) -> FeedProbe {
    todo!("WP-6")
}

/// One `events` row, every optional column read as NULL.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRow {
    pub id: i64,
    pub at: i64,
    pub kind: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub r#ref: Option<String>,
    pub url: Option<String>,
    pub lane: Option<String>,
}

/// One `nodes` row with a non-empty `summary`, or one `lane_story` section.
#[derive(Clone, Debug, PartialEq)]
pub struct RollupRow {
    pub id: i64,
    pub ord: Option<i64>,
    pub heading: Option<String>,
    pub body: String,
    pub lane: Option<String>,
    pub updated_at: i64,
}
