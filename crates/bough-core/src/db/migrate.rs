//! Idempotent forward-only migration + 3 sanctioned reshapes (port of
//! `src/db/migrate.ts`).
//!
//! The invariant: **migration is forward-only and idempotent.** Applying it to
//! a fresh file and applying it to a file that already has every table must
//! leave the same database and must never fail. Nothing in this file swallows
//! an error. `user_version` is the forward-only guard — a database written by
//! a future bough is refused loudly at open rather than silently half-read.
//!
//! Frozen-schema rule: new columns require a named, PRAGMA-guarded reshape
//! function with a prose paragraph; ALTER TABLE appends, so columns go at the
//! END of the table. Three of these is not yet a ladder; a fourth without a
//! paragraph would be.

use rusqlite::Connection;

use crate::errors::BoughError;

/// The schema generation this build writes and understands. Not a migration
/// step counter — there are no steps.
pub const SCHEMA_VERSION: i64 = 1;

/// The frozen schema, vendored verbatim from `src/db/schema.sql`.
pub fn schema_sql() -> &'static str {
    include_str!("schema.sql")
}

fn sql_err(e: rusqlite::Error) -> BoughError {
    BoughError::bad_request(format!("sqlite: {e}"))
}

/// Apply the frozen schema to `db` and stamp its version.
///
/// Safe to call on every open: on a fresh file it creates everything, and on
/// an existing one every statement is a no-op. Returns the `user_version` the
/// file was at *before* this call, so a caller can tell a first open (0) from
/// a reopen. Throws when the file was written by a newer bough than this one —
/// forward-only means exactly that, and there is no downgrade path.
pub fn migrate(db: &Connection) -> Result<i64, BoughError> {
    let found = user_version(db)?;
    if found > SCHEMA_VERSION {
        return Err(BoughError::bad_request(format!(
            "this database was written by a newer bough (schema v{found}, this build \
understands v{SCHEMA_VERSION}). Opening it would silently downgrade the \
data. Upgrade bough, or point BOUGH_DB at a different file."
        )));
    }
    rebuild_day_one_command_history(db)?;
    add_schedule_session_id(db)?;
    add_command_message_id(db)?;
    add_session_description(db)?;
    db.execute_batch(schema_sql()).map_err(sql_err)?;
    if found < SCHEMA_VERSION {
        set_user_version(db, SCHEMA_VERSION)?;
    }
    Ok(found)
}

fn table_exists(db: &Connection, name: &str) -> Result<bool, BoughError> {
    db.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?")
        .and_then(|mut stmt| stmt.exists([name]))
        .map_err(sql_err)
}

