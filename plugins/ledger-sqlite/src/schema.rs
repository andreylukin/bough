//! Invariant: append-only is enforced BELOW the Rust API (V1). `UPDATE`/`DELETE` on `steps`,
//! `step_refs` and `edges` abort at the sqlite level; `rollups` accepts exactly one NULL → value
//! write to `superseded_by` and nothing else moving; `agents` carries no triggers at all, because
//! §3 exempts it as mutable config.

use bough_plugin_ledger::{LedgerError, LEDGER_FORMAT_VERSION};
use rusqlite::Connection;

/// The whole §3 schema of the phase plan §2.8: tables, indexes, FTS5, and the triggers.
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Create the schema if it is absent and check `PRAGMA user_version`.
///
/// A version this binary does not speak is [`LedgerError::FormatVersion`] — loud, at open, never a
/// silent migration.
pub fn open_and_migrate(conn: &Connection, path: &str) -> Result<(), LedgerError> {
    let found: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(store_err)?;
    match found {
        // A brand-new file: install the schema and stamp it.
        0 => {
            conn.execute_batch(SCHEMA_SQL).map_err(store_err)?;
            conn.execute_batch(&format!("PRAGMA user_version = {LEDGER_FORMAT_VERSION};"))
                .map_err(store_err)?;
            Ok(())
        }
        v if v == LEDGER_FORMAT_VERSION => {
            // Idempotent: every statement is `IF NOT EXISTS`, so re-opening is a no-op and a db
            // written by an older build of the same version gains anything it is missing.
            conn.execute_batch(SCHEMA_SQL).map_err(store_err)?;
            Ok(())
        }
        found => Err(LedgerError::FormatVersion {
            path: path.to_string(),
            found,
            expected: LEDGER_FORMAT_VERSION,
        }),
    }
}

/// The declared ENVELOPE — table and column names of `steps`/`edges`/`rollups`, in order — which
/// [`bough_plugin_ledger::envelope_fingerprint`] hashes. Step types are not part of it.
pub fn envelope() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "steps",
            vec![
                "id",
                "traj_id",
                "seq",
                "at",
                "wake_id",
                "type",
                "class",
                "body",
                "cites",
                "ignorable",
            ],
        ),
        // `step_refs` is CANONICAL for matching/routing (§3), so a column change there is an
        // envelope change: the drift test must see this table too.
        ("step_refs", vec!["step_id", "ref"]),
        (
            "edges",
            vec!["child_traj", "parent_traj", "at_seq", "kind", "at"],
        ),
        (
            "rollups",
            vec![
                "id",
                "traj_id",
                "kind",
                "tier",
                "from_seq",
                "to_seq",
                "src_trajs",
                "body",
                "notable_refs",
                "prompt_ver",
                "sealed_at",
                "superseded_by",
            ],
        ),
    ]
}

/// Every store error crosses the seam as [`LedgerError::Store`], which is `anyhow`-shaped.
pub(crate) fn store_err(e: rusqlite::Error) -> LedgerError {
    LedgerError::Store(anyhow::Error::new(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The envelope this crate declares must be the envelope the schema actually creates —
    /// otherwise `envelope_fingerprint()` hashes a fiction.
    #[test]
    fn the_declared_envelope_matches_the_created_tables() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        open_and_migrate(&conn, ":memory:").expect("fresh db migrates");
        for (table, declared) in envelope() {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .expect("table_info");
            let actual: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .expect("columns")
                .map(|c| c.expect("column name"))
                .collect();
            assert_eq!(actual, declared, "column list of `{table}` drifted");
        }
    }

    #[test]
    fn a_foreign_user_version_is_refused() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        let err = open_and_migrate(&conn, ":memory:").expect_err("a foreign version must fail");
        assert!(
            matches!(err, LedgerError::FormatVersion { found: 99, .. }),
            "unexpected error: {err}"
        );
    }
}
