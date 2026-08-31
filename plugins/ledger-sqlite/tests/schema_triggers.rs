//! Invariant under test: append-only lives BELOW the Rust API (V1). These tests never touch
//! `LedgerStore` — they hold a raw `rusqlite::Connection` and try exactly what a stray script or a
//! future bug would try. `steps`, `step_refs` and `edges` refuse UPDATE and DELETE outright;
//! `rollups` accepts the single NULL → value write to `superseded_by` and nothing else; `agents`
//! accepts both, because §3 exempts it as mutable config.

use bough_plugin_ledger::{LedgerError, LEDGER_FORMAT_VERSION};
use bough_plugin_ledger_sqlite::schema::open_and_migrate;
use rusqlite::Connection;

/// A migrated db with one row in each append-only table.
fn seeded() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory db");
    open_and_migrate(&conn, ":memory:").expect("fresh db migrates");
    conn.execute_batch(
        "INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
           VALUES ('s1', 't', 1, '2026-01-01T00:00:00+00:00', 'w', 'step/start', 'thought',
                   '{\"index\":0}', '[]', 0);
         INSERT INTO step_refs (step_id, ref) VALUES ('s1', 'gh:o/r#1');
         INSERT INTO edges (child_traj, parent_traj, at_seq, kind, at)
           VALUES ('c', 't', 1, 'ancestor', '2026-01-01T00:00:00+00:00');
         INSERT INTO rollups (id, traj_id, kind, tier, from_seq, to_seq, src_trajs, body,
                              notable_refs, prompt_ver, sealed_at, superseded_by)
           VALUES ('r1', 't', 'tier', 1, 1, 9, '[]', '{}', '[]', 'v1',
                   '2026-01-01T00:00:00+00:00', NULL);
         INSERT INTO rollups (id, traj_id, kind, tier, from_seq, to_seq, src_trajs, body,
                              notable_refs, prompt_ver, sealed_at, superseded_by)
           VALUES ('r2', 't', 'tier', 1, 1, 9, '[]', '{}', '[]', 'v2',
                   '2026-01-01T00:00:00+00:00', NULL);
         INSERT INTO agents (name, traj_id, routing_refs, wake_classes, model_override,
                             tick_floor, digest_rollup_id)
           VALUES ('sol', 't', '[]', '[]', NULL, NULL, NULL);",
    )
    .expect("seed rows insert");
    conn
}

/// The abort message the trigger raised, so a test asserts on the RULE and not just on failure.
fn refusal(conn: &Connection, sql: &str) -> String {
    conn.execute(sql, [])
        .expect_err("the trigger must abort this statement")
        .to_string()
}