fn has_column(db: &Connection, table: &str, column: &str) -> Result<bool, BoughError> {
    // PRAGMA table_info takes no bound parameter; `table` here is always a
    // literal from this module, never caller input.
    let mut stmt = db
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sql_err)?;
    let mut rows = stmt.query([]).map_err(sql_err)?;
    while let Some(row) = rows.next().map_err(sql_err)? {
        let name: String = row.get(1).map_err(sql_err)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The fourth sanctioned reshape: sessions gained `description` (2026-08) —
/// the rolling summary's one line, rewritten by the cheap tier as the session's
/// log grows. A session is a USER RECORD, so this ALTERs in place; existing
/// rows get NULL, which is exactly "not summarized yet", the same state a new
/// session starts in. Idempotent by PRAGMA check; no-op on a fresh file, where
/// `schema.sql` creates the column directly.
fn add_session_description(db: &Connection) -> Result<(), BoughError> {
    if !table_exists(db, "sessions")? {
        return Ok(());
    }
    if has_column(db, "sessions", "description")? {
        return Ok(());
    }
    db.execute_batch("ALTER TABLE sessions ADD COLUMN description TEXT")
        .map_err(sql_err)
}

/// The third sanctioned reshape: command_history gained `message_id` (2026-08)
/// — the supervisor message whose program ran the command, so a recalled
/// command reaches the round around it. ALTER rather than rebuild, unlike the
/// day-one reshape below: the memory is worth more now than it was with a
/// handful of rows, and existing commands losing only their link to a program
/// is a far smaller loss than losing the commands.
fn add_command_message_id(db: &Connection) -> Result<(), BoughError> {
    if !table_exists(db, "command_history")? {
        return Ok(());
    }
    if has_column(db, "command_history", "message_id")? {
        return Ok(());
    }
    db.execute_batch("ALTER TABLE command_history ADD COLUMN message_id TEXT")
        .map_err(sql_err)
}

/// The second sanctioned reshape: schedules gained `session_id` (2026-08) —
/// the conversation each firing reports back to. A schedule is a USER RECORD,
/// not a cache, so this one ALTERs in place and keeps every row (existing
/// schedules get NULL: they report to nobody, which is the pre-change
/// behavior). Idempotent by PRAGMA check; no-op on a fresh file, where
/// `schema.sql` creates the column directly.
fn add_schedule_session_id(db: &Connection) -> Result<(), BoughError> {
    if !table_exists(db, "schedules")? {
        return Ok(());
    }
    if has_column(db, "schedules", "session_id")? {
        return Ok(());
    }
    db.execute_batch("ALTER TABLE schedules ADD COLUMN session_id TEXT")
        .map_err(sql_err)
}

/// The first sanctioned reshape, and a deliberate exception to "no migration
/// ladder": command_history gained `output_head`/`spill_path` the day after it
/// shipped (2026-08). A file whose command_history predates the columns has
/// its command-history GROUP dropped and recreated empty by the schema exec
/// that follows — the memory is an accumulating cache, not a record. Deleted
/// rows' embeddings become orphans the embed layer never returns. No-op on
/// every database born after.
fn rebuild_day_one_command_history(db: &Connection) -> Result<(), BoughError> {
    if !table_exists(db, "command_history")? {
        return Ok(());
    }
    if has_column(db, "command_history", "output_head")? {
        return Ok(());
    }
    db.execute_batch(
        "DROP TABLE IF EXISTS command_history_fts;
         DROP TABLE IF EXISTS command_dirs;
         DROP TABLE IF EXISTS command_tags;
         DROP TABLE IF EXISTS command_history;",
    )
    .map_err(sql_err)
}

/// The file's stamped schema generation; 0 for a database this never touched.
pub fn user_version(db: &Connection) -> Result<i64, BoughError> {
    db.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| BoughError::bad_request(format!("user_version read failed: {e}")))
}

/// `PRAGMA user_version` takes no bound parameter, so the value is
/// interpolated — guarded because an interpolated non-integer would be a SQL
/// injection in the one place this module writes SQL by concatenation.
fn set_user_version(db: &Connection, version: i64) -> Result<(), BoughError> {
    if version < 0 {
        return Err(BoughError::bad_request(format!(
            "refusing to stamp a non-integer schema version: {version}"
        )));
    }
    db.execute_batch(&format!("PRAGMA user_version = {version}"))
        .map_err(sql_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_reports_the_version_it_found_and_stamps_the_current_one() {
        let raw = Connection::open_in_memory().unwrap();
        assert_eq!(user_version(&raw).unwrap(), 0);
        assert_eq!(migrate(&raw).unwrap(), 0, "a fresh file is at version 0");
        assert_eq!(user_version(&raw).unwrap(), SCHEMA_VERSION);
        assert_eq!(
            migrate(&raw).unwrap(),
            SCHEMA_VERSION,
            "a second run finds the stamp it left"
        );
        assert_eq!(user_version(&raw).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn migration_is_forward_only_a_newer_schema_version_is_refused() {
        let raw = Connection::open_in_memory().unwrap();
        raw.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1))
            .unwrap();
        let err = migrate(&raw).expect_err("a newer file must be refused");
        let msg = err.to_string();
        // The error must name both versions, not just say 'failed'.
        assert!(msg.contains("newer bough"), "{msg}");
        assert!(msg.contains(&format!("v{}", SCHEMA_VERSION + 1)), "{msg}");
        assert!(msg.contains(&format!("v{SCHEMA_VERSION}")), "{msg}");
    }

    #[test]
    fn dropped_columns_are_absent_from_the_schema() {
        // The port's promise, made checkable: archived_at / deprecated_at /
        // first_output_at and message_embeddings do not exist, so no caller
        // can start depending on them.
        let raw = Connection::open_in_memory().unwrap();
        migrate(&raw).unwrap();
        for gone in ["archived_at", "deprecated_at"] {
            assert!(
                !has_column(&raw, "sessions", gone).unwrap(),
                "sessions.{gone} must not exist"
            );
        }
        assert!(
            !has_column(&raw, "turns", "first_output_at").unwrap(),
            "turns.first_output_at must not exist"
        );
        assert!(
            !table_exists(&raw, "message_embeddings").unwrap(),
            "message_embeddings must not exist"
        );
    }
}
