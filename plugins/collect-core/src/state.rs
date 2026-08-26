//! Invariant: the watermark is this row's OWN state, in this row's OWN sqlite file, and it is
//! written AFTER the delivery it covers. It is an optimisation, never the correctness argument —
//! the ref guard is (see `guard.rs`).
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS collect_watermarks (
//!   source     TEXT PRIMARY KEY,
//!   last_row   INTEGER NOT NULL,
//!   last_at    INTEGER NOT NULL,   -- millis; 0 ⇒ none
//!   cursor     TEXT,               -- an opaque provider cursor, or NULL
//!   updated_at INTEGER NOT NULL
//! );
//! ```

use std::path::Path;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::Connection;

use crate::CollectError;

/// Per (source, key) watermark. The `old-feed-adapter` shape.
pub struct WatermarkStore {
    // `rusqlite::Connection` is `Send` but not `Sync`; it is never held across an await.
    conn: Mutex<Connection>,
}

/// One source's position.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Watermark {
    pub last_row: i64,
    pub last_at: Option<DateTime<Utc>>,
    /// An opaque provider cursor (Linear's `endCursor`), when the source has one.
    pub cursor: Option<String>,
}

fn state(e: impl std::fmt::Display) -> CollectError {
    CollectError::State(e.to_string())
}

impl WatermarkStore {
    /// Open, creating the schema if absent.
    pub fn open(path: &Path) -> Result<WatermarkStore, CollectError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| CollectError::State(format!("{}: {e}", dir.display())))?;
            }
        }
        let conn = Connection::open(path).map_err(state)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS collect_watermarks (
               source     TEXT PRIMARY KEY,
               last_row   INTEGER NOT NULL,
               last_at    INTEGER NOT NULL,
               cursor     TEXT,
               updated_at INTEGER NOT NULL
             );",
        )
        .map_err(state)?;
        Ok(WatermarkStore {
            conn: Mutex::new(conn),
        })
    }

    /// The stored watermark for a source; [`Watermark::default`] when there is none.
    pub fn get(&self, source: &str) -> Result<Watermark, CollectError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT last_row, last_at, cursor FROM collect_watermarks WHERE source = ?1")
            .map_err(state)?;
        let mut rows = stmt.query([source]).map_err(state)?;
        match rows.next().map_err(state)? {
            Some(row) => {
                let last_row: i64 = row.get(0).map_err(state)?;
                let last_at: i64 = row.get(1).map_err(state)?;
                let cursor: Option<String> = row.get(2).map_err(state)?;
                Ok(Watermark {
                    last_row,
                    last_at: DateTime::from_timestamp_millis(last_at).filter(|_| last_at != 0),
                    cursor,
                })
            }
            None => Ok(Watermark::default()),
        }
    }

    /// Advance a source's watermark. The clock is INJECTED.
    pub fn set(
        &self,
        source: &str,
        mark: Watermark,
        now: DateTime<Utc>,
    ) -> Result<(), CollectError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO collect_watermarks (source, last_row, last_at, cursor, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source) DO UPDATE SET
               last_row = excluded.last_row,
               last_at = excluded.last_at,
               cursor = excluded.cursor,
               updated_at = excluded.updated_at",
            rusqlite::params![
                source,
                mark.last_row,
                mark.last_at.map(|t| t.timestamp_millis()).unwrap_or(0),
                mark.cursor,
                now.timestamp_millis(),
            ],
        )
        .map_err(state)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
    }

    #[test]
    fn a_source_never_swept_reads_as_the_default() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = WatermarkStore::open(&dir.path().join("collect.db")).expect("a fresh store");
        assert_eq!(store.get("prs").expect("a read"), Watermark::default());
    }

    #[test]
    fn a_watermark_with_a_cursor_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("collect.db");
        let mark = Watermark {
            last_row: 12,
            last_at: Some(now()),
            cursor: Some("abc".to_string()),
        };
        {
            let store = WatermarkStore::open(&path).expect("a fresh store");
            store.set("issues", mark.clone(), now()).expect("a write");
        }
        let store = WatermarkStore::open(&path).expect("a reopen");
        assert_eq!(store.get("issues").expect("a read"), mark);
    }
}