#[test]
fn update_on_steps_is_refused_by_the_trigger() {
    let conn = seeded();
    let msg = refusal(&conn, "UPDATE steps SET body = '{}' WHERE id = 's1'");
    assert!(msg.contains("append-only"), "unhelpful refusal: {msg}");
    let body: String = conn
        .query_row("SELECT body FROM steps WHERE id = 's1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(body, "{\"index\":0}", "the row changed anyway");
}

#[test]
fn delete_on_steps_is_refused_by_the_trigger() {
    let conn = seeded();
    let msg = refusal(&conn, "DELETE FROM steps WHERE id = 's1'");
    assert!(msg.contains("append-only"), "unhelpful refusal: {msg}");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM steps", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn update_on_edges_is_refused() {
    let conn = seeded();
    let msg = refusal(&conn, "UPDATE edges SET at_seq = 99 WHERE child_traj = 'c'");
    assert!(msg.contains("append-only"), "unhelpful refusal: {msg}");
}

#[test]
fn delete_on_edges_is_refused() {
    let conn = seeded();
    let msg = refusal(&conn, "DELETE FROM edges WHERE child_traj = 'c'");
    assert!(msg.contains("append-only"), "unhelpful refusal: {msg}");
}

#[test]
fn delete_on_step_refs_is_refused() {
    let conn = seeded();
    let msg = refusal(&conn, "DELETE FROM step_refs WHERE step_id = 's1'");
    assert!(msg.contains("append-only"), "unhelpful refusal: {msg}");
    // The matching index is canonical (§3): losing a ref silently would unroute an agent.
    let msg = refusal(&conn, "UPDATE step_refs SET ref = 'x' WHERE step_id = 's1'");
    assert!(msg.contains("append-only"), "unhelpful refusal: {msg}");
}

#[test]
fn delete_on_rollups_is_refused() {
    let conn = seeded();
    let msg = refusal(&conn, "DELETE FROM rollups WHERE id = 'r1'");
    assert!(msg.contains("sealed"), "unhelpful refusal: {msg}");
}

#[test]
fn superseded_by_can_be_set_once() {
    let conn = seeded();
    conn.execute(
        "UPDATE rollups SET superseded_by = 'r2' WHERE id = 'r1'",
        [],
    )
    .expect("NULL -> value is the one permitted write");
    let by: Option<String> = conn
        .query_row(
            "SELECT superseded_by FROM rollups WHERE id = 'r1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(by.as_deref(), Some("r2"));
}

#[test]
fn a_second_supersession_is_refused_by_the_trigger() {
    let conn = seeded();
    conn.execute(
        "UPDATE rollups SET superseded_by = 'r2' WHERE id = 'r1'",
        [],
    )
    .expect("the first supersession stands");
    let msg = refusal(
        &conn,
        "UPDATE rollups SET superseded_by = 'r3' WHERE id = 'r1'",
    );
    assert!(msg.contains("set-once"), "unhelpful refusal: {msg}");
    // And it cannot be cleared back to NULL either.
    let msg = refusal(
        &conn,
        "UPDATE rollups SET superseded_by = NULL WHERE id = 'r1'",
    );
    assert!(msg.contains("set-once"), "unhelpful refusal: {msg}");
}

#[test]
fn an_update_touching_another_rollup_column_is_refused() {
    let conn = seeded();
    let msg = refusal(
        &conn,
        "UPDATE rollups SET superseded_by = 'r2', body = '{\"edited\":true}' WHERE id = 'r1'",
    );
    assert!(msg.contains("set-once"), "unhelpful refusal: {msg}");
    let msg = refusal(&conn, "UPDATE rollups SET tier = 2 WHERE id = 'r1'");
    assert!(msg.contains("set-once"), "unhelpful refusal: {msg}");
}

/// §3 exempts `agents` from append-only in so many words. The absence of a trigger is a decision,
/// so it gets a test of its own rather than being read off the schema by eye.
#[test]
fn the_agents_table_accepts_update_and_delete() {
    let conn = seeded();
    conn.execute(
        "UPDATE agents SET model_override = 'terra' WHERE name = 'sol'",
        [],
    )
    .expect("agents is mutable config");
    conn.execute("DELETE FROM agents WHERE name = 'sol'", [])
        .expect("agents rows are deletable");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn opening_a_db_with_a_foreign_format_version_fails_loud() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");

    // A db written by some other build of bough: schema present, version not ours.
    let conn = Connection::open(&path).expect("create");
    open_and_migrate(&conn, &path.to_string_lossy()).expect("fresh db migrates");
    conn.execute_batch(&format!(
        "PRAGMA user_version = {};",
        LEDGER_FORMAT_VERSION + 7
    ))
    .unwrap();
    drop(conn);

    let conn = Connection::open(&path).expect("reopen");
    let err = open_and_migrate(&conn, &path.to_string_lossy())
        .expect_err("a foreign format version must refuse to open");
    match err {
        LedgerError::FormatVersion {
            found, expected, ..
        } => {
            assert_eq!(
                (found, expected),
                (LEDGER_FORMAT_VERSION + 7, LEDGER_FORMAT_VERSION)
            );
        }
        other => panic!("expected FormatVersion, got: {other}"),
    }
    // And the message names the file, so the operator knows which ledger to look at.
    assert!(open_and_migrate(&conn, &path.to_string_lossy())
        .unwrap_err()
        .to_string()
        .contains(&path.to_string_lossy().to_string()));
}
