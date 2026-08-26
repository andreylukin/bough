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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};

use crate::OldFeedError;

/// The source name the watermark store keys `events` by.
pub const EVENTS: &str = "jungler.events";
/// The source name the watermark store keys `nodes` by.
pub const NODES: &str = "jungler.nodes";
/// The source name the watermark store keys `lane_story` by.
pub const LANE_STORY: &str = "jungler.lane_story";

/// The three tables, in the order a sweep reads them, with their watermark source names.
pub const TABLES: [(&str, &str); 3] = [
    ("events", EVENTS),
    ("nodes", NODES),
    ("lane_story", LANE_STORY),
];

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

/// The columns without which a table cannot be swept at all: its key and its timestamp.
pub fn required_columns(table: &str) -> &'static [&'static str] {
    match table {
        "events" => &["id", "at"],
        "nodes" => &["id", "updated_at"],
        "lane_story" => &["id", "updated_at"],
        _ => &[],
    }
}

/// Reads `sqlite_master`. NEVER an error: an absent or unreadable jungler db means the jungler
/// half is disabled, one line is logged, and the row still ACTIVATES (§14, V7).
pub fn probe(path: &Path) -> FeedProbe {
    if !path.exists() {
        return FeedProbe::Missing;
    }
    let conn = match open(path) {
        Ok(c) => c,
        Err(e) => return FeedProbe::Unreadable(e.to_string()),
    };
    let tables = match table_names(&conn) {
        Ok(t) => t,
        Err(e) => return FeedProbe::Unreadable(e.to_string()),
    };
    let mut missing_columns = Vec::new();
    for (table, _) in TABLES {
        if !tables.contains(table) {
            continue;
        }
        let cols = match columns(&conn, table) {
            Ok(c) => c,
            Err(e) => return FeedProbe::Unreadable(e.to_string()),
        };
        for req in required_columns(table) {
            if !cols.contains(*req) {
                missing_columns.push(format!("{table}.{req}"));
            }
        }
    }
    FeedProbe::Present {
        tables: tables.into_iter().collect(),
        missing_columns,
    }
}

/// PURE over a probe: which sources a sweep may read, and why each of the others may not.
///
/// The reason strings are what `/oldfeed` renders and what the one logged line says, so they name
/// the source and the cause and nothing else.
pub fn sources(probe: &FeedProbe, path: &Path) -> (BTreeSet<String>, Vec<(String, String)>) {
    let mut on = BTreeSet::new();
    let mut off = Vec::new();
    match probe {
        FeedProbe::Missing => {
            for (_, source) in TABLES {
                off.push((source.to_string(), format!("{} is absent", path.display())));
            }
        }
        FeedProbe::Unreadable(detail) => {
            for (_, source) in TABLES {
                off.push((
                    source.to_string(),
                    format!("{} is unreadable: {detail}", path.display()),
                ));
            }
        }
        FeedProbe::Present {
            tables,
            missing_columns,
        } => {
            for (table, source) in TABLES {
                if !tables.iter().any(|t| t == table) {
                    off.push((source.to_string(), format!("no `{table}` table")));
                    continue;
                }
                let missing: Vec<&String> = missing_columns
                    .iter()
                    .filter(|m| m.starts_with(&format!("{table}.")))
                    .collect();
                if missing.is_empty() {
                    on.insert(source.to_string());
                } else {
                    let names: Vec<&str> = missing.iter().map(|m| m.as_str()).collect();
                    off.push((
                        source.to_string(),
                        format!("missing required column(s): {}", names.join(", ")),
                    ));
                }
            }
        }
    }
    (on, off)
}

/// Read-only, and never creating the file: an absent db must probe as [`FeedProbe::Missing`]
/// rather than being conjured into existence by the adapter that reads it.
pub fn open(path: &Path) -> Result<Connection, OldFeedError> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?)
}

/// Every table name in the db.
pub fn table_names(conn: &Connection) -> Result<BTreeSet<String>, OldFeedError> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = BTreeSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// One table's columns.
pub fn columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>, OldFeedError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    let mut out = BTreeSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// Every table's columns, read once per sweep.
pub fn column_map(conn: &Connection) -> Result<BTreeMap<String, BTreeSet<String>>, OldFeedError> {
    let present = table_names(conn)?;
    let mut out = BTreeMap::new();
    for (table, _) in TABLES {
        if present.contains(table) {
            out.insert(table.to_string(), columns(conn, table)?);
        }
    }
    Ok(out)
}

/// `"kind"`, or `NULL AS "kind"` when the column is not there. THE defensive column reader: a
/// jungler that predates a column reads it as NULL instead of failing the query.
fn pick(cols: &BTreeSet<String>, name: &str) -> String {
    if cols.contains(name) {
        format!("\"{name}\"")
    } else {
        format!("NULL AS \"{name}\"")
    }
}

/// A jungler `events` row, every optional column already defaulted.
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

/// A jungler `nodes` row.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRow {
    pub id: i64,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub updated_at: i64,
    pub lane: Option<String>,
}

