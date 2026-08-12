//! Every query the note memory runs.
//!
//! Split out of `sqlite_db.rs` rather than added to it: the note tables are a
//! coherent group with their own joins, and the trait impl there delegates
//! here so the SQL stays in `db/` (architecture.md §1) without that file
//! growing another thousand lines.
//!
//! TWO INVARIANTS THIS FILE HOLDS.
//!
//! **A section write is one transaction** — revision push, body update, tag
//! replacement, citation replacement and the FTS row. A half-written section
//! whose FTS row still describes the previous body is a search index that
//! lies, and the same reasoning as `record_command`'s single transaction.
//!
//! **Subset matching happens in SQL, not in Rust.** A section surfaces when
//! its tags are a subset of the reader's context, which is
//! `NOT EXISTS (a tag of mine outside your set)` — a form the
//! `section_tags(tag, section_id)` index answers directly. Pulling every
//! section into memory to filter would make resolution cost scale with the
//! whole note memory rather than with the context.

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use crate::errors::BoughError;
use crate::types::{
    Citation, NoteAuthor, NoteLogRow, NoteRow, SectionRevision, SectionRow, SectionWrite,
};

fn db_err(e: rusqlite::Error) -> BoughError {
    BoughError::bad_request(format!("note memory: {e}"))
}

/// `?,?,?` for an IN clause of `n` values.
fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

