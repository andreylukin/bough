//! Invariant: append-only is enforced BELOW the Rust API (V1). `UPDATE`/`DELETE` on `steps`,
//! `step_refs` and `edges` abort at the sqlite level; `rollups` accepts exactly one NULL → value
//! write to `superseded_by` and nothing else moving; `agents` carries no triggers at all, because
//! §3 exempts it as mutable config.

use bough_plugin_ledger::LedgerError;
use rusqlite::Connection;

/// The whole §3 schema of the phase plan §2.8: tables, indexes, FTS5, and the triggers.
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Create the schema if it is absent and check `PRAGMA user_version`.
///
/// A version this binary does not speak is [`LedgerError::FormatVersion`] — loud, at open, never a
/// silent migration.
pub fn open_and_migrate(conn: &Connection, path: &str) -> Result<(), LedgerError> {
    todo!("WP-2: schema::open_and_migrate")
}

/// The declared ENVELOPE — table and column names of `steps`/`edges`/`rollups`, in order — which
/// [`bough_plugin_ledger::envelope_fingerprint`] hashes. Step types are not part of it.
pub fn envelope() -> Vec<(&'static str, Vec<&'static str>)> {
    todo!("WP-2: schema::envelope")
}
