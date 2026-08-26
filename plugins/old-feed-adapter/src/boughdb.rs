//! Invariant: `~/.bough/bough.db` is opened READ-ONLY, always, and NOTHING read here becomes mail.
//!
//! `command_history` / `command_tags` are COMPETENCE MEMORY: §14's cheap win is that they prime a
//! query, never that they are delivered — no agent should receive every shell command as an event
//! (§17). `note_sections` is the other half: cited evidence, each section carrying a
//! `note:<note>#<ord>` cite so a claim built on it can say where it came from.
//!
//! The old daemon's shape, as it stands on disk today:
//!
//! ```sql
//! command_history (id INTEGER PK, ts INTEGER, repo TEXT, cmd TEXT, tags TEXT,
//!                  exit_code INTEGER, output_head TEXT, …)
//! command_tags    (command_id INTEGER, tag TEXT)
//! note_sections   (id INTEGER PK, note_id INTEGER, ord INTEGER, heading TEXT, body TEXT,
//!                  author TEXT, created_at INTEGER, updated_at INTEGER)
//! ```

use std::collections::BTreeSet;
use std::path::Path;

use bough_plugin_ledger::{Cite, Ref};
use rusqlite::Connection;

use crate::jungler::{self, ts_to_utc};
use crate::{CommandMemory, NoteEvidence, NoteQuery, OldFeedError, PrimingQuery};

/// Open the old bough db READ-ONLY. An absent file is `Ok(None)`: the priming half is simply not
/// available, which is never a failure of the row (§14, V7).
pub fn open(path: &Path) -> Result<Option<Connection>, OldFeedError> {
    if !path.exists() {
        return Ok(None);
    }
    jungler::open(path).map(Some)
}

/// Which of the two priming sources this db can serve.
pub fn probe(conn: &Connection) -> Result<(bool, bool), OldFeedError> {
    let tables = jungler::table_names(conn)?;
    Ok((
        tables.contains("command_history"),
        tables.contains("note_sections"),
    ))
}

/// §14's cheap win: command memory, filtered by repo and tag, newest first.
///
/// The tags come from `command_tags` (the normalized index), not from the denormalized `tags`
/// string, so a query and its answer are read through the same table.
pub fn prime(
    conn: &Connection,
    q: &PrimingQuery,
    limit: usize,
) -> Result<Vec<CommandMemory>, OldFeedError> {
    let cols = jungler::columns(conn, "command_history")?;
    let mut wheres: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(repo) = &q.repo {
        if cols.contains("repo") {
            params.push(Box::new(repo.clone()));
            wheres.push(format!("\"repo\" = ?{}", params.len()));
        }
    }
    if let Some(text) = &q.contains {
        params.push(Box::new(format!("%{text}%")));
        wheres.push(format!("\"cmd\" LIKE ?{}", params.len()));
    }
    if !q.tags.is_empty() {
        let mut slots = Vec::new();
        for tag in &q.tags {
            params.push(Box::new(tag.clone()));
            slots.push(format!("?{}", params.len()));
        }
        wheres.push(format!(
            "EXISTS (SELECT 1 FROM \"command_tags\" t WHERE t.command_id = h.id AND t.tag IN ({}))",
            slots.join(", ")
        ));
    }
    params.push(Box::new(limit as i64));
    let limit_slot = params.len();

    let sql = format!(
        "SELECT h.id, h.\"cmd\", {}, {}, {}, {} FROM \"command_history\" h {} \
         ORDER BY {} h.id DESC LIMIT ?{limit_slot}",
        col(&cols, "repo"),
        col(&cols, "ts"),
        col(&cols, "exit_code"),
        col(&cols, "output_head"),
        if wheres.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", wheres.join(" AND "))
        },
        if cols.contains("ts") {
            "h.\"ts\" DESC,"
        } else {
            ""
        },
    );

    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(refs.as_slice(), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<i64>>(3)?.unwrap_or_default(),
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        ))
    })?;
    let mut out: Vec<(i64, CommandMemory)> = Vec::new();
    for row in rows {
        let (id, cmd, repo, ts, exit_code, output_head) = row?;
        out.push((
            id,
            CommandMemory {
                cmd,
                tags: Vec::new(),
                repo,
                at: ts_to_utc(ts),
                exit_code,
                output_head,
            },
        ));
    }
    // The tags of the rows that survived the filter, one small query per row: the limit is a
    // priming limit (tens), so this is bounded by construction.
    let mut tagged = Vec::with_capacity(out.len());
    let mut stmt = conn.prepare("SELECT tag FROM \"command_tags\" WHERE command_id = ?1")?;
    for (id, mut memory) in out {
        let tags = stmt.query_map([id], |r| r.get::<_, String>(0))?;
        let mut set = BTreeSet::new();
        for t in tags {
            set.insert(t?);
        }
        memory.tags = set.into_iter().collect();
        tagged.push(memory);
    }
    Ok(tagged)
}

/// `note_sections` as CITED EVIDENCE. Ordered `(note, ord)` so a note reads in its own order.
pub fn notes(
    conn: &Connection,
    q: &NoteQuery,
    limit: usize,
) -> Result<Vec<NoteEvidence>, OldFeedError> {
    let cols = jungler::columns(conn, "note_sections")?;
    let mut wheres: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(text) = &q.contains {
        params.push(Box::new(format!("%{text}%")));
        let a = params.len();
        params.push(Box::new(format!("%{text}%")));
        let b = params.len();
        wheres.push(format!("(\"heading\" LIKE ?{a} OR \"body\" LIKE ?{b})"));
    }
    params.push(Box::new(limit as i64));
    let limit_slot = params.len();

    let sql = format!(
        "SELECT \"note_id\", \"ord\", {}, {}, {} FROM \"note_sections\" {} \
         ORDER BY \"note_id\" ASC, \"ord\" ASC LIMIT ?{limit_slot}",
        col(&cols, "heading"),
        col(&cols, "body"),
        col(&cols, "author"),
        if wheres.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", wheres.join(" AND "))
        },
    );
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(refs.as_slice(), |r| {
        Ok(NoteEvidence {
            note: r.get(0)?,
            ord: r.get(1)?,
            heading: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            body: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            author: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            cite: Cite {
                r#ref: Ref::new("note:0#0"),
                url: None,
            },
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        let mut ev = r?;
        ev.cite = note_cite(ev.note, ev.ord);
        out.push(ev);
    }
    Ok(out)
}

/// The one spelling of a note-section cite: `note:<note>#<ord>`.
pub fn note_cite(note: i64, ord: i64) -> Cite {
    Cite {
        r#ref: Ref::new(format!("note:{note}#{ord}")),
        url: None,
    }
}

/// `"repo"`, or `NULL AS "repo"` when the column is not there. The defensive column reader
/// again: an old db that predates a column reads it as NULL instead of failing the query.
fn col(cols: &BTreeSet<String>, name: &str) -> String {
    if cols.contains(name) {
        format!("\"{name}\"")
    } else {
        format!("NULL AS \"{name}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_cite_names_the_section() {
        assert_eq!(note_cite(4, 2).r#ref.as_str(), "note:4#2");
    }
}