/// A jungler `lane_story` row.
#[derive(Clone, Debug, PartialEq)]
pub struct StoryRow {
    pub id: i64,
    pub lane: Option<String>,
    pub ord: i64,
    pub heading: Option<String>,
    pub body: Option<String>,
    pub updated_at: i64,
}

/// Rows with `id > after`, oldest first, at most `limit`.
pub fn read_events(
    conn: &Connection,
    cols: &BTreeSet<String>,
    after: i64,
    limit: usize,
) -> Result<Vec<EventRow>, OldFeedError> {
    let sql = format!(
        "SELECT \"id\", {}, {}, {}, {}, {}, {}, {} FROM \"events\" \
         WHERE \"id\" > ?1 ORDER BY \"id\" ASC LIMIT ?2",
        pick(cols, "at"),
        pick(cols, "kind"),
        pick(cols, "subject"),
        pick(cols, "body"),
        pick(cols, "ref"),
        pick(cols, "url"),
        pick(cols, "lane"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![after, limit as i64], |r| {
        Ok(EventRow {
            id: r.get(0)?,
            at: r.get::<_, Option<i64>>(1)?.unwrap_or_default(),
            kind: r.get(2)?,
            subject: r.get(3)?,
            body: r.get(4)?,
            r#ref: r.get(5)?,
            url: r.get(6)?,
            lane: r.get(7)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Rows with `id > after`, oldest first, at most `limit`.
pub fn read_nodes(
    conn: &Connection,
    cols: &BTreeSet<String>,
    after: i64,
    limit: usize,
) -> Result<Vec<NodeRow>, OldFeedError> {
    let sql = format!(
        "SELECT \"id\", {}, {}, {}, {}, {} FROM \"nodes\" \
         WHERE \"id\" > ?1 ORDER BY \"id\" ASC LIMIT ?2",
        pick(cols, "kind"),
        pick(cols, "title"),
        pick(cols, "summary"),
        pick(cols, "updated_at"),
        pick(cols, "lane"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![after, limit as i64], |r| {
        Ok(NodeRow {
            id: r.get(0)?,
            kind: r.get(1)?,
            title: r.get(2)?,
            summary: r.get(3)?,
            updated_at: r.get::<_, Option<i64>>(4)?.unwrap_or_default(),
            lane: r.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Rows with `id > after`, oldest first, at most `limit`. The SEALING order is `ord`, which
/// [`crate::OldFeedHandle::sweep`] applies to the batch this returns.
pub fn read_lane_story(
    conn: &Connection,
    cols: &BTreeSet<String>,
    after: i64,
    limit: usize,
) -> Result<Vec<StoryRow>, OldFeedError> {
    let sql = format!(
        "SELECT \"id\", {}, {}, {}, {}, {} FROM \"lane_story\" \
         WHERE \"id\" > ?1 ORDER BY \"id\" ASC LIMIT ?2",
        pick(cols, "lane"),
        pick(cols, "ord"),
        pick(cols, "heading"),
        pick(cols, "body"),
        pick(cols, "updated_at"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![after, limit as i64], |r| {
        Ok(StoryRow {
            id: r.get(0)?,
            lane: r.get(1)?,
            ord: r.get::<_, Option<i64>>(2)?.unwrap_or_default(),
            heading: r.get(3)?,
            body: r.get(4)?,
            updated_at: r.get::<_, Option<i64>>(5)?.unwrap_or_default(),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// The old daemons store epoch MILLISECONDS; a seconds-scale value is accepted too rather than
/// landing every old row in 1970. Out-of-range reads as the epoch — never as "now", which would
/// make a step's `at` depend on when the adapter happened to run.
pub fn ts_to_utc(raw: i64) -> DateTime<Utc> {
    let ms = if raw.abs() > 100_000_000_000 {
        raw
    } else {
        raw.saturating_mul(1000)
    };
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("the epoch is a valid instant"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_missing_column_is_read_as_null() {
        assert_eq!(pick(&set(&["kind"]), "kind"), "\"kind\"");
        assert_eq!(pick(&set(&["kind"]), "url"), "NULL AS \"url\"");
    }

    #[test]
    fn an_absent_db_disables_every_source() {
        let (on, off) = sources(&FeedProbe::Missing, Path::new("/nope/jungler.db"));
        assert!(on.is_empty());
        assert_eq!(off.len(), 3);
        assert!(off[0].1.contains("absent"));
    }

    #[test]
    fn a_missing_required_column_disables_only_its_own_source() {
        let probe = FeedProbe::Present {
            tables: vec![
                "events".to_string(),
                "nodes".to_string(),
                "lane_story".to_string(),
            ],
            missing_columns: vec!["events.at".to_string()],
        };
        let (on, off) = sources(&probe, Path::new("/x/jungler.db"));
        assert_eq!(on, set(&[NODES, LANE_STORY]));
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].0, EVENTS);
    }

    #[test]
    fn milliseconds_and_seconds_both_read_as_the_same_instant() {
        assert_eq!(ts_to_utc(1_700_000_000_000), ts_to_utc(1_700_000_000));
    }
}