pub fn upsert_note(
    conn: &Connection,
    path: &str,
    title: &str,
    tags: &[String],
    now: i64,
) -> Result<i64, BoughError> {
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    tx.execute(
        "INSERT INTO notes (path, title, created_at, updated_at, synced_ts)
         VALUES (?1, ?2, ?3, ?3, 0)
         ON CONFLICT(path) DO UPDATE SET title = ?2, updated_at = ?3",
        params![path, title, now],
    )
    .map_err(db_err)?;
    let id: i64 = tx
        .query_row("SELECT id FROM notes WHERE path = ?1", params![path], |r| {
            r.get(0)
        })
        .map_err(db_err)?;
    // Attachment is REPLACED, not merged: the tag set is the note's identity as
    // a query, and a merge would make removing a tag impossible.
    tx.execute("DELETE FROM note_tags WHERE note_id = ?1", params![id])
        .map_err(db_err)?;
    for tag in tags {
        tx.execute(
            "INSERT OR IGNORE INTO note_tags (note_id, tag) VALUES (?1, ?2)",
            params![id, tag],
        )
        .map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(id)
}

fn tags_for_note(conn: &Connection, note_id: i64) -> Result<Vec<String>, BoughError> {
    let mut stmt = conn
        .prepare("SELECT tag FROM note_tags WHERE note_id = ?1 ORDER BY tag")
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![note_id], |r| r.get::<_, String>(0))
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

fn note_from_row(conn: &Connection, row: &rusqlite::Row<'_>) -> Result<NoteRow, rusqlite::Error> {
    let id: i64 = row.get("id")?;
    Ok(NoteRow {
        id,
        path: row.get("path")?,
        title: row.get("title")?,
        tags: tags_for_note(conn, id).unwrap_or_default(),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        synced_ts: row.get("synced_ts")?,
        closed_at: row.get("closed_at")?,
    })
}

pub fn note_by_path(conn: &Connection, path: &str) -> Result<Option<NoteRow>, BoughError> {
    let mut stmt = conn
        .prepare("SELECT * FROM notes WHERE path = ?1")
        .map_err(db_err)?;
    stmt.query_row(params![path], |r| note_from_row(conn, r))
        .optional()
        .map_err(db_err)
}

pub fn list_notes(conn: &Connection) -> Result<Vec<NoteRow>, BoughError> {
    let mut stmt = conn
        .prepare("SELECT * FROM notes ORDER BY path")
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |r| note_from_row(conn, r))
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

pub fn notes_for_tags(conn: &Connection, tags: &[String]) -> Result<Vec<NoteRow>, BoughError> {
    if tags.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT n.* FROM notes n
           JOIN note_tags t ON t.note_id = n.id
          WHERE t.tag IN ({})
          GROUP BY n.id
          ORDER BY n.path",
        placeholders(tags.len())
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(params_from_iter(tags.iter()), |r| note_from_row(conn, r))
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

pub fn set_note_synced(conn: &Connection, note_id: i64, ts: i64) -> Result<(), BoughError> {
    // MAX, never assignment: a re-fold over an older window must not un-sync
    // rows already accounted for.
    conn.execute(
        "UPDATE notes SET synced_ts = MAX(synced_ts, ?2) WHERE id = ?1",
        params![note_id, ts],
    )
    .map_err(db_err)?;
    Ok(())
}

pub fn close_note(conn: &Connection, note_id: i64, at: i64) -> Result<(), BoughError> {
    conn.execute(
        "UPDATE notes SET closed_at = ?2 WHERE id = ?1",
        params![note_id, at],
    )
    .map_err(db_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn tags_for_section(conn: &Connection, section_id: i64) -> Vec<String> {
    let Ok(mut stmt) =
        conn.prepare("SELECT tag FROM section_tags WHERE section_id = ?1 ORDER BY tag")
    else {
        return Vec::new();
    };
    stmt.query_map(params![section_id], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

fn citations_for_section(conn: &Connection, section_id: i64) -> Vec<Citation> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT kind, ref FROM section_citations WHERE section_id = ?1 ORDER BY kind, ref",
    ) else {
        return Vec::new();
    };
    stmt.query_map(params![section_id], |r| {
        Ok(Citation {
            kind: r.get(0)?,
            reference: r.get(1)?,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

fn section_from_row(
    conn: &Connection,
    row: &rusqlite::Row<'_>,
) -> Result<SectionRow, rusqlite::Error> {
    let id: i64 = row.get("id")?;
    Ok(SectionRow {
        id,
        note_id: row.get("note_id")?,
        note_path: row.get("note_path")?,
        ord: row.get("ord")?,
        heading: row.get("heading")?,
        body: row.get("body")?,
        tags: tags_for_section(conn, id),
        citations: citations_for_section(conn, id),
        author: NoteAuthor::parse(&row.get::<_, String>("author")?),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// The projection every section read shares: the section plus its note's path.
const SECTION_SELECT: &str = "SELECT s.id, s.note_id, n.path AS note_path, s.ord, s.heading,
        s.body, s.author, s.created_at, s.updated_at
   FROM note_sections s JOIN notes n ON n.id = s.note_id";

pub fn put_section(conn: &Connection, write: &SectionWrite, now: i64) -> Result<i64, BoughError> {
    let tx = conn.unchecked_transaction().map_err(db_err)?;

    let existing: Option<(i64, String, String, String, i64)> = tx
        .query_row(
            "SELECT id, heading, body, author, created_at FROM note_sections
              WHERE note_id = ?1 AND heading = ?2",
            params![write.note_id, write.heading],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(db_err)?;

    let id = match existing {
        Some((id, old_heading, old_body, old_author, _)) => {
            // HISTORY FIRST. The superseded body is pushed down before the new
            // one lands, so a warning cleared by a rewrite leaves the claim it
            // replaced on the record instead of vanishing.
            if old_body != write.body || old_heading != write.heading {
                let rev: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(rev), 0) + 1 FROM section_revisions
                          WHERE section_id = ?1",
                        params![id],
                        |r| r.get(0),
                    )
                    .map_err(db_err)?;
                tx.execute(
                    "INSERT INTO section_revisions
                       (section_id, rev, heading, body, author, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, rev, old_heading, old_body, old_author, now],
                )
                .map_err(db_err)?;
            }
            tx.execute(
                "UPDATE note_sections
                    SET ord = ?2, heading = ?3, body = ?4, author = ?5, updated_at = ?6
                  WHERE id = ?1",
                params![
                    id,
                    write.ord,
                    write.heading,
                    write.body,
                    write.author.as_str(),
                    now
                ],
            )
            .map_err(db_err)?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO note_sections
                   (note_id, ord, heading, body, author, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    write.note_id,
                    write.ord,
                    write.heading,
                    write.body,
                    write.author.as_str(),
                    now
                ],
            )
            .map_err(db_err)?;
            tx.last_insert_rowid()
        }
    };

    // Tags DEFAULT to the note's. That default is what keeps authoring
    // unchanged: a section written under `atlas:rollout:prod` appears only
    // there until someone narrows it on purpose.
    let tags: Vec<String> = match &write.tags {
        Some(tags) => tags.clone(),
        None => {
            let mut stmt = tx
                .prepare("SELECT tag FROM note_tags WHERE note_id = ?1")
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![write.note_id], |r| r.get::<_, String>(0))
                .map_err(db_err)?;
            rows.filter_map(Result::ok).collect()
        }
    };
    tx.execute(
        "DELETE FROM section_tags WHERE section_id = ?1",
        params![id],
    )
    .map_err(db_err)?;
    for tag in &tags {
        tx.execute(
            "INSERT OR IGNORE INTO section_tags (section_id, tag) VALUES (?1, ?2)",
            params![id, tag],
        )
        .map_err(db_err)?;
    }

    tx.execute(
        "DELETE FROM section_citations WHERE section_id = ?1",
        params![id],
    )
    .map_err(db_err)?;
    for c in &write.citations {
        tx.execute(
            "INSERT OR IGNORE INTO section_citations (section_id, kind, ref, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, c.kind, c.reference, now],
        )
        .map_err(db_err)?;
    }

    let path: String = tx
        .query_row(
            "SELECT path FROM notes WHERE id = ?1",
            params![write.note_id],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    tx.execute("DELETE FROM notes_fts WHERE section_id = ?1", params![id])
        .map_err(db_err)?;
    tx.execute(
        "INSERT INTO notes_fts (heading, body, path, section_id, note_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![write.heading, write.body, path, id, write.note_id],
    )
    .map_err(db_err)?;
    tx.execute(
        "UPDATE notes SET updated_at = ?2 WHERE id = ?1",
        params![write.note_id, now],
    )
    .map_err(db_err)?;

    tx.commit().map_err(db_err)?;
    Ok(id)
}

pub fn sections_for_note(conn: &Connection, note_id: i64) -> Result<Vec<SectionRow>, BoughError> {
    let sql = format!("{SECTION_SELECT} WHERE s.note_id = ?1 ORDER BY s.ord, s.id");
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(params![note_id], |r| section_from_row(conn, r))
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

/// Sections whose tag set is a SUBSET of `context`.
///
/// Expressed as "has at least one tag, and has no tag outside the context" —
/// a section with no tags at all would otherwise match every context
/// vacuously, which is the empty-set trap this shape avoids.
pub fn sections_for_context(
    conn: &Connection,
    context: &[String],
    exclude_note: Option<i64>,
) -> Result<Vec<SectionRow>, BoughError> {
    if context.is_empty() {
        return Ok(Vec::new());
    }
    let list = placeholders(context.len());
    let sql = format!(
        "{SECTION_SELECT}
          WHERE EXISTS (SELECT 1 FROM section_tags st
                         WHERE st.section_id = s.id AND st.tag IN ({list}))
            AND NOT EXISTS (SELECT 1 FROM section_tags st2
                             WHERE st2.section_id = s.id AND st2.tag NOT IN ({list}))
            AND (?{} IS NULL OR s.note_id <> ?{})
          ORDER BY s.updated_at DESC",
        context.len() * 2 + 1,
        context.len() * 2 + 1
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    for _ in 0..2 {
        for tag in context {
            args.push(Box::new(tag.clone()));
        }
    }
    args.push(Box::new(exclude_note));
    let rows = stmt
        .query_map(params_from_iter(args.iter()), |r| section_from_row(conn, r))
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

pub fn section_revisions(
    conn: &Connection,
    section_id: i64,
) -> Result<Vec<SectionRevision>, BoughError> {
    let mut stmt = conn
        .prepare(
            "SELECT rev, heading, body, author, created_at FROM section_revisions
              WHERE section_id = ?1 ORDER BY rev DESC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![section_id], |r| {
            Ok(SectionRevision {
                rev: r.get(0)?,
                heading: r.get(1)?,
                body: r.get(2)?,
                author: NoteAuthor::parse(&r.get::<_, String>(3)?),
                created_at: r.get(4)?,
            })
        })
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

pub fn delete_section(conn: &Connection, section_id: i64) -> Result<(), BoughError> {
    let tx = conn.unchecked_transaction().map_err(db_err)?;
    for sql in [
        "DELETE FROM section_tags WHERE section_id = ?1",
        "DELETE FROM section_citations WHERE section_id = ?1",
        "DELETE FROM notes_fts WHERE section_id = ?1",
        "DELETE FROM note_sections WHERE id = ?1",
    ] {
        tx.execute(sql, params![section_id]).map_err(db_err)?;
    }
    // Revisions SURVIVE a delete: the history of a claim is the record that it
    // was ever made, and dropping it with the section would be the silent loss
    // the revision table exists to prevent.
    tx.commit().map_err(db_err)?;
    Ok(())
}

pub fn search_sections(
    conn: &Connection,
    words: &[String],
    limit: i64,
) -> Result<Vec<SectionRow>, BoughError> {
    if words.is_empty() {
        return Ok(Vec::new());
    }
    // Each word quoted: a user typing `atlas:rollout` means two words, not FTS
    // operator syntax. Same treatment `search_commands` gives its input.
    let query = words
        .iter()
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ");
    let sql = format!(
        "{SECTION_SELECT}
           JOIN notes_fts f ON f.section_id = s.id
          WHERE notes_fts MATCH ?1
          ORDER BY rank
          LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(params![query, limit], |r| section_from_row(conn, r))
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

// ---------------------------------------------------------------------------
// The derived zone
// ---------------------------------------------------------------------------

pub fn append_note_log(
    conn: &Connection,
    note_id: i64,
    ts: i64,
    source: NoteAuthor,
    text: &str,
) -> Result<bool, BoughError> {
    // DEDUPLICATION AT WRITE TIME, which is what makes a later consolidation
    // rewrite unnecessary: the cheap tier fires per round, and a rollout that
    // runs the same check ten times must not produce ten identical lines.
    let last: Option<String> = conn
        .query_row(
            "SELECT text FROM note_log WHERE note_id = ?1 ORDER BY ts DESC, id DESC LIMIT 1",
            params![note_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    if last.as_deref() == Some(text) {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO note_log (note_id, ts, source, text) VALUES (?1, ?2, ?3, ?4)",
        params![note_id, ts, source.as_str(), text],
    )
    .map_err(db_err)?;
    Ok(true)
}

pub fn note_log(
    conn: &Connection,
    note_id: i64,
    limit: i64,
) -> Result<Vec<NoteLogRow>, BoughError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, ts, source, text FROM note_log
              WHERE note_id = ?1 ORDER BY ts DESC, id DESC LIMIT ?2",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![note_id, limit], |r| {
            Ok(NoteLogRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                source: NoteAuthor::parse(&r.get::<_, String>(2)?),
                text: r.get(3)?,
            })
        })
        .map_err(db_err)?;
    let mut out = rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    out.reverse(); // oldest first, the order a log is read in
    Ok(out)
}

/// THE CITATION GUARD.
///
/// A `command` citation must name a row that exists AND that carries one of the
/// section's tags. Both halves matter: existence stops an invented id, and the
/// tag check stops a real id that has nothing to do with the claim — which is
/// the shape a plausible-but-wrong citation actually takes.
///
/// `file`, `url` and `section` are accepted structurally (a path or a URL is
/// not a row to look up, and a section citation is checked by the caller that
/// resolves it).
pub fn citation_is_valid(
    conn: &Connection,
    kind: &str,
    reference: &str,
    tags: &[String],
) -> Result<bool, BoughError> {
    match kind {
        "command" => {
            let Ok(id) = reference.parse::<i64>() else {
                return Ok(false);
            };
            if tags.is_empty() {
                let n: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM command_history WHERE id = ?1",
                        params![id],
                        |r| r.get(0),
                    )
                    .map_err(db_err)?;
                return Ok(n > 0);
            }
            let sql = format!(
                "SELECT COUNT(*) FROM command_tags WHERE command_id = ?1 AND tag IN ({})",
                placeholders(tags.len())
            );
            let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(id)];
            for t in tags {
                args.push(Box::new(t.clone()));
            }
            let n: i64 = conn
                .query_row(&sql, params_from_iter(args.iter()), |r| r.get(0))
                .map_err(db_err)?;
            Ok(n > 0)
        }
        "message" => {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE id = ?1",
                    params![reference],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            Ok(n > 0)
        }
        "file" | "url" | "section" => Ok(!reference.trim().is_empty()),
        _ => Ok(false),
    }
}
