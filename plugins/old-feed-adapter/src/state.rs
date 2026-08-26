//! Invariant: the watermarks live in the adapter's OWN sqlite file (P3-D13). The ledger is
//! append-only and its schema belongs to `ledger-sqlite`; a mutable collector watermark has no
//! business there, and a separate file dies with one `rm` when Phase 6 sets `disabled: true`.
//!
//! The watermark is written LAST, after the delivery it covers: a crash between the two cannot
//! duplicate, because the ref guard in `lib.rs` catches the redelivery on restart.
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

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::OldFeedError;

/// One source's position in the old feed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Watermark {
    pub last_row: i64,
    pub last_at: i64,
}

/// The adapter's own store.
pub struct WatermarkStore {
    conn: Mutex<Connection>,
}

impl WatermarkStore {
    /// Open (creating the table if needed).
    pub fn open(path: &Path) -> Result<WatermarkStore, OldFeedError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| OldFeedError::Failed(format!("{}: {e}", dir.display())))?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS feed_watermarks (
               source     TEXT PRIMARY KEY,
               last_row   INTEGER NOT NULL,
               last_at    INTEGER NOT NULL,
               updated_at INTEGER NOT NULL
             );",
        )?;
        Ok(WatermarkStore {
            conn: Mutex::new(conn),
        })
    }

    /// Where a source stands. A source never swept reads as the default.
    pub fn get(&self, source: &str) -> Result<Watermark, OldFeedError> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare("SELECT last_row, last_at FROM feed_watermarks WHERE source = ?1")?;
        let mut rows = stmt.query([source])?;
        match rows.next()? {
            Some(row) => Ok(Watermark {
                last_row: row.get(0)?,
                last_at: row.get(1)?,
            }),
            None => Ok(Watermark::default()),
        }
    }

    /// Advance a source. Written LAST, after the delivery it covers.
    pub fn set(
        &self,
        source: &str,
        mark: Watermark,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), OldFeedError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO feed_watermarks (source, last_row, last_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source) DO UPDATE SET
               last_row = excluded.last_row,
               last_at = excluded.last_at,
               updated_at = excluded.updated_at",
            rusqlite::params![source, mark.last_row, mark.last_at, now.timestamp_millis()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
    }

    #[test]
    fn a_source_never_swept_reads_as_the_default() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = WatermarkStore::open(&dir.path().join("old-feed.db")).expect("a fresh store");
        assert_eq!(store.get("jungler.events").unwrap(), Watermark::default());
    }

    #[test]
    fn a_watermark_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("old-feed.db");
        {
            let store = WatermarkStore::open(&path).expect("a fresh store");
            store
                .set(
                    "jungler.events",
                    Watermark {
                        last_row: 7,
                        last_at: 12,
                    },
                    now(),
                )
                .expect("a write");
        }
        let store = WatermarkStore::open(&path).expect("a reopen");
        assert_eq!(
            store.get("jungler.events").unwrap(),
            Watermark {
                last_row: 7,
                last_at: 12
            }
        );
    }
}
