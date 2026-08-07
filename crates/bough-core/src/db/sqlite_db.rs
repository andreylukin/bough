//! `SqliteDb` — the concrete `Db` port over rusqlite (port of `src/db/db.ts`).
//!
//! The only place in the tree that speaks SQL. The three ordering rules:
//!
//! 1. `messages_for` orders by `(created_at, rowid)`, never `created_at`
//!    alone — branch seeding writes with a real clock, so a turn started
//!    immediately after a seed lands in the *same millisecond*.
//! 2. `thread_for` is every ancestor's messages root→parent, then the
//!    session's own.
//! 3. `ancestor_chain` walks `parent_id` to the lineage root, root first,
//!    inclusive of the session itself.
//!
//! Injection: the database path and the clock are constructor arguments.
//! `update_turn` is the one method that stamps a time of its own, and it
//! stamps the injected clock — so a test drives checkpoint ordering without
//! sleeping. Row→domain mappers here are the ONLY snake_case→camelCase
//! translation point. Storage conventions: timestamps are epoch ms integers,
//! booleans are 0/1, and anything structured is JSON text.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use super::migrate::migrate;
use crate::errors::BoughError;
use crate::paths::db_path;
use crate::schema::parts::{
    Message, Part, Schedule, Session, SessionKind, Turn, TurnStatus, Usage, WorkflowAgent,
    WorkflowPhase, WorkflowRun,
};
use crate::types::{
    system_clock, Clock, CommandRecord, CommandTagOpts, CommandTagRow, Db, PriorFailures,
    RecentFailure, SearchHit, SessionRuntime, StateEntry, TagDiversityDay, TaggedCommand,
    TurnPatch, UsageTotals, WorkflowAgentPatch, WorkflowPatch,
};

// ---- small helpers ----------------------------------------------------------

fn sql_err(e: rusqlite::Error) -> BoughError {
    BoughError::bad_request(format!("sqlite: {e}"))
}

/// Bridge a domain-mapping failure back through rusqlite's row-mapper type.
fn conv(e: BoughError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

/// A string-serde enum (`TurnStatus`, `SessionKind`, …) as its stored TEXT.
fn enum_str<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        other => unreachable!("enum did not serialize to a string: {other:?}"),
    }
}

/// The stored TEXT back to its enum; a value serde does not know is an error.
fn enum_val<T: DeserializeOwned>(s: &str) -> Result<T, BoughError> {
    serde_json::from_value(Value::String(s.to_owned()))
        .map_err(|e| BoughError::bad_request(format!("unrecognized stored value {s:?}: {e}")))
}

fn bit(v: bool) -> i64 {
    if v {
        1
    } else {
        0
    }
}

/// JSON columns: absent AND null both store as NULL, so a read round-trips.
fn json_col(v: &Option<Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(other) => Some(other.to_string()),
    }
}

/// The text a message contributes to the keyword index: its prose and its
/// reasoning. Tool calls, results and image paths are deliberately excluded —
/// a search over transcripts should find what was *said*.
fn indexable_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            Part::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

// ---- row → domain -----------------------------------------------------------
// One mapper per table; absent optionals come back as `None` — one shape per row.

fn to_session(row: &Row) -> rusqlite::Result<Session> {
    let kind: String = row.get("kind")?;
    Ok(Session {
        id: row.get("id")?,
        parent_id: row.get("parent_id")?,
        title: row.get("title")?,
        kind: enum_val::<SessionKind>(&kind).map_err(conv)?,
        created_at: row.get("created_at")?,
        workspace: row.get("workspace")?,
        origin_dir: row.get("origin_dir")?,
        base: row.get("base")?,
        origin_id: row.get("origin_id")?,
        origin_message_id: row.get("origin_message_id")?,
        model: row.get("model")?,
        effort: row.get("effort")?,
        draft: row.get("draft")?,
        context_tokens: row.get("context_tokens")?,
        cached_tokens: row.get("cached_tokens")?,
        last_llm_at: row.get("last_llm_at")?,
        outcome_ok: row.get::<_, Option<i64>>("outcome_ok")?.map(|v| v == 1),
    })
}

fn to_message(row: &Row) -> rusqlite::Result<Message> {
    let role: String = row.get("role")?;
    let parts: String = row.get("parts")?;
    Ok(Message {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        role: enum_val(&role).map_err(conv)?,
        parts: serde_json::from_str(&parts)
            .map_err(|e| conv(BoughError::bad_request(format!("corrupt parts JSON: {e}"))))?,
        pending: row.get::<_, i64>("pending")? == 1,
        created_at: row.get("created_at")?,
    })
}

/// A turn's `usage` is present once the provider has reported anything for it
/// — a turn that errored before its first round has none, and reporting zeros
/// there would be a claim we cannot make.
fn to_turn(row: &Row) -> rusqlite::Result<Turn> {
    let status: String = row.get("status")?;
    let input: Option<i64> = row.get("input_tokens")?;
    let output: Option<i64> = row.get("output_tokens")?;
    let reported = input.is_some() || output.is_some();
    Ok(Turn {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        message_id: row.get("message_id")?,
        status: enum_val::<TurnStatus>(&status).map_err(conv)?,
        step: row.get("step")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        error: row.get("error")?,
        usage: if reported {
            Some(Usage {
                input_tokens: input.unwrap_or(0),
                output_tokens: output.unwrap_or(0),
                reasoning_tokens: row.get("reasoning_tokens")?,
                cache_read_tokens: row.get("cache_read_tokens")?,
                cache_write_tokens: row.get("cache_write_tokens")?,
                cost_usd: row.get("cost_usd")?,
            })
        } else {
            None
        },
    })
}

fn to_schedule(row: &Row) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: row.get("id")?,
        title: row.get("title")?,
        prompt: row.get("prompt")?,
        workspace: row.get("workspace")?,
        spec: row.get("spec")?,
        enabled: row.get::<_, i64>("enabled")? == 1,
        created_at: row.get("created_at")?,
        last_run_at: row.get("last_run_at")?,
        next_run_at: row.get("next_run_at")?,
        session_id: row.get("session_id")?,
    })
}

fn to_workflow(row: &Row) -> rusqlite::Result<WorkflowRun> {
    let status: String = row.get("status")?;
    let phases: String = row.get("phases")?;
    let parse_json = |v: Option<String>| -> rusqlite::Result<Option<Value>> {
        match v {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| conv(BoughError::bad_request(format!("corrupt JSON column: {e}")))),
        }
    };
    Ok(WorkflowRun {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        name: row.get("name")?,
        description: row.get("description")?,
        script: row.get("script")?,
        phases: serde_json::from_str::<Vec<WorkflowPhase>>(&phases)
            .map_err(|e| conv(BoughError::bad_request(format!("corrupt phases JSON: {e}"))))?,
        status: enum_val(&status).map_err(conv)?,
        current_phase: row.get("current_phase")?,
        result: parse_json(row.get("result")?)?,
        error: row.get("error")?,
        args: parse_json(row.get("args")?)?,
        resume_of: row.get("resume_of")?,
        created_at: row.get("created_at")?,
        finished_at: row.get("finished_at")?,
    })
}

fn to_workflow_agent(row: &Row) -> rusqlite::Result<WorkflowAgent> {
    let status: String = row.get("status")?;
    Ok(WorkflowAgent {
        id: row.get("id")?,
        run_id: row.get("run_id")?,
        idx: row.get("idx")?,
        key: row.get("key")?,
        label: row.get("label")?,
        phase: row.get("phase")?,
        prompt: row.get("prompt")?,
        model: row.get("model")?,
        status: enum_val(&status).map_err(conv)?,
        result: row.get("result")?,
        error: row.get("error")?,
        session_id: row.get("session_id")?,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
    })
}

/// The raw turn columns `update_turn` merges over — kept as stored (Options),
/// because usage columns must round-trip NULLs, not zeros.
struct RawTurn {
    status: String,
    step: String,
    error: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    cost_usd: Option<f64>,
}

// ---- the handle -------------------------------------------------------------

/// How `open_db`/`SqliteDb` take their seams.
#[derive(Default)]
pub struct DbOptions {
    /// Injected clock; only `update_turn` reads it. Absent = the system clock.
    pub now: Option<Clock>,
}

/// The one concrete `Db`. Lives behind `Arc<Mutex<..>>` (`types::SharedDb`);
/// rusqlite `Connection` is `!Sync`.
pub struct SqliteDb {
    conn: Connection,
    now: Clock,
}

impl SqliteDb {
    pub fn new(path: &str, opts: DbOptions) -> Result<Self, BoughError> {
        let conn = Connection::open(path)
            .map_err(|e| BoughError::bad_request(format!("cannot open database {path}: {e}")))?;
        // Declared foreign keys are only enforced when this is on, and it is a
        // per-connection setting — off by default, so set at every open.
        conn.execute_batch("PRAGMA foreign_keys = ON")
            .map_err(sql_err)?;
        migrate(&conn)?;
        Ok(SqliteDb {
            conn,
            now: opts.now.unwrap_or_else(system_clock),
        })
    }

    /// Consume and close the handle. (The trait's `close(&self)` is a no-op —
    /// dropping the handle closes the connection.)
    pub fn close(self) {}

    fn all<T, F>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: F,
    ) -> Result<Vec<T>, BoughError>
    where
        F: FnMut(&Row) -> rusqlite::Result<T>,
    {
        let mut stmt = self.conn.prepare(sql).map_err(sql_err)?;
        let rows = stmt.query_map(params, f).map_err(sql_err)?;
        rows.collect::<rusqlite::Result<Vec<T>>>().map_err(sql_err)
    }

    fn one<T, F>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: F,
    ) -> Result<Option<T>, BoughError>
    where
        F: FnOnce(&Row) -> rusqlite::Result<T>,
    {
        self.conn
            .query_row(sql, params, f)
            .optional()
            .map_err(sql_err)
    }

    fn run(&self, sql: &str, params: impl rusqlite::Params) -> Result<(), BoughError> {
        self.conn.execute(sql, params).map(|_| ()).map_err(sql_err)
    }

    fn agent(&self, id: &str) -> Result<Option<WorkflowAgent>, BoughError> {
        self.one(
            "SELECT * FROM workflow_agents WHERE id = ?",
            [id],
            to_workflow_agent,
        )
    }
}

impl Db for SqliteDb {
    // ---- sessions -----------------------------------------------------------

    /// Insert and return the row *as stored*: `create_session(s)` and
    /// `get_session(s.id)` then agree field for field, so a caller can never
    /// hold a session carrying a value the database did not keep.
    fn create_session(&self, s: Session) -> Result<Session, BoughError> {
        let kind = enum_str(&s.kind);
        self.run(
            "INSERT INTO sessions
               (id, parent_id, title, kind, created_at, workspace, origin_dir, base,
                origin_id, origin_message_id, model, effort, draft)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                s.id,
                s.parent_id,
                s.title,
                kind,
                s.created_at,
                s.workspace,
                s.origin_dir,
                s.base,
                s.origin_id,
                s.origin_message_id,
                s.model,
                s.effort,
                s.draft,
            ],
        )?;
        self.get_session(&s.id)?
            .ok_or_else(|| BoughError::bad_request("createSession read-back found no row"))
    }

    fn get_session(&self, id: &str) -> Result<Option<Session>, BoughError> {
        self.one("SELECT * FROM sessions WHERE id = ?", [id], to_session)
    }

    fn get_session_runtime(&self, id: &str) -> Result<SessionRuntime, BoughError> {
        let r = self.one(
            "SELECT workspace, base FROM sessions WHERE id = ?",
            [id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )?;
        let (workspace, base) = r.unwrap_or((None, None));
        Ok(SessionRuntime { workspace, base })
    }

    /// Every session, newest first. No visibility filter: the *caller* derives
    /// that from `kind` + `originId`. Tie-broken by rowid so two sessions
    /// created in one millisecond have a stable order.
    fn list_sessions(&self) -> Result<Vec<Session>, BoughError> {
        self.all(
            "SELECT * FROM sessions ORDER BY created_at DESC, rowid DESC",
            [],
            to_session,
        )
    }

    /// The branches collapsed under `originId`, in creation order — the drill-in.
    fn sessions_by_origin(&self, origin_id: &str) -> Result<Vec<Session>, BoughError> {
        self.all(
            "SELECT * FROM sessions WHERE origin_id = ? ORDER BY created_at, rowid",
            [origin_id],
            to_session,
        )
    }

    /// Root→self, inclusive; `[]` for an unknown id. The `seen` set is what
    /// stops a cycle introduced by a bad write from hanging the server.
    fn ancestor_chain(&self, id: &str) -> Result<Vec<Session>, BoughError> {
        let mut chain: Vec<Session> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cur = self.get_session(id)?;
        while let Some(s) = cur {
            if seen.contains(&s.id) {
                break;
            }
            seen.insert(s.id.clone());
            let parent = s.parent_id.clone();
            chain.push(s);
            cur = match parent {
                Some(pid) => self.get_session(&pid)?,
                None => None,
            };
        }
        chain.reverse();
        Ok(chain)
    }

    fn set_session_title(&self, id: &str, title: &str) -> Result<(), BoughError> {
        self.run(
            "UPDATE sessions SET title = ? WHERE id = ?",
            params![title, id],
        )
    }

    fn set_session_workspace(&self, id: &str, workspace: &str) -> Result<(), BoughError> {
        self.run(
            "UPDATE sessions SET workspace = ? WHERE id = ?",
            params![workspace, id],
        )
    }

    fn set_session_base(&self, id: &str, base: &str) -> Result<(), BoughError> {
        self.run(
            "UPDATE sessions SET base = ? WHERE id = ?",
            params![base, id],
        )
    }

    /// Set by handoff; cleared with `None` by the first posted message.
    fn set_session_draft(&self, id: &str, draft: Option<&str>) -> Result<(), BoughError> {
        self.run(
            "UPDATE sessions SET draft = ? WHERE id = ?",
            params![draft, id],
        )
    }

    /// `None` clears the pin back to the global default.
    fn set_session_model(&self, id: &str, model: Option<&str>) -> Result<(), BoughError> {
        self.run(
            "UPDATE sessions SET model = ? WHERE id = ?",
            params![model, id],
        )
    }

    fn set_session_effort(&self, id: &str, effort: Option<&str>) -> Result<(), BoughError> {
        self.run(
            "UPDATE sessions SET effort = ? WHERE id = ?",
            params![effort, id],
        )
    }

    /// Whether the delegated TURN errored. Not an acceptance gate.
    fn set_session_outcome(&self, id: &str, ok: bool) -> Result<(), BoughError> {
        self.run(
            "UPDATE sessions SET outcome_ok = ? WHERE id = ?",
            params![bit(ok), id],
        )
    }

    /// Fold one round's usage into the session. Two different things happen
    /// here and conflating them is the classic bug: the cost columns
    /// ACCUMULATE across the session, while `context_tokens` / `cached_tokens`
    /// / `last_llm_at` are OVERWRITTEN — they describe the last round only,
    /// because the context meter is a gauge, not a total.
    fn add_session_usage(&self, id: &str, usage: &Usage, at: i64) -> Result<(), BoughError> {
        let read = usage.cache_read_tokens.unwrap_or(0);
        let write = usage.cache_write_tokens.unwrap_or(0);
        self.run(
            "UPDATE sessions SET
               input_tokens      = COALESCE(input_tokens, 0) + ?,
               output_tokens     = COALESCE(output_tokens, 0) + ?,
               reasoning_tokens  = COALESCE(reasoning_tokens, 0) + ?,
               cache_read_total  = COALESCE(cache_read_total, 0) + ?,
               cache_write_total = COALESCE(cache_write_total, 0) + ?,
               cost_usd          = COALESCE(cost_usd, 0) + ?,
               context_tokens    = ?,
               cached_tokens     = ?,
               last_llm_at       = ?
             WHERE id = ?",
            params![
                usage.input_tokens,
                usage.output_tokens,
                usage.reasoning_tokens.unwrap_or(0),
                read,
                write,
                usage.cost_usd.unwrap_or(0.0),
                usage.input_tokens + read + write,
                read + write,
                at,
                id,
            ],
        )
    }

    fn session_usage(&self, id: &str) -> Result<UsageTotals, BoughError> {
        type Cols = (
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<f64>,
        );
        let r: Option<Cols> = self.one(
            "SELECT input_tokens, output_tokens, reasoning_tokens, cache_read_total,
                    cache_write_total, cost_usd
               FROM sessions WHERE id = ?",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        let (input, output, reasoning, read, write, cost) =
            r.unwrap_or((None, None, None, None, None, None));
        Ok(UsageTotals {
            input_tokens: input.unwrap_or(0),
            output_tokens: output.unwrap_or(0),
            reasoning_tokens: reasoning.unwrap_or(0),
            cache_read_tokens: read.unwrap_or(0),
            cache_write_tokens: write.unwrap_or(0),
            cost_usd: cost.unwrap_or(0.0),
        })
    }

    /// The session plus every branch that collapsed under it, transitively.
    /// Follows `origin_id` but only through `subagent` / `workflow_agent`
    /// rows: a fork or a compaction is a sibling the user opened deliberately.
    /// `UNION` (not `UNION ALL`) so a cyclic `origin_id` terminates.
    fn tree_usage(&self, id: &str) -> Result<UsageTotals, BoughError> {
        self.conn
            .query_row(
                "WITH RECURSIVE tree(id) AS (
                   SELECT id FROM sessions WHERE id = ?
                   UNION
                   SELECT s.id FROM sessions s JOIN tree t ON s.origin_id = t.id
                    WHERE s.kind IN ('subagent', 'workflow_agent')
                 )
                 SELECT COALESCE(SUM(input_tokens), 0)      AS input,
                        COALESCE(SUM(output_tokens), 0)     AS output,
                        COALESCE(SUM(reasoning_tokens), 0)  AS reasoning,
                        COALESCE(SUM(cache_read_total), 0)  AS read,
                        COALESCE(SUM(cache_write_total), 0) AS write,
                        COALESCE(SUM(cost_usd), 0)          AS cost
                   FROM sessions WHERE id IN (SELECT id FROM tree)",
                [id],
                |row| {
                    Ok(UsageTotals {
                        input_tokens: row.get("input")?,
                        output_tokens: row.get("output")?,
                        reasoning_tokens: row.get("reasoning")?,
                        cache_read_tokens: row.get("read")?,
                        cache_write_tokens: row.get("write")?,
                        cost_usd: row.get("cost")?,
                    })
                },
            )
            .map_err(sql_err)
    }

    /// Sessions with a turn in flight. Read from `turns`, not from a pending
    /// message: an orphaned turn's message can still be pending after a
    /// restart, and a session that looks busy forever is exactly the hang
    /// recovery exists to prevent.
    fn busy_session_ids(&self) -> Result<HashSet<String>, BoughError> {
        let rows = self.all(
            "SELECT DISTINCT session_id FROM turns WHERE status = 'running'",
            [],
            |row| row.get::<_, String>(0),
        )?;
        Ok(rows.into_iter().collect())
    }

    // ---- messages -----------------------------------------------------------

    fn create_message(&self, m: Message) -> Result<Message, BoughError> {
        let role = enum_str(&m.role);
        let parts = serde_json::to_string(&m.parts)
            .map_err(|e| BoughError::bad_request(format!("unserializable parts: {e}")))?;
        self.run(
            "INSERT INTO messages (id, session_id, role, parts, pending, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                m.id,
                m.session_id,
                role,
                parts,
                bit(m.pending),
                m.created_at
            ],
        )?;
        self.get_message(&m.id)?
            .ok_or_else(|| BoughError::bad_request("createMessage read-back found no row"))
    }

    fn get_message(&self, id: &str) -> Result<Option<Message>, BoughError> {
        self.one("SELECT * FROM messages WHERE id = ?", [id], to_message)
    }

    /// The session's own messages, oldest first. `ORDER BY created_at, rowid`
    /// — never `created_at` alone. Dropping the tie-break silently reorders
    /// history.
    fn messages_for(&self, session_id: &str) -> Result<Vec<Message>, BoughError> {
        self.all(
            "SELECT * FROM messages WHERE session_id = ? ORDER BY created_at, rowid",
            [session_id],
            to_message,
        )
    }

    /// The full replayable thread: every ancestor's messages root→parent, then
    /// the session's own.
    fn thread_for(&self, session_id: &str) -> Result<Vec<Message>, BoughError> {
        let mut out = Vec::new();
        for s in self.ancestor_chain(session_id)? {
            out.extend(self.messages_for(&s.id)?);
        }
        Ok(out)
    }

    /// Wholesale overwrite — the turn runner streams into this every round.
    fn update_message(&self, id: &str, parts: &[Part], pending: bool) -> Result<(), BoughError> {
        let parts = serde_json::to_string(parts)
            .map_err(|e| BoughError::bad_request(format!("unserializable parts: {e}")))?;
        self.run(
            "UPDATE messages SET parts = ?, pending = ? WHERE id = ?",
            params![parts, bit(pending), id],
        )
    }

    /// Delete `message_id` and every message after it in that session — the
    /// ONE destructive write in this file (the unsend backend). Ordering is
    /// `(created_at, rowid)`, the same tie-broken order `messages_for` reads
    /// by — a row-value comparison, so a message that shares a millisecond
    /// with the target is cut only if it was actually written after it. One
    /// transaction, and the turn rows go first: `turns.message_id` is a real
    /// foreign key. Its own FTS rows go with it, or a deleted message keeps
    /// answering searches.
    fn delete_messages_from(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Vec<String>, BoughError> {
        let anchor: Option<(i64, i64)> = self.one(
            "SELECT created_at, rowid FROM messages WHERE id = ? AND session_id = ?",
            params![message_id, session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let Some((created_at, rowid)) = anchor else {
            return Ok(Vec::new());
        };
        let doomed: Vec<String> = self.all(
            "SELECT id FROM messages
              WHERE session_id = ? AND (created_at, rowid) >= (?, ?)
              ORDER BY created_at, rowid",
            params![session_id, created_at, rowid],
            |row| row.get(0),
        )?;
        if doomed.is_empty() {
            return Ok(doomed);
        }
        let tx = self.conn.unchecked_transaction().map_err(sql_err)?;
        for id in &doomed {
            tx.execute("DELETE FROM turns WHERE message_id = ?", [id])
                .map_err(sql_err)?;
            tx.execute("DELETE FROM messages_fts WHERE message_id = ?", [id])
                .map_err(sql_err)?;
            tx.execute("DELETE FROM messages WHERE id = ?", [id])
                .map_err(sql_err)?;
        }
        tx.commit().map_err(sql_err)?;
        Ok(doomed)
    }

    // ---- turns --------------------------------------------------------------

    fn create_turn(&self, t: Turn) -> Result<Turn, BoughError> {
        let status = enum_str(&t.status);
        self.run(
            "INSERT INTO turns (id, session_id, message_id, status, step, created_at, updated_at, error)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![t.id, t.session_id, t.message_id, status, t.step, t.created_at, t.updated_at, t.error],
        )?;
        self.get_turn(&t.id)?
            .ok_or_else(|| BoughError::bad_request("createTurn read-back found no row"))
    }

    fn get_turn(&self, id: &str) -> Result<Option<Turn>, BoughError> {
        self.one("SELECT * FROM turns WHERE id = ?", [id], to_turn)
    }

    /// The turn that produced a supervisor message; most recently touched wins.
    fn turn_for_message(&self, message_id: &str) -> Result<Option<Turn>, BoughError> {
        self.one(
            "SELECT * FROM turns WHERE message_id = ? ORDER BY updated_at DESC, rowid DESC LIMIT 1",
            [message_id],
            to_turn,
        )
    }

    fn turns_for_session(&self, session_id: &str) -> Result<Vec<Turn>, BoughError> {
        self.all(
            "SELECT * FROM turns WHERE session_id = ? ORDER BY created_at, rowid",
            [session_id],
            to_turn,
        )
    }

    /// Boot recovery reads `running` here and orphans every row it finds.
    fn turns_by_status(&self, status: TurnStatus) -> Result<Vec<Turn>, BoughError> {
        let status = enum_str(&status);
        self.all(
            "SELECT * FROM turns WHERE status = ? ORDER BY created_at, rowid",
            [status],
            to_turn,
        )
    }

    /// The latest turn status per session. Correlated `LIMIT 1` rather than
    /// `GROUP BY` with a bare column: bare-column-with-MAX picks an arbitrary
    /// row among ties, and two checkpoints in one millisecond is the normal
    /// case, not the rare one.
    fn latest_turn_statuses(&self) -> Result<HashMap<String, TurnStatus>, BoughError> {
        let rows: Vec<(String, String)> = self.all(
            "SELECT session_id, status FROM turns
              WHERE rowid = (
                SELECT rowid FROM turns x WHERE x.session_id = turns.session_id
                 ORDER BY x.updated_at DESC, x.rowid DESC LIMIT 1
              )",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut out = HashMap::new();
        for (session_id, status) in rows {
            out.insert(session_id, enum_val(&status)?);
        }
        Ok(out)
    }

    /// Checkpoint a turn. Every call bumps `updated_at` from the injected
    /// clock. `usage` REPLACES the turn's usage columns rather than
    /// accumulating: the runner carries the turn's running total and
    /// checkpoints it. Missing id → silent no-op.
    fn update_turn(&self, id: &str, patch: TurnPatch) -> Result<(), BoughError> {
        let cur: Option<RawTurn> = self.one("SELECT * FROM turns WHERE id = ?", [id], |row| {
            Ok(RawTurn {
                status: row.get("status")?,
                step: row.get("step")?,
                error: row.get("error")?,
                input_tokens: row.get("input_tokens")?,
                output_tokens: row.get("output_tokens")?,
                reasoning_tokens: row.get("reasoning_tokens")?,
                cache_read_tokens: row.get("cache_read_tokens")?,
                cache_write_tokens: row.get("cache_write_tokens")?,
                cost_usd: row.get("cost_usd")?,
            })
        })?;
        let Some(cur) = cur else {
            return Ok(());
        };
        let status = patch.status.map(|s| enum_str(&s)).unwrap_or(cur.status);
        let step = patch.step.unwrap_or(cur.step);
        let error = patch.error.apply(cur.error);
        let u = patch.usage;
        self.run(
            "UPDATE turns SET status = ?, step = ?, updated_at = ?, error = ?,
               input_tokens = ?, output_tokens = ?, reasoning_tokens = ?,
               cache_read_tokens = ?, cache_write_tokens = ?, cost_usd = ?
             WHERE id = ?",
            params![
                status,
                step,
                (self.now)(),
                error,
                u.as_ref().map(|u| u.input_tokens).or(cur.input_tokens),
                u.as_ref().map(|u| u.output_tokens).or(cur.output_tokens),
                if let Some(u) = &u {
                    u.reasoning_tokens
                } else {
                    cur.reasoning_tokens
                },
                if let Some(u) = &u {
                    u.cache_read_tokens
                } else {
                    cur.cache_read_tokens
                },
                if let Some(u) = &u {
                    u.cache_write_tokens
                } else {
                    cur.cache_write_tokens
                },
                if let Some(u) = &u {
                    u.cost_usd
                } else {
                    cur.cost_usd
                },
                id,
            ],
        )
    }

    // ---- durable KV, scoped to the lineage root -----------------------------

    fn get_state(&self, root_id: &str, key: &str) -> Result<Option<String>, BoughError> {
        self.one(
            "SELECT value FROM session_state WHERE root_id = ? AND key = ?",
            params![root_id, key],
            |row| row.get(0),
        )
    }

    /// Upsert: a re-set overwrites in place and re-stamps `updated_at`.
    fn set_state(&self, root_id: &str, key: &str, value: &str, now: i64) -> Result<(), BoughError> {
        self.run(
            "INSERT INTO session_state (root_id, key, value, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT(root_id, key)
               DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![root_id, key, value, now],
        )
    }

    /// Keys and sizes only — a listing must never drag whole values into context.
    fn list_state(&self, root_id: &str) -> Result<Vec<StateEntry>, BoughError> {
        self.all(
            "SELECT key, length(value) AS bytes, updated_at FROM session_state
              WHERE root_id = ? ORDER BY key",
            [root_id],
            |row| {
                Ok(StateEntry {
                    key: row.get("key")?,
                    bytes: row.get("bytes")?,
                    updated_at: row.get("updated_at")?,
                })
            },
        )
    }

    /// True when a row was actually removed, so the caller learns "there was none".
    fn delete_state(&self, root_id: &str, key: &str) -> Result<bool, BoughError> {
        let existed = self.get_state(root_id, key)?.is_some();
        self.run(
            "DELETE FROM session_state WHERE root_id = ? AND key = ?",
            params![root_id, key],
        )?;
        Ok(existed)
    }

    // ---- schedules ----------------------------------------------------------

    fn create_schedule(&self, s: Schedule) -> Result<Schedule, BoughError> {
        self.run(
            "INSERT INTO schedules
               (id, title, prompt, workspace, spec, enabled, created_at, last_run_at, next_run_at,
                session_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                s.id,
                s.title,
                s.prompt,
                s.workspace,
                s.spec,
                bit(s.enabled),
                s.created_at,
                s.last_run_at,
                s.next_run_at,
                s.session_id,
            ],
        )?;
        self.get_schedule(&s.id)?
            .ok_or_else(|| BoughError::bad_request("createSchedule read-back found no row"))
    }

    fn get_schedule(&self, id: &str) -> Result<Option<Schedule>, BoughError> {
        self.one("SELECT * FROM schedules WHERE id = ?", [id], to_schedule)
    }

    fn list_schedules(&self) -> Result<Vec<Schedule>, BoughError> {
        self.all(
            "SELECT * FROM schedules ORDER BY created_at, rowid",
            [],
            to_schedule,
        )
    }

    /// The ticker's due set: enabled and past due, soonest first.
    fn due_schedules(&self, now: i64) -> Result<Vec<Schedule>, BoughError> {
        self.all(
            "SELECT * FROM schedules WHERE enabled = 1 AND next_run_at <= ?
              ORDER BY next_run_at, rowid",
            [now],
            to_schedule,
        )
    }

    /// Overwrites the mutable fields (NOT session_id, NOT created_at); the
    /// caller merges a PATCH into the full row.
    fn update_schedule(&self, s: &Schedule) -> Result<(), BoughError> {
        self.run(
            "UPDATE schedules SET title = ?, prompt = ?, workspace = ?, spec = ?, enabled = ?,
               last_run_at = ?, next_run_at = ? WHERE id = ?",
            params![
                s.title,
                s.prompt,
                s.workspace,
                s.spec,
                bit(s.enabled),
                s.last_run_at,
                s.next_run_at,
                s.id,
            ],
        )
    }

    /// Stamp a fire. The caller computes `next_run_at` FROM NOW, never from
    /// the stale stored value — missed slots fire once, no burst.
    fn mark_schedule_run(
        &self,
        id: &str,
        last_run_at: i64,
        next_run_at: i64,
    ) -> Result<(), BoughError> {
        self.run(
            "UPDATE schedules SET last_run_at = ?, next_run_at = ? WHERE id = ?",
            params![last_run_at, next_run_at, id],
        )
    }

    fn delete_schedule(&self, id: &str) -> Result<(), BoughError> {
        self.run("DELETE FROM schedules WHERE id = ?", [id])
    }

    // ---- workflows ----------------------------------------------------------

    fn create_workflow(&self, w: WorkflowRun) -> Result<WorkflowRun, BoughError> {
        let status = enum_str(&w.status);
        let phases = serde_json::to_string(&w.phases)
            .map_err(|e| BoughError::bad_request(format!("unserializable phases: {e}")))?;
        self.run(
            "INSERT INTO workflows
               (id, session_id, name, description, script, phases, status, current_phase,
                result, error, args, resume_of, created_at, finished_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                w.id,
                w.session_id,
                w.name,
                w.description,
                w.script,
                phases,
                status,
                w.current_phase,
                json_col(&w.result),
                w.error,
                json_col(&w.args),
                w.resume_of,
                w.created_at,
                w.finished_at,
            ],
        )?;
        self.get_workflow(&w.id)?
            .ok_or_else(|| BoughError::bad_request("createWorkflow read-back found no row"))
    }

    fn get_workflow(&self, id: &str) -> Result<Option<WorkflowRun>, BoughError> {
        self.one("SELECT * FROM workflows WHERE id = ?", [id], to_workflow)
    }

    /// Runs belonging to a session — meaning its whole LINEAGE's runs.
    /// `origin_id` is followed only for fork/compaction: on a subagent or a
    /// workflow agent the same column means the SPAWNER, and a delegate
    /// listing its spawner's runs would be showing work that is not its own.
    fn list_workflows(&self, session_id: Option<&str>) -> Result<Vec<WorkflowRun>, BoughError> {
        let Some(session_id) = session_id else {
            return self.all(
                "SELECT * FROM workflows ORDER BY created_at DESC, rowid DESC",
                [],
                to_workflow,
            );
        };
        let mut ids: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = vec![session_id.to_string()];
        while !queue.is_empty() {
            let id = queue.remove(0);
            if seen.contains(&id) {
                continue;
            }
            seen.insert(id.clone());
            let Some(s) = self.get_session(&id)? else {
                continue;
            };
            ids.push(id);
            if let Some(pid) = &s.parent_id {
                queue.push(pid.clone());
            }
            if let Some(oid) = &s.origin_id {
                if s.kind == SessionKind::Fork || s.kind == SessionKind::Compaction {
                    queue.push(oid.clone());
                }
            }
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql = format!(
            "SELECT * FROM workflows WHERE session_id IN ({placeholders})
             ORDER BY created_at DESC, rowid DESC"
        );
        self.all(&sql, params_from_iter(ids.iter()), to_workflow)
    }

    /// Runs still `running`/`paused` at boot — orphaned like turns.
    fn unfinished_workflows(&self) -> Result<Vec<WorkflowRun>, BoughError> {
        self.all(
            "SELECT * FROM workflows WHERE status IN ('running', 'paused') ORDER BY created_at, rowid",
            [],
            to_workflow,
        )
    }

    /// Patch a run's mutable fields by key membership ([`crate::types::Patch`]
    /// reifies TS's `"x" in patch`). Identity fields (id, sessionId, script,
    /// createdAt, resumeOf) are not patchable: the script text is the record
    /// of what actually ran, and a rerun is a NEW run pointing back via
    /// `resumeOf`.
    fn update_workflow(&self, id: &str, patch: WorkflowPatch) -> Result<(), BoughError> {
        let Some(cur) = self.get_workflow(id)? else {
            return Ok(());
        };
        let status = enum_str(&patch.status.unwrap_or(cur.status));
        let phases = serde_json::to_string(&patch.phases.unwrap_or(cur.phases))
            .map_err(|e| BoughError::bad_request(format!("unserializable phases: {e}")))?;
        self.run(
            "UPDATE workflows SET name = ?, description = ?, phases = ?, status = ?,
               current_phase = ?, result = ?, error = ?, args = ?, finished_at = ?
             WHERE id = ?",
            params![
                patch.name.unwrap_or(cur.name),
                patch.description.unwrap_or(cur.description),
                phases,
                status,
                patch.current_phase.apply(cur.current_phase),
                json_col(&patch.result.apply(cur.result)),
                patch.error.apply(cur.error),
                json_col(&patch.args.apply(cur.args)),
                patch.finished_at.apply(cur.finished_at),
                id,
            ],
        )
    }

    /// The `schema` column is always inserted NULL — the wire `WorkflowAgent`
    /// has no schema field (the JSON Schema is part of what `key` hashes, so a
    /// rerun already re-runs the right calls without reading it back).
    fn create_workflow_agent(&self, a: WorkflowAgent) -> Result<WorkflowAgent, BoughError> {
        let status = enum_str(&a.status);
        self.run(
            "INSERT INTO workflow_agents
               (id, run_id, idx, key, label, phase, prompt, model, schema, status, result,
                error, session_id, started_at, finished_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                a.id,
                a.run_id,
                a.idx,
                a.key,
                a.label,
                a.phase,
                a.prompt,
                a.model,
                Option::<String>::None,
                status,
                a.result,
                a.error,
                a.session_id,
                a.started_at,
                a.finished_at,
            ],
        )?;
        self.agent(&a.id)?
            .ok_or_else(|| BoughError::bad_request("createWorkflowAgent read-back found no row"))
    }

    /// Patch a journal row. `started_at` is patchable on purpose: a queued
    /// agent's clock is reset when it actually leaves the run's semaphore.
    fn update_workflow_agent(&self, id: &str, patch: WorkflowAgentPatch) -> Result<(), BoughError> {
        let Some(cur) = self.agent(id)? else {
            return Ok(());
        };
        let status = enum_str(&patch.status.unwrap_or(cur.status));
        self.run(
            "UPDATE workflow_agents SET label = ?, phase = ?, status = ?, result = ?, error = ?,
               session_id = ?, started_at = ?, finished_at = ? WHERE id = ?",
            params![
                patch.label.unwrap_or(cur.label),
                patch.phase.apply(cur.phase),
                status,
                patch.result.apply(cur.result),
                patch.error.apply(cur.error),
                patch.session_id.apply(cur.session_id),
                patch.started_at.unwrap_or(cur.started_at),
                patch.finished_at.apply(cur.finished_at),
                id,
            ],
        )
    }

    fn list_workflow_agents(&self, run_id: &str) -> Result<Vec<WorkflowAgent>, BoughError> {
        self.all(
            "SELECT * FROM workflow_agents WHERE run_id = ? ORDER BY idx, rowid",
            [run_id],
            to_workflow_agent,
        )
    }

    /// Journal lookup on rerun: the source run's row for a call key. First
    /// call wins.
    fn find_workflow_agent(
        &self,
        run_id: &str,
        key: &str,
    ) -> Result<Option<WorkflowAgent>, BoughError> {
        self.one(
            "SELECT * FROM workflow_agents WHERE run_id = ? AND key = ? ORDER BY idx, rowid LIMIT 1",
            params![run_id, key],
            to_workflow_agent,
        )
    }

    // ---- command-history memory ---------------------------------------------

    /// Append one finished command with its tag/dir junction rows and FTS row,
    /// in one transaction — a half-recorded command (history row without its
    /// tags) would silently skew every popularity query that joins them.
    fn record_command(&self, r: &CommandRecord) -> Result<(), BoughError> {
        let tx = self.conn.unchecked_transaction().map_err(sql_err)?;
        tx.execute(
            "INSERT INTO command_history
               (session_id, ts, repo, cmd, tags, exit_code, duration_ms, output_head,
                spill_path, source, message_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                r.session_id,
                r.ts,
                r.repo,
                r.cmd,
                r.tags,
                r.exit_code,
                r.duration_ms,
                r.output_head,
                r.spill_path,
                r.source,
                r.message_id,
            ],
        )
        .map_err(sql_err)?;
        let id = tx.last_insert_rowid();
        for tag in &r.tag_list {
            tx.execute(
                "INSERT INTO command_tags (command_id, tag) VALUES (?, ?)",
                params![id, tag],
            )
            .map_err(sql_err)?;
        }
        for dir in &r.dirs {
            tx.execute(
                "INSERT INTO command_dirs (command_id, rel_dir) VALUES (?, ?)",
                params![id, dir],
            )
            .map_err(sql_err)?;
        }
        tx.execute(
            "INSERT INTO command_history_fts (cmd, tags, output_head, command_id)
             VALUES (?, ?, ?, ?)",
            params![r.cmd, r.tags, r.output_head, id],
        )
        .map_err(sql_err)?;
        tx.commit().map_err(sql_err)
    }

    /// `dir` scopes to the directory and its DESCENDANTS (`src` matches
    /// `src/tui` but not `src2`), never name prefixes.
    fn command_tag_rows(
        &self,
        repo: &str,
        opts: CommandTagOpts,
    ) -> Result<Vec<CommandTagRow>, BoughError> {
        let mut conds: Vec<&str> = vec!["h.repo = ?"];
        let mut params: Vec<SqlValue> = vec![SqlValue::from(repo.to_string())];
        if let Some(since) = opts.since_ts {
            conds.push("h.ts >= ?");
            params.push(SqlValue::from(since));
        }
        if let Some(dir) = &opts.dir {
            conds.push(
                "EXISTS (SELECT 1 FROM command_dirs d
                          WHERE d.command_id = h.id AND (d.rel_dir = ? OR d.rel_dir LIKE ? || '/%'))",
            );
            params.push(SqlValue::from(dir.clone()));
            params.push(SqlValue::from(dir.clone()));
        }
        let sql = format!(
            "SELECT t.tag AS tag, h.ts AS ts, h.exit_code AS exit_code
               FROM command_history h JOIN command_tags t ON t.command_id = h.id
              WHERE {}",
            conds.join(" AND ")
        );
        self.all(&sql, params_from_iter(params), |row| {
            Ok(CommandTagRow {
                tag: row.get("tag")?,
                ts: row.get("ts")?,
                exit_code: row.get("exit_code")?,
            })
        })
    }

    /// How many distinct repos the memory holds, and how many of them use
    /// each tag.
    ///
    /// The contrast the priming note is ranked against: a tag every project
    /// uses names a TOOL (`git`, `bun`, `rg`) and reusing it was never in
    /// question, while a tag only this project uses names its subject and is
    /// the vocabulary worth sharing.
    fn tag_spread(&self, since_ts: Option<i64>) -> Result<(i64, HashMap<String, i64>), BoughError> {
        let since = since_ts.unwrap_or(i64::MIN);
        let repos: i64 = self
            .one(
                "SELECT COUNT(DISTINCT h.repo) AS n FROM command_history h WHERE h.ts >= ?",
                params![since],
                |row| row.get(0),
            )?
            .unwrap_or(0);
        let rows: Vec<(String, i64)> = self.all(
            "SELECT t.tag AS tag, COUNT(DISTINCT h.repo) AS repos
               FROM command_history h JOIN command_tags t ON t.command_id = h.id
              WHERE h.ts >= ? GROUP BY t.tag",
            params![since],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((repos, rows.into_iter().collect()))
    }

    /// Tag diversity per day — what `bough tags stats` reports. Grouped in
    /// SQLite's LOCAL time, because the question is "what did I do on
    /// Tuesday" and a UTC day boundary answers a different one. References
    /// (`instr(tag, '.') > 0`) are counted apart from coined words.
    fn tag_diversity_by_day(
        &self,
        since_ts: i64,
        repo: Option<&str>,
    ) -> Result<Vec<TagDiversityDay>, BoughError> {
        let scope = if repo.is_some() {
            " AND h.repo = ?"
        } else {
            ""
        };
        let sql = format!(
            "WITH d AS (
               SELECT h.id AS id, h.session_id AS session_id, h.tags AS tags,
                      date(h.ts / 1000, 'unixepoch', 'localtime') AS day
                 FROM command_history h WHERE h.ts >= ?{scope}
             )
             SELECT d.day AS day,
                    COUNT(DISTINCT d.session_id) AS sessions,
                    COUNT(DISTINCT d.id) AS commands,
                    COUNT(DISTINCT CASE WHEN d.tags <> '' THEN d.id END) AS tagged,
                    COUNT(DISTINCT CASE WHEN instr(t.tag, '.') = 0 THEN t.tag END) AS distinct_tags,
                    COUNT(DISTINCT CASE WHEN instr(t.tag, '.') > 0 THEN t.tag END) AS distinct_refs,
                    COUNT(t.tag) AS tag_uses,
                    (SELECT COUNT(*) FROM (
                       SELECT t2.tag FROM d d2 JOIN command_tags t2 ON t2.command_id = d2.id
                        WHERE d2.day = d.day AND instr(t2.tag, '.') = 0
                        GROUP BY t2.tag HAVING COUNT(*) = 1
                     )) AS singletons
               FROM d LEFT JOIN command_tags t ON t.command_id = d.id
              GROUP BY d.day ORDER BY d.day DESC"
        );
        let to_row = |row: &Row| -> rusqlite::Result<TagDiversityDay> {
            Ok(TagDiversityDay {
                day: row.get("day")?,
                sessions: row.get("sessions")?,
                commands: row.get("commands")?,
                tagged: row.get("tagged")?,
                distinct_tags: row.get("distinct_tags")?,
                distinct_refs: row.get("distinct_refs")?,
                tag_uses: row.get("tag_uses")?,
                singletons: row.get("singletons")?,
            })
        };
        match repo {
            Some(r) => self.all(&sql, params![since_ts, r], to_row),
            None => self.all(&sql, params![since_ts], to_row),
        }
    }

    /// Commands recorded under one tag, newest first — `bough tags show`.
    /// The limit keeps the TS clamp (`Math.max(1, Math.trunc(limit))`),
    /// bound rather than interpolated (db.md §37).
    fn commands_for_tag(
        &self,
        tag: &str,
        repo: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<TaggedCommand>, BoughError> {
        let scope = if repo.is_some() {
            " AND h.repo = ?"
        } else {
            ""
        };
        let sql = format!(
            "SELECT h.ts AS ts, h.repo AS repo, h.cmd AS cmd, h.tags AS tags,
                    h.exit_code AS exit_code, h.duration_ms AS duration_ms,
                    h.session_id AS session_id, h.message_id AS message_id
               FROM command_history h JOIN command_tags t ON t.command_id = h.id
              WHERE t.tag = ?{scope}
              ORDER BY h.ts DESC LIMIT ?"
        );
        let cap = limit.unwrap_or(20).max(1);
        let to_row = |row: &Row| -> rusqlite::Result<TaggedCommand> {
            Ok(TaggedCommand {
                ts: row.get("ts")?,
                repo: row.get("repo")?,
                cmd: row.get("cmd")?,
                tags: row.get("tags")?,
                exit_code: row.get("exit_code")?,
                duration_ms: row.get("duration_ms")?,
                session_id: row.get("session_id")?,
                message_id: row.get("message_id")?,
            })
        };
        match repo {
            Some(r) => self.all(&sql, params![tag, r, cap], to_row),
            None => self.all(&sql, params![tag, cap], to_row),
        }
    }

    /// This repo's coined vocabulary and how often each word was used — the
    /// input to write-time tag hygiene (`history/tags/hygiene.rs`), which
    /// needs to know what is already a word here before it can tell a novel
    /// one from a typo of one.
    ///
    /// References are excluded: they are keys, never vocabulary, and nothing
    /// may snap onto or away from `linear.eng-1234`.
    fn repo_tag_counts(
        &self,
        repo: &str,
        since_ts: i64,
    ) -> Result<HashMap<String, i64>, BoughError> {
        let rows: Vec<(String, i64)> = self.all(
            "SELECT t.tag AS tag, count(*) AS uses
               FROM command_history h JOIN command_tags t ON t.command_id = h.id
              WHERE h.repo = ? AND h.ts >= ? AND instr(t.tag, '.') = 0
              GROUP BY t.tag",
            params![repo, since_ts],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(rows.into_iter().collect())
    }

    /// How this EXACT command has failed here before. One row, or None when
    /// the command has no failing history: the count, how many of those were
    /// this session, and — via SQLite's bare-column-with-max rule, which takes
    /// the non-aggregated columns from the `max(ts)` row — what the last one
    /// printed.
    fn prior_failures(
        &self,
        repo: &str,
        cmd: &str,
        since_ts: i64,
        session_id: &str,
    ) -> Result<Option<PriorFailures>, BoughError> {
        let row: Option<(i64, Option<i64>, Option<i64>, Option<i64>, Option<String>)> = self.one(
            "SELECT count(*) AS n,
                    sum(session_id = ?) AS n_session,
                    max(ts) AS last_ts, exit_code, output_head
               FROM command_history
              WHERE repo = ? AND cmd = ? AND ts >= ?
                AND exit_code IS NOT NULL AND exit_code <> 0",
            params![session_id, repo, cmd, since_ts],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let Some((n, n_session, last_ts, exit_code, output_head)) = row else {
            return Ok(None);
        };
        let Some(last_ts) = last_ts else {
            return Ok(None);
        };
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(PriorFailures {
            count: n,
            in_session: n_session.unwrap_or(0),
            last_ts,
            exit_code,
            output_head: output_head.unwrap_or_default(),
        }))
    }

    /// Recent failures in this repo, newest first — the input to
    /// error-signature recall. Bounded by `limit` (clamped to ≥ 1, matching
    /// TS's `Math.max(1, trunc)`).
    fn recent_failures(
        &self,
        repo: &str,
        since_ts: i64,
        limit: i64,
    ) -> Result<Vec<RecentFailure>, BoughError> {
        self.all(
            "SELECT cmd, output_head, ts, session_id
               FROM command_history
              WHERE repo = ? AND ts >= ? AND exit_code IS NOT NULL AND exit_code <> 0
              ORDER BY ts DESC LIMIT ?",
            params![repo, since_ts, limit.max(1)],
            |row| {
                Ok(RecentFailure {
                    cmd: row.get("cmd")?,
                    output_head: row
                        .get::<_, Option<String>>("output_head")?
                        .unwrap_or_default(),
                    ts: row.get("ts")?,
                    session_id: row.get("session_id")?,
                })
            },
        )
    }

    /// The most recent command that SUCCEEDED here and starts with `prefix` —
    /// the "someone already got this right" half of the echo. `prefix` is
    /// matched with LIKE and must already be escaped for it; `\` is the escape
    /// character.
    fn last_success_like(
        &self,
        repo: &str,
        prefix: &str,
        not_cmd: &str,
        since_ts: i64,
    ) -> Result<Option<String>, BoughError> {
        self.one(
            "SELECT cmd FROM command_history
              WHERE repo = ? AND exit_code = 0 AND ts >= ?
                AND cmd LIKE ? ESCAPE '\\' AND cmd <> ?
              ORDER BY ts DESC LIMIT 1",
            params![repo, since_ts, format!("{prefix}%"), not_cmd],
            |row| row.get(0),
        )
    }

    /// The program a supervisor message ran, or None. Reads the first
    /// `tool_call` part with a string `input.code` — one program per round is
    /// the whole design, so "first" is "the one". A part list that will not
    /// parse is a corrupt row, not a crash for a reader.
    fn program_for_message(&self, message_id: &str) -> Result<Option<String>, BoughError> {
        let parts: Option<String> = self.one(
            "SELECT parts FROM messages WHERE id = ?",
            [message_id],
            |row| row.get(0),
        )?;
        let Some(parts) = parts else {
            return Ok(None);
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&parts) else {
            return Ok(None);
        };
        if let Some(arr) = parsed.as_array() {
            for p in arr {
                if p.get("type").and_then(Value::as_str) == Some("tool_call") {
                    if let Some(code) = p
                        .get("input")
                        .and_then(|i| i.get("code"))
                        .and_then(Value::as_str)
                    {
                        return Ok(Some(code.to_string()));
                    }
                }
            }
        }
        Ok(None)
    }

    // ---- keyword search ------------------------------------------------------

    /// Index (or re-index) one message. Idempotent by delete-then-insert:
    /// `messages_fts` is a standalone table with no unique constraint to lean
    /// on, so the delete is what stops a supervisor message from appearing
    /// once per round of its turn. A message with no prose contributes no row
    /// at all.
    fn index_message(&self, m: &Message) -> Result<(), BoughError> {
        self.run("DELETE FROM messages_fts WHERE message_id = ?", [&m.id])?;
        let text = indexable_text(&m.parts);
        if text.is_empty() {
            return Ok(());
        }
        self.run(
            "INSERT INTO messages_fts (text, message_id, session_id) VALUES (?, ?, ?)",
            params![text, m.id, m.session_id],
        )
    }

    /// Keyword search over transcripts. Ordered by relevance and tie-broken by
    /// `(created_at DESC, message_id)` rather than anything FTS-internal, so a
    /// rebuilt index returns results in the same order as an incrementally
    /// built one. An FTS syntax error becomes a 400 naming the query.
    fn search_messages(
        &self,
        query: &str,
        session_id: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<SearchHit>, BoughError> {
        let limit = limit.unwrap_or(20);
        let scoped = session_id.is_some();
        let sql = format!(
            "SELECT messages_fts.message_id AS message_id,
                    messages_fts.session_id AS session_id,
                    snippet(messages_fts, 0, '', '', '…', 24) AS snippet,
                    m.created_at AS created_at
               FROM messages_fts
               JOIN messages m ON m.id = messages_fts.message_id
              WHERE messages_fts MATCH ?
                {}
              ORDER BY rank, m.created_at DESC, messages_fts.message_id
              LIMIT ?",
            if scoped {
                "AND messages_fts.session_id = ?"
            } else {
                ""
            }
        );
        let params: Vec<SqlValue> = match session_id {
            Some(sid) => vec![
                SqlValue::from(query.to_string()),
                SqlValue::from(sid.to_string()),
                SqlValue::from(limit),
            ],
            None => vec![SqlValue::from(query.to_string()), SqlValue::from(limit)],
        };
        let mapped = (|| -> rusqlite::Result<Vec<SearchHit>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |row| {
                Ok(SearchHit {
                    message_id: row.get("message_id")?,
                    session_id: row.get("session_id")?,
                    snippet: row.get("snippet")?,
                    created_at: row.get("created_at")?,
                })
            })?;
            rows.collect()
        })();
        mapped.map_err(|e| {
            BoughError::bad_request(format!(
                "search query {} is not valid FTS5 syntax ({e}). \
Quote a phrase as \"like this\"; bare \", *, ^, : and NEAR are operators.",
                serde_json::to_string(query).unwrap_or_else(|_| format!("{query:?}"))
            ))
        })
    }

    /// Rebuild the whole index from `messages` — deliberately "clear, then run
    /// `index_message` over every row in `(created_at, rowid)` order" rather
    /// than a bulk INSERT..SELECT: sharing the one projection function is what
    /// makes a rebuild produce results identical to incremental indexing.
    fn rebuild_search_index(&self) -> Result<(), BoughError> {
        self.run("DELETE FROM messages_fts", [])?;
        let messages = self.all(
            "SELECT * FROM messages ORDER BY created_at, rowid",
            [],
            to_message,
        )?;
        for m in &messages {
            self.index_message(m)?;
        }
        Ok(())
    }

    fn close(&self) {
        // rusqlite closes on drop; the trait method exists for TS parity.
    }
}

/// Open the database, creating its parent directory when it does not exist.
/// `None` resolves through `paths::db_path()` — `BOUGH_DB`, else
/// `<BOUGH_HOME>/bough.db`. `":memory:"` needs no directory.
pub fn open_db(path: Option<&str>, opts: DbOptions) -> Result<SqliteDb, BoughError> {
    let path = match path {
        Some(p) => p.to_string(),
        None => db_path().to_string_lossy().into_owned(),
    };
    if path != ":memory:" && !path.starts_with("file:") {
        if let Some(parent) = Path::new(&path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                BoughError::bad_request(format!(
                    "cannot create database directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }
    SqliteDb::new(&path, opts)
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
    use std::sync::Arc;

    use serde_json::json;

    use super::super::migrate::{user_version, SCHEMA_VERSION};
    use super::*;
    use crate::schema::parts::{Role, WorkflowAgentStatus, WorkflowStatus};
    use crate::types::Patch;

    // ---- fixtures -----------------------------------------------------------

    fn mem() -> SqliteDb {
        SqliteDb::new(":memory:", DbOptions::default()).unwrap()
    }

    fn clocked(v: Arc<AtomicI64>) -> SqliteDb {
        SqliteDb::new(
            ":memory:",
            DbOptions {
                now: Some(Arc::new(move || v.load(Ordering::SeqCst))),
            },
        )
        .unwrap()
    }

    fn session(id: &str) -> Session {
        Session {
            id: id.into(),
            parent_id: None,
            title: id.into(),
            kind: SessionKind::Root,
            created_at: 1_000,
            workspace: None,
            origin_dir: None,
            base: None,
            origin_id: None,
            origin_message_id: None,
            model: None,
            effort: None,
            draft: None,
            context_tokens: None,
            cached_tokens: None,
            last_llm_at: None,
            outcome_ok: None,
        }
    }

    fn message(id: &str, session_id: &str, text: &str, created_at: i64) -> Message {
        Message {
            id: id.into(),
            session_id: session_id.into(),
            role: Role::User,
            parts: vec![Part::Text { text: text.into() }],
            pending: false,
            created_at,
        }
    }

    fn turn(id: &str, session_id: &str, message_id: &str) -> Turn {
        Turn {
            id: id.into(),
            session_id: session_id.into(),
            message_id: message_id.into(),
            status: TurnStatus::Running,
            step: "start".into(),
            created_at: 1_000,
            updated_at: 1_000,
            error: None,
            usage: None,
        }
    }

    fn texts(ms: &[Message]) -> Vec<String> {
        ms.iter()
            .map(|m| match &m.parts[0] {
                Part::Text { text } => text.clone(),
                _ => "?".into(),
            })
            .collect()
    }

    fn schedule(id: &str, next_run_at: i64, enabled: bool) -> Schedule {
        Schedule {
            id: id.into(),
            title: id.into(),
            prompt: "p".into(),
            workspace: None,
            session_id: None,
            spec: "every:1h".into(),
            enabled,
            created_at: 1,
            last_run_at: None,
            next_run_at,
        }
    }

    fn workflow(id: &str, session_id: &str) -> WorkflowRun {
        WorkflowRun {
            id: id.into(),
            session_id: session_id.into(),
            name: id.into(),
            description: "".into(),
            script: "".into(),
            phases: vec![],
            status: WorkflowStatus::Done,
            current_phase: None,
            result: None,
            error: None,
            args: None,
            resume_of: None,
            created_at: 1,
            finished_at: Some(2),
        }
    }

    fn cmd_record() -> CommandRecord {
        CommandRecord {
            session_id: "s1".into(),
            ts: 1_000,
            repo: "repo".into(),
            cmd: "true".into(),
            tags: "".into(),
            tag_list: vec![],
            dirs: vec![],
            exit_code: Some(0),
            duration_ms: Some(1),
            output_head: "".into(),
            spill_path: None,
            source: "live".into(),
            message_id: None,
        }
    }

    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "bough-rs-db-{tag}-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Every object in `sqlite_master`, plus the stamped version. Read raw.
    fn introspect(path: &str) -> (Vec<(String, String, Option<String>)>, i64) {
        let raw = Connection::open(path).unwrap();
        let mut stmt = raw
            .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
            .unwrap();
        let objects = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let version = user_version(&raw).unwrap();
        (objects, version)
    }

    // ---- invariant 1: same-millisecond ordering -----------------------------

    #[test]
    fn messages_for_breaks_a_created_at_tie_by_insertion_order() {
        let db = mem();
        db.create_session(session("s")).unwrap();
        // Everything in the same millisecond: only rowid can order these.
        db.create_message(message("m1", "s", "first", 5_000))
            .unwrap();
        db.create_message(message("m2", "s", "second", 5_000))
            .unwrap();
        db.create_message(message("m3", "s", "third", 5_000))
            .unwrap();
        assert_eq!(
            texts(&db.messages_for("s").unwrap()),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn a_turn_started_in_the_same_millisecond_as_a_seed_sorts_after_it() {
        // The branch-seeding scenario: seeded messages are written with a REAL
        // clock, and the fresh turn posted immediately afterwards lands on the
        // same timestamp. The seed must still come first.
        let db = mem();
        db.create_session(session("branch")).unwrap();
        let seeded_at = (system_clock())();
        db.create_message(message("seed1", "branch", "seeded user", seeded_at))
            .unwrap();
        db.create_message(message("seed2", "branch", "seeded reply", seeded_at))
            .unwrap();
        db.create_message(message("live", "branch", "the new turn", seeded_at))
            .unwrap();
        assert_eq!(
            texts(&db.messages_for("branch").unwrap()),
            ["seeded user", "seeded reply", "the new turn"]
        );
    }

    #[test]
    fn created_at_still_dominates_rowid_when_timestamps_differ() {
        // The tie-break must not become the primary key: a message inserted
        // later but stamped earlier still sorts earlier.
        let db = mem();
        db.create_session(session("s")).unwrap();
        db.create_message(message("late", "s", "later", 9_000))
            .unwrap();
        db.create_message(message("early", "s", "earlier", 1_000))
            .unwrap();
        assert_eq!(texts(&db.messages_for("s").unwrap()), ["earlier", "later"]);
    }

    // ---- threadFor / ancestorChain ------------------------------------------

    #[test]
    fn thread_for_concatenates_three_levels_root_parent_own() {
        let db = mem();
        let mut root = session("root");
        root.created_at = 1;
        db.create_session(root).unwrap();
        let mut mid = session("mid");
        mid.parent_id = Some("root".into());
        mid.kind = SessionKind::Fork;
        mid.created_at = 2;
        db.create_session(mid).unwrap();
        let mut leaf = session("leaf");
        leaf.parent_id = Some("mid".into());
        leaf.kind = SessionKind::Fork;
        leaf.created_at = 3;
        db.create_session(leaf).unwrap();

        // Interleaved timestamps on purpose: the thread is grouped by SESSION,
        // root first, and only ordered by time WITHIN a session.
        db.create_message(message("r2", "root", "root b", 200))
            .unwrap();
        db.create_message(message("r1", "root", "root a", 100))
            .unwrap();
        db.create_message(message("m1", "mid", "mid a", 50))
            .unwrap();
        db.create_message(message("m2", "mid", "mid b", 400))
            .unwrap();
        db.create_message(message("l1", "leaf", "leaf a", 10))
            .unwrap();
        db.create_message(message("l2", "leaf", "leaf b", 300))
            .unwrap();

        assert_eq!(
            texts(&db.thread_for("leaf").unwrap()),
            ["root a", "root b", "mid a", "mid b", "leaf a", "leaf b"]
        );
        // A mid-tree read stops at its own messages.
        assert_eq!(
            texts(&db.thread_for("mid").unwrap()),
            ["root a", "root b", "mid a", "mid b"]
        );
        assert_eq!(texts(&db.thread_for("root").unwrap()), ["root a", "root b"]);
    }

    #[test]
    fn ancestor_chain_is_root_first_and_inclusive_unknown_ids_are_empty() {
        let db = mem();
        db.create_session(session("root")).unwrap();
        let mut mid = session("mid");
        mid.parent_id = Some("root".into());
        db.create_session(mid).unwrap();
        let mut leaf = session("leaf");
        leaf.parent_id = Some("mid".into());
        db.create_session(leaf).unwrap();
        let ids = |v: Vec<Session>| v.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(
            ids(db.ancestor_chain("leaf").unwrap()),
            ["root", "mid", "leaf"]
        );
        assert_eq!(ids(db.ancestor_chain("root").unwrap()), ["root"]);
        assert!(db.ancestor_chain("nope").unwrap().is_empty());
    }

    #[test]
    fn a_parent_id_cycle_terminates_instead_of_hanging() {
        // The `seen` set is what stops a cycle introduced by a bad write from
        // hanging the server on every read of that session.
        let db = mem();
        db.create_session(session("a")).unwrap();
        let mut b = session("b");
        b.parent_id = Some("a".into());
        db.create_session(b).unwrap();
        db.conn
            .execute("UPDATE sessions SET parent_id = 'b' WHERE id = 'a'", [])
            .unwrap();
        let chain = db.ancestor_chain("b").unwrap();
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn a_subagents_thread_is_its_own_messages_only() {
        // A subagent gets a fresh, task-only thread — parentId is None even
        // though origin_id points back at the spawner.
        let db = mem();
        db.create_session(session("spawner")).unwrap();
        db.create_message(message("p1", "spawner", "parent context", 1))
            .unwrap();
        let mut sub = session("sub");
        sub.kind = SessionKind::Subagent;
        sub.origin_id = Some("spawner".into());
        db.create_session(sub).unwrap();
        db.create_message(message("t1", "sub", "the task", 2))
            .unwrap();
        assert_eq!(texts(&db.thread_for("sub").unwrap()), ["the task"]);
        assert_eq!(
            db.sessions_by_origin("spawner")
                .unwrap()
                .iter()
                .map(|s| &s.id)
                .collect::<Vec<_>>(),
            ["sub"]
        );
    }

    // ---- migration ----------------------------------------------------------

    #[test]
    fn migration_is_idempotent_across_three_opens() {
        let dir = temp_dir("idem");
        let path = dir.join("bough.db");
        let path_s = path.to_str().unwrap();

        let first = open_db(Some(path_s), DbOptions::default()).unwrap();
        let mut s = session("s");
        s.workspace = Some("/w".into());
        s.base = Some("abc123".into());
        first.create_session(s).unwrap();
        first
            .create_message(message("m1", "s", "hello", 100))
            .unwrap();
        first.create_turn(turn("t1", "s", "m1")).unwrap();
        let mut sc = schedule("sc1", 2, true);
        sc.title = "nightly".into();
        sc.prompt = "do the thing".into();
        sc.workspace = Some("/w".into());
        sc.spec = "daily@09:00".into();
        sc.session_id = Some("s".into());
        first.create_schedule(sc).unwrap();
        drop(first);
        let schema_before = introspect(path_s);

        // Second open re-applies the same schema block. It must not throw,
        // must not alter the schema, and must not touch a single row.
        let second = open_db(Some(path_s), DbOptions::default()).unwrap();
        assert_eq!(
            second.get_session("s").unwrap().unwrap().base.as_deref(),
            Some("abc123")
        );
        assert_eq!(texts(&second.messages_for("s").unwrap()), ["hello"]);
        assert_eq!(second.get_turn("t1").unwrap().unwrap().step, "start");
        assert_eq!(
            second.get_schedule("sc1").unwrap().unwrap().spec,
            "daily@09:00"
        );
        drop(second);
        assert_eq!(introspect(path_s), schema_before);

        // ...and a third, to prove the second open was not a special case.
        let third = open_db(Some(path_s), DbOptions::default()).unwrap();
        assert_eq!(texts(&third.messages_for("s").unwrap()), ["hello"]);
        drop(third);
        assert_eq!(introspect(path_s), schema_before);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- sessions -----------------------------------------------------------

    #[test]
    fn create_session_returns_the_row_as_stored() {
        let db = mem();
        let mut s = session("s");
        s.workspace = Some("/w".into());
        s.origin_dir = Some("/w".into());
        s.model = Some("m".into());
        s.draft = Some("hi".into());
        let created = db.create_session(s).unwrap();
        assert_eq!(Some(created.clone()), db.get_session("s").unwrap());
        assert_eq!(created.workspace.as_deref(), Some("/w"));
        assert_eq!(created.outcome_ok, None);
    }

    #[test]
    fn list_sessions_is_newest_first_and_hides_nothing() {
        let db = mem();
        let mut a = session("a");
        a.created_at = 1;
        db.create_session(a).unwrap();
        let mut sub = session("sub");
        sub.created_at = 2;
        sub.kind = SessionKind::Subagent;
        sub.origin_id = Some("a".into());
        db.create_session(sub).unwrap();
        let mut b = session("b");
        b.created_at = 3;
        db.create_session(b).unwrap();
        // Visibility is the caller's derivation: every kind is returned here.
        assert_eq!(
            db.list_sessions()
                .unwrap()
                .iter()
                .map(|s| &s.id)
                .collect::<Vec<_>>(),
            ["b", "sub", "a"]
        );
    }

    #[test]
    fn session_setters_round_trip_and_null_clears_a_pin() {
        let db = mem();
        db.create_session(session("s")).unwrap();
        db.set_session_title("s", "renamed").unwrap();
        db.set_session_workspace("s", "/checkout").unwrap();
        db.set_session_base("s", "deadbeef").unwrap();
        db.set_session_model("s", Some("opus")).unwrap();
        db.set_session_effort("s", Some("high")).unwrap();
        db.set_session_draft("s", Some("prefilled")).unwrap();
        db.set_session_outcome("s", false).unwrap();
        let s = db.get_session("s").unwrap().unwrap();
        assert_eq!(s.title, "renamed");
        assert_eq!(s.model.as_deref(), Some("opus"));
        assert_eq!(s.effort.as_deref(), Some("high"));
        assert_eq!(s.draft.as_deref(), Some("prefilled"));
        assert_eq!(s.outcome_ok, Some(false));
        assert_eq!(
            db.get_session_runtime("s").unwrap(),
            SessionRuntime {
                workspace: Some("/checkout".into()),
                base: Some("deadbeef".into())
            }
        );

        db.set_session_model("s", None).unwrap();
        db.set_session_draft("s", None).unwrap();
        let s = db.get_session("s").unwrap().unwrap();
        assert_eq!(s.model, None);
        assert_eq!(s.draft, None);
    }

    #[test]
    fn add_session_usage_accumulates_cost_and_overwrites_the_context_gauge() {
        let db = mem();
        db.create_session(session("s")).unwrap();
        db.add_session_usage(
            "s",
            &Usage {
                input_tokens: 100,
                output_tokens: 10,
                reasoning_tokens: Some(5),
                cache_read_tokens: Some(900),
                cache_write_tokens: Some(0),
                cost_usd: Some(0.01),
            },
            111,
        )
        .unwrap();
        db.add_session_usage(
            "s",
            &Usage {
                input_tokens: 50,
                output_tokens: 20,
                reasoning_tokens: Some(1),
                cache_read_tokens: Some(2_000),
                cache_write_tokens: Some(100),
                cost_usd: Some(0.02),
            },
            222,
        )
        .unwrap();

        let totals = db.session_usage("s").unwrap();
        assert_eq!(totals.input_tokens, 150);
        assert_eq!(totals.output_tokens, 30);
        assert_eq!(totals.reasoning_tokens, 6);
        assert_eq!(totals.cache_read_tokens, 2_900);
        assert_eq!(totals.cache_write_tokens, 100);
        assert!((totals.cost_usd - 0.03).abs() < 1e-9);
        let s = db.get_session("s").unwrap().unwrap();
        // The gauge describes the LAST round only: 50 uncached + 2000 read +
        // 100 written.
        assert_eq!(s.context_tokens, Some(2_150));
        assert_eq!(s.cached_tokens, Some(2_100));
        assert_eq!(s.last_llm_at, Some(222));
    }

    #[test]
    fn tree_usage_rolls_up_delegated_branches_and_excludes_forks() {
        let db = mem();
        db.create_session(session("root")).unwrap();
        for (id, kind, origin) in [
            ("sub", SessionKind::Subagent, "root"),
            ("nested", SessionKind::Subagent, "sub"),
            ("wfa", SessionKind::WorkflowAgent, "root"),
            ("fork", SessionKind::Fork, "root"),
        ] {
            let mut s = session(id);
            s.kind = kind;
            s.origin_id = Some(origin.into());
            db.create_session(s).unwrap();
        }
        for id in ["root", "sub", "nested", "wfa", "fork"] {
            db.add_session_usage(
                id,
                &Usage {
                    input_tokens: 10,
                    output_tokens: 1,
                    cost_usd: Some(1.0),
                    ..Default::default()
                },
                1,
            )
            .unwrap();
        }

        // root + sub + nested + wfa = 4; the fork is a sibling the user
        // opened, not delegated work charged to this tree.
        assert_eq!(db.tree_usage("root").unwrap().cost_usd, 4.0);
        assert_eq!(db.tree_usage("root").unwrap().input_tokens, 40);
        assert_eq!(db.tree_usage("sub").unwrap().cost_usd, 2.0);
        assert_eq!(db.tree_usage("nested").unwrap().cost_usd, 1.0);
    }

    #[test]
    fn busy_session_ids_reads_running_turns_not_pending_messages() {
        let db = mem();
        db.create_session(session("a")).unwrap();
        db.create_session(session("b")).unwrap();
        db.create_message(message("ma", "a", "x", 1)).unwrap();
        db.create_message(message("mb", "b", "x", 1)).unwrap();
        db.create_turn(turn("ta", "a", "ma")).unwrap();
        let mut tb = turn("tb", "b", "mb");
        tb.status = TurnStatus::Orphaned;
        db.create_turn(tb).unwrap();
        // b's message is still pending after a crash, but its turn is orphaned
        // — the session must not read as busy forever.
        db.update_message("mb", &[Part::Text { text: "x".into() }], true)
            .unwrap();
        let busy = db.busy_session_ids().unwrap();
        assert_eq!(busy, HashSet::from(["a".to_string()]));
    }

    // ---- messages -----------------------------------------------------------

    #[test]
    fn update_message_overwrites_parts_and_the_pending_flag() {
        let db = mem();
        db.create_session(session("s")).unwrap();
        db.create_message(Message {
            id: "m".into(),
            session_id: "s".into(),
            role: Role::Supervisor,
            parts: vec![],
            pending: true,
            created_at: 1,
        })
        .unwrap();
        db.update_message(
            "m",
            &[
                Part::Reasoning {
                    text: "thinking".into(),
                    meta: None,
                    model: None,
                },
                Part::Text {
                    text: "done".into(),
                },
            ],
            false,
        )
        .unwrap();
        let m = db.get_message("m").unwrap().unwrap();
        assert!(!m.pending);
        assert_eq!(m.parts.len(), 2);
        assert!(matches!(m.parts[0], Part::Reasoning { .. }));
    }

    #[test]
    fn delete_messages_from_cuts_the_tail_in_one_transaction() {
        let db = mem();
        db.create_session(session("s")).unwrap();
        db.create_message(message("m1", "s", "keep", 100)).unwrap();
        db.create_message(message("m2", "s", "cut", 200)).unwrap();
        db.create_message(message("m3", "s", "cut too", 200))
            .unwrap();
        db.create_turn(turn("t2", "s", "m2")).unwrap();
        for id in ["m1", "m2", "m3"] {
            let m = db.get_message(id).unwrap().unwrap();
            db.index_message(&m).unwrap();
        }
        let deleted = db.delete_messages_from("s", "m2").unwrap();
        assert_eq!(deleted, ["m2", "m3"]);
        assert_eq!(texts(&db.messages_for("s").unwrap()), ["keep"]);
        // The turn row went first (FK), and the FTS rows went with it.
        assert!(db.get_turn("t2").unwrap().is_none());
        assert!(db.search_messages("cut", None, None).unwrap().is_empty());
        // Wrong session or unknown anchor: nothing deleted.
        assert!(db.delete_messages_from("s", "nope").unwrap().is_empty());
        assert!(db.delete_messages_from("other", "m1").unwrap().is_empty());
    }

    // ---- turns --------------------------------------------------------------

    #[test]
    fn update_turn_checkpoints_with_the_injected_clock() {
        let clock = Arc::new(AtomicI64::new(5_000));
        let db = clocked(clock.clone());
        db.create_session(session("s")).unwrap();
        db.create_message(message("m", "s", "x", 1)).unwrap();
        let mut t = turn("t", "s", "m");
        t.created_at = 1;
        t.updated_at = 1;
        let t = db.create_turn(t).unwrap();
        assert_eq!(t.usage, None, "a turn with no reported round has no usage");

        clock.store(6_000, Ordering::SeqCst);
        db.update_turn(
            "t",
            TurnPatch {
                step: Some("round 1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(db.get_turn("t").unwrap().unwrap().updated_at, 6_000);
        assert_eq!(
            db.get_turn("t").unwrap().unwrap().status,
            TurnStatus::Running,
            "an unpatched field is preserved"
        );

        clock.store(7_000, Ordering::SeqCst);
        db.update_turn(
            "t",
            TurnPatch {
                status: Some(TurnStatus::Done),
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    cost_usd: Some(0.5),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let done = db.get_turn("t").unwrap().unwrap();
        assert_eq!(done.updated_at, 7_000);
        assert_eq!(done.step, "round 1");
        assert_eq!(
            done.usage,
            Some(Usage {
                input_tokens: 10,
                output_tokens: 2,
                reasoning_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost_usd: Some(0.5),
            })
        );

        // A turn's usage is a running total the runner carries: patching it
        // again REPLACES rather than adds, or every round after the first
        // double-counts.
        clock.store(8_000, Ordering::SeqCst);
        db.update_turn(
            "t",
            TurnPatch {
                usage: Some(Usage {
                    input_tokens: 25,
                    output_tokens: 4,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            db.get_turn("t")
                .unwrap()
                .unwrap()
                .usage
                .unwrap()
                .input_tokens,
            25
        );

        db.update_turn(
            "t",
            TurnPatch {
                status: Some(TurnStatus::Error),
                error: Patch::Set("context window exceeded".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            db.get_turn("t").unwrap().unwrap().error.as_deref(),
            Some("context window exceeded")
        );
        db.update_turn(
            "t",
            TurnPatch {
                error: Patch::Clear,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(db.get_turn("t").unwrap().unwrap().error, None);

        // Unknown id: silent no-op.
        db.update_turn("nope", TurnPatch::default()).unwrap();
    }

    #[test]
    fn turn_lookups_by_status_by_message_latest_per_session() {
        let clock = Arc::new(AtomicI64::new(1));
        let db = clocked(clock.clone());
        db.create_session(session("s")).unwrap();
        db.create_message(message("m1", "s", "a", 1)).unwrap();
        db.create_message(message("m2", "s", "b", 2)).unwrap();
        let mut t1 = turn("t1", "s", "m1");
        t1.created_at = 1;
        t1.updated_at = 1;
        db.create_turn(t1).unwrap();
        let mut t2 = turn("t2", "s", "m2");
        t2.created_at = 2;
        t2.updated_at = 2;
        db.create_turn(t2).unwrap();

        let ids = |ts: Vec<Turn>| ts.into_iter().map(|t| t.id).collect::<Vec<_>>();
        assert_eq!(ids(db.turns_for_session("s").unwrap()), ["t1", "t2"]);
        assert_eq!(
            ids(db.turns_by_status(TurnStatus::Running).unwrap()),
            ["t1", "t2"]
        );
        assert_eq!(db.turn_for_message("m1").unwrap().unwrap().id, "t1");

        // Both checkpoints land on the same millisecond — the tie-break must
        // still pick the later row rather than an arbitrary one.
        clock.store(9, Ordering::SeqCst);
        db.update_turn(
            "t1",
            TurnPatch {
                status: Some(TurnStatus::Done),
                ..Default::default()
            },
        )
        .unwrap();
        db.update_turn(
            "t2",
            TurnPatch {
                status: Some(TurnStatus::Interrupted),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            db.latest_turn_statuses().unwrap(),
            HashMap::from([("s".to_string(), TurnStatus::Interrupted)])
        );
        assert!(db.turns_by_status(TurnStatus::Running).unwrap().is_empty());
    }

    // ---- durable KV ---------------------------------------------------------

    #[test]
    fn session_state_is_upserted_listed_by_key_and_reports_real_deletes() {
        let db = mem();
        db.set_state("root", "b", r#"{"n":2}"#, 10).unwrap();
        db.set_state("root", "a", r#"{"n":1}"#, 20).unwrap();
        db.set_state("root", "a", r#"{"n":11}"#, 30).unwrap();
        db.set_state("other", "a", r#"{"n":9}"#, 40).unwrap();

        assert_eq!(
            db.get_state("root", "a").unwrap().as_deref(),
            Some(r#"{"n":11}"#)
        );
        assert_eq!(db.get_state("root", "missing").unwrap(), None);
        assert_eq!(
            db.list_state("root").unwrap(),
            vec![
                StateEntry {
                    key: "a".into(),
                    bytes: 8,
                    updated_at: 30
                },
                StateEntry {
                    key: "b".into(),
                    bytes: 7,
                    updated_at: 10
                },
            ]
        );
        assert!(db.delete_state("root", "a").unwrap());
        assert!(!db.delete_state("root", "a").unwrap());
        // Scoping is by root id: the other lineage is untouched.
        assert_eq!(
            db.get_state("other", "a").unwrap().as_deref(),
            Some(r#"{"n":9}"#)
        );
    }

    // ---- schedules ----------------------------------------------------------

    #[test]
    fn due_schedules_returns_enabled_past_due_rows_soonest_first() {
        let db = mem();
        db.create_schedule(schedule("late", 50, true)).unwrap();
        db.create_schedule(schedule("later", 90, true)).unwrap();
        db.create_schedule(schedule("off", 10, false)).unwrap();
        db.create_schedule(schedule("future", 500, true)).unwrap();

        let ids = |v: Vec<Schedule>| v.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(db.due_schedules(100).unwrap()), ["late", "later"]);
        db.mark_schedule_run("late", 100, 3_700_100).unwrap();
        assert_eq!(ids(db.due_schedules(100).unwrap()), ["later"]);
        assert_eq!(
            db.get_schedule("late").unwrap().unwrap().last_run_at,
            Some(100)
        );

        let mut s = db.get_schedule("later").unwrap().unwrap();
        s.enabled = false;
        s.spec = "daily@07:00".into();
        db.update_schedule(&s).unwrap();
        assert!(!db.get_schedule("later").unwrap().unwrap().enabled);
        assert_eq!(
            db.get_schedule("later").unwrap().unwrap().spec,
            "daily@07:00"
        );
        assert!(db.due_schedules(100).unwrap().is_empty());

        db.delete_schedule("off").unwrap();
        assert!(db.get_schedule("off").unwrap().is_none());
    }

    // ---- workflows ----------------------------------------------------------

    #[test]
    fn workflow_rows_round_trip_and_patch_by_field_membership() {
        let db = mem();
        db.create_session(session("s")).unwrap();
        let mut w = workflow("w1", "s");
        w.name = "audit".into();
        w.description = "review handlers".into();
        w.script = "export const meta = {}".into();
        w.phases = vec![
            WorkflowPhase {
                title: "Review".into(),
                detail: None,
            },
            WorkflowPhase {
                title: "Verify".into(),
                detail: Some("second pass".into()),
            },
        ];
        w.status = WorkflowStatus::Running;
        w.args = Some(json!({"files": ["a.ts"]}));
        w.finished_at = None;
        let run = db.create_workflow(w).unwrap();
        assert_eq!(run.phases.len(), 2);
        assert_eq!(run.args, Some(json!({"files": ["a.ts"]})));

        db.update_workflow(
            "w1",
            WorkflowPatch {
                current_phase: Patch::Set("Review".into()),
                ..Default::default()
            },
        )
        .unwrap();
        // An unpatched field survives — including the JSON ones.
        assert_eq!(
            db.get_workflow("w1").unwrap().unwrap().args,
            Some(json!({"files": ["a.ts"]}))
        );
        assert_eq!(
            db.get_workflow("w1")
                .unwrap()
                .unwrap()
                .current_phase
                .as_deref(),
            Some("Review")
        );

        db.update_workflow(
            "w1",
            WorkflowPatch {
                status: Some(WorkflowStatus::Done),
                result: Patch::Set(json!([1, 2, 3])),
                finished_at: Patch::Set(99),
                ..Default::default()
            },
        )
        .unwrap();
        let done = db.get_workflow("w1").unwrap().unwrap();
        assert_eq!(done.status, WorkflowStatus::Done);
        assert_eq!(done.result, Some(json!([1, 2, 3])));
        assert_eq!(done.finished_at, Some(99));
        assert_eq!(
            done.script, "export const meta = {}",
            "the script is never patched"
        );

        assert!(db.unfinished_workflows().unwrap().is_empty());
        assert_eq!(
            db.list_workflows(Some("s"))
                .unwrap()
                .iter()
                .map(|w| &w.id)
                .collect::<Vec<_>>(),
            ["w1"]
        );
        assert!(db.list_workflows(Some("nobody")).unwrap().is_empty());
    }

    #[test]
    fn a_fork_and_a_compaction_list_their_ancestors_runs_not_just_their_own() {
        // A fork's transcript IS its ancestor chain's messages, so it renders
        // the parent's workflow cards; scoping to one session id left every
        // one of those cards with no run row to read.
        let db = mem();
        db.create_session(session("root")).unwrap();
        let mut fork = session("fork");
        fork.parent_id = Some("root".into());
        fork.kind = SessionKind::Fork;
        db.create_session(fork).unwrap();
        let mut compact = session("compact");
        compact.parent_id = Some("fork".into());
        compact.kind = SessionKind::Compaction;
        db.create_session(compact).unwrap();
        db.create_session(session("other")).unwrap();
        db.create_workflow(workflow("wRoot", "root")).unwrap();
        db.create_workflow(workflow("wFork", "fork")).unwrap();
        db.create_workflow(workflow("wOther", "other")).unwrap();

        let ids = |sid: &str| {
            let mut v = db
                .list_workflows(Some(sid))
                .unwrap()
                .into_iter()
                .map(|w| w.id)
                .collect::<Vec<_>>();
            v.sort();
            v
        };
        assert_eq!(ids("root"), ["wRoot"]);
        assert_eq!(ids("fork"), ["wFork", "wRoot"]);
        // Two levels down, and still not the unrelated session's run.
        assert_eq!(ids("compact"), ["wFork", "wRoot"]);
        assert_eq!(ids("other"), ["wOther"]);
    }

    #[test]
    fn a_fork_seeded_by_copy_reads_its_origins_runs_a_subagent_does_not() {
        // Forking a ROOT parents the branch at the root's parent — which is
        // nothing — so `origin_id` is the only edge back to the run that
        // produced the copied card.
        let db = mem();
        db.create_session(session("root")).unwrap();
        let mut branch = session("branch");
        branch.kind = SessionKind::Fork;
        branch.origin_id = Some("root".into());
        db.create_session(branch).unwrap();
        let mut helper = session("helper");
        helper.kind = SessionKind::Subagent;
        helper.origin_id = Some("root".into());
        db.create_session(helper).unwrap();
        db.create_workflow(workflow("w1", "root")).unwrap();

        assert_eq!(
            db.list_workflows(Some("branch"))
                .unwrap()
                .iter()
                .map(|w| &w.id)
                .collect::<Vec<_>>(),
            ["w1"]
        );
        // `origin_id` on a delegate means its SPAWNER. Its runs are not the
        // delegate's.
        assert!(db.list_workflows(Some("helper")).unwrap().is_empty());
    }

    #[test]
    fn the_agent_journal_is_keyed_lookup_plus_ordered_listing() {
        let db = mem();
        db.create_session(session("s")).unwrap();
        let mut w = workflow("w1", "s");
        w.status = WorkflowStatus::Running;
        w.finished_at = None;
        db.create_workflow(w).unwrap();
        let agent = |id: &str, idx: i64, key: &str| WorkflowAgent {
            id: id.into(),
            run_id: "w1".into(),
            idx,
            key: key.into(),
            label: id.into(),
            phase: Some("Review".into()),
            prompt: format!("review {id}"),
            model: None,
            status: WorkflowAgentStatus::Queued,
            result: None,
            error: None,
            session_id: None,
            started_at: 10,
            finished_at: None,
        };
        db.create_workflow_agent(agent("a2", 2, "k2")).unwrap();
        db.create_workflow_agent(agent("a1", 1, "k1")).unwrap();

        assert_eq!(
            db.list_workflow_agents("w1")
                .unwrap()
                .iter()
                .map(|a| &a.id)
                .collect::<Vec<_>>(),
            ["a1", "a2"]
        );
        assert_eq!(
            db.find_workflow_agent("w1", "k2").unwrap().unwrap().id,
            "a2"
        );
        assert!(db.find_workflow_agent("w1", "nope").unwrap().is_none());
        assert!(db.find_workflow_agent("other", "k1").unwrap().is_none());

        // A queued agent's clock restarts when it actually leaves the
        // semaphore. The backing subagent session must exist first —
        // `session_id` is a real foreign key.
        let mut sub1 = session("sub1");
        sub1.kind = SessionKind::WorkflowAgent;
        sub1.origin_id = Some("s".into());
        db.create_session(sub1).unwrap();
        db.update_workflow_agent(
            "a1",
            WorkflowAgentPatch {
                status: Some(WorkflowAgentStatus::Running),
                session_id: Patch::Set("sub1".into()),
                started_at: Some(500),
                ..Default::default()
            },
        )
        .unwrap();
        let a1 = db.find_workflow_agent("w1", "k1").unwrap().unwrap();
        assert_eq!(a1.status, WorkflowAgentStatus::Running);
        assert_eq!(a1.started_at, 500);
        assert_eq!(a1.prompt, "review a1", "an unpatched field survives");

        db.update_workflow_agent(
            "a1",
            WorkflowAgentPatch {
                status: Some(WorkflowAgentStatus::Done),
                result: Patch::Set("report text".into()),
                finished_at: Patch::Set(900),
                ..Default::default()
            },
        )
        .unwrap();
        let a1 = db.find_workflow_agent("w1", "k1").unwrap().unwrap();
        assert_eq!(a1.result.as_deref(), Some("report text"));
        assert_eq!(a1.session_id.as_deref(), Some("sub1"));
    }

    // ---- keyword search -----------------------------------------------------

    #[test]
    fn search_indexes_prose_is_idempotent_and_rebuilds_identically() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        db.create_session(session("s2")).unwrap();
        let m1 = db
            .create_message(Message {
                id: "m1".into(),
                session_id: "s1".into(),
                role: Role::User,
                parts: vec![Part::Text {
                    text: "the patch engine anchors on hashes".into(),
                }],
                pending: false,
                created_at: 100,
            })
            .unwrap();
        let m2 = db
            .create_message(Message {
                id: "m2".into(),
                session_id: "s2".into(),
                role: Role::Supervisor,
                parts: vec![
                    Part::Reasoning {
                        text: "consider the patch grammar".into(),
                        meta: None,
                        model: None,
                    },
                    Part::ToolCall {
                        id: "c1".into(),
                        name: "run_steps".into(),
                        input: json!({"code": "patch patch patch"}),
                    },
                    Part::Text {
                        text: "applied the patch".into(),
                    },
                ],
                pending: false,
                created_at: 200,
            })
            .unwrap();
        let m3 = db
            .create_message(Message {
                id: "m3".into(),
                session_id: "s1".into(),
                role: Role::User,
                parts: vec![Part::ToolResult {
                    call_id: "c1".into(),
                    output: json!("patch"),
                    is_error: false,
                    interrupted: None,
                }],
                pending: false,
                created_at: 300,
            })
            .unwrap();
        for m in [&m1, &m2, &m3] {
            db.index_message(m).unwrap();
        }
        // Re-indexing the same message must not duplicate it — the streaming
        // runner re-indexes on every update.
        db.index_message(&m2).unwrap();

        let hits = db.search_messages("patch", None, None).unwrap();
        let mut ids = hits
            .iter()
            .map(|h| h.message_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(ids, ["m1", "m2"]);
        assert_eq!(hits.len(), 2, "no duplicate row from re-indexing");
        assert!(
            !hits.iter().any(|h| h.message_id == "m3"),
            "tool results are not indexed — only prose and reasoning"
        );
        assert!(hits.iter().all(|h| h.snippet.contains("patch")));
        assert_eq!(
            hits.iter()
                .find(|h| h.message_id == "m1")
                .unwrap()
                .created_at,
            100
        );

        assert_eq!(
            db.search_messages("patch", Some("s2"), None)
                .unwrap()
                .iter()
                .map(|h| &h.message_id)
                .collect::<Vec<_>>(),
            ["m2"]
        );
        assert_eq!(db.search_messages("patch", None, Some(1)).unwrap().len(), 1);
        assert!(db
            .search_messages("nonexistentword", None, None)
            .unwrap()
            .is_empty());
        // Reasoning text is indexed too.
        assert_eq!(
            db.search_messages("grammar", None, None)
                .unwrap()
                .iter()
                .map(|h| &h.message_id)
                .collect::<Vec<_>>(),
            ["m2"]
        );

        let incremental = db.search_messages("patch", None, None).unwrap();
        db.rebuild_search_index().unwrap();
        assert_eq!(
            db.search_messages("patch", None, None).unwrap(),
            incremental,
            "rebuild == incremental"
        );
    }

    #[test]
    fn a_malformed_search_query_is_a_400_that_says_what_to_do() {
        let db = mem();
        let err = db
            .search_messages("\"unterminated", None, None)
            .expect_err("a malformed query must throw");
        assert_eq!(err.status(), 400);
        let msg = err.to_string();
        assert!(
            msg.contains("FTS5"),
            "the message names the syntax, not just 'failed': {msg}"
        );
        assert!(
            msg.contains("Quote a phrase"),
            "and it names the move that resolves it: {msg}"
        );
    }

    // ---- integrity ----------------------------------------------------------

    #[test]
    fn foreign_keys_are_enforced_on_every_connection() {
        let db = mem();
        let err = db
            .create_message(message("m", "no-such-session", "orphan", 1))
            .expect_err("an orphan message must be rejected");
        assert!(
            err.to_string().to_uppercase().contains("FOREIGN KEY"),
            "expected a FOREIGN KEY error, got: {err}"
        );
    }

    #[test]
    fn open_db_creates_the_parent_directory() {
        let dir = temp_dir("mkdir");
        let path = dir.join("nested").join("deeper").join("bough.db");
        let db = open_db(Some(path.to_str().unwrap()), DbOptions::default()).unwrap();
        db.create_session(session("s")).unwrap();
        assert_eq!(db.get_session("s").unwrap().unwrap().id, "s");
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- command-history memory ---------------------------------------------

    #[test]
    fn record_command_round_trips_through_command_tag_rows_scoped_by_repo() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        let mut r = cmd_record();
        r.tags = "git:push".into();
        r.tag_list = vec!["git".into(), "push".into()];
        db.record_command(&r).unwrap();
        let mut r2 = cmd_record();
        r2.repo = "other".into();
        r2.tags = "bun".into();
        r2.tag_list = vec!["bun".into()];
        r2.ts = 2_000;
        db.record_command(&r2).unwrap();
        assert_eq!(
            db.command_tag_rows("repo", CommandTagOpts::default())
                .unwrap(),
            vec![
                CommandTagRow {
                    tag: "git".into(),
                    ts: 1_000,
                    exit_code: Some(0)
                },
                CommandTagRow {
                    tag: "push".into(),
                    ts: 1_000,
                    exit_code: Some(0)
                },
            ]
        );
        assert_eq!(
            db.command_tag_rows("other", CommandTagOpts::default())
                .unwrap(),
            vec![CommandTagRow {
                tag: "bun".into(),
                ts: 2_000,
                exit_code: Some(0)
            }]
        );
    }

    #[test]
    fn command_tag_rows_dir_scope_covers_the_dir_and_its_descendants_not_name_prefixes() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        for (tag, dir) in [("a", "src"), ("b", "src/tui"), ("c", "src2")] {
            let mut r = cmd_record();
            r.tags = tag.into();
            r.tag_list = vec![tag.into()];
            r.dirs = vec![dir.into()];
            db.record_command(&r).unwrap();
        }
        let tags = |dir: &str| {
            db.command_tag_rows(
                "repo",
                CommandTagOpts {
                    dir: Some(dir.into()),
                    since_ts: None,
                },
            )
            .unwrap()
            .into_iter()
            .map(|r| r.tag)
            .collect::<Vec<_>>()
        };
        assert_eq!(tags("src"), ["a", "b"]);
        assert_eq!(tags("src/tui"), ["b"]);
    }

    #[test]
    fn command_tag_rows_since_ts_floors_the_lookback() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        for (tag, ts) in [("old", 10), ("new", 500)] {
            let mut r = cmd_record();
            r.tags = tag.into();
            r.tag_list = vec![tag.into()];
            r.ts = ts;
            db.record_command(&r).unwrap();
        }
        assert_eq!(
            db.command_tag_rows(
                "repo",
                CommandTagOpts {
                    dir: None,
                    since_ts: Some(100)
                }
            )
            .unwrap()
            .into_iter()
            .map(|r| r.tag)
            .collect::<Vec<_>>(),
            ["new"]
        );
    }

    #[test]
    fn prior_failures_aggregates_and_pulls_the_latest_failures_row() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        db.create_session(session("s2")).unwrap();
        let fail = |session: &str, ts: i64, code: i64, head: &str| {
            let mut r = cmd_record();
            r.session_id = session.into();
            r.cmd = "make x".into();
            r.ts = ts;
            r.exit_code = Some(code);
            r.output_head = head.into();
            r
        };
        db.record_command(&fail("s1", 100, 1, "first boom"))
            .unwrap();
        db.record_command(&fail("s2", 200, 2, "second boom"))
            .unwrap();
        // A success and a different command never count.
        let mut ok = cmd_record();
        ok.cmd = "make x".into();
        ok.ts = 300;
        db.record_command(&ok).unwrap();
        let mut other = cmd_record();
        other.cmd = "make y".into();
        other.exit_code = Some(1);
        db.record_command(&other).unwrap();

        let pf = db
            .prior_failures("repo", "make x", 0, "s1")
            .unwrap()
            .unwrap();
        assert_eq!(pf.count, 2);
        assert_eq!(pf.in_session, 1);
        assert_eq!(pf.last_ts, 200);
        // Bare-column-with-max(ts): the non-aggregated columns come from the
        // latest failure.
        assert_eq!(pf.exit_code, Some(2));
        assert_eq!(pf.output_head, "second boom");

        let windowed = db
            .prior_failures("repo", "make x", 150, "s1")
            .unwrap()
            .unwrap();
        assert_eq!(windowed.count, 1);
        assert_eq!(windowed.in_session, 0);
        assert!(db
            .prior_failures("repo", "never ran", 0, "s1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn recent_failures_and_last_success_like() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        let rec = |cmd: &str, ts: i64, code: i64| {
            let mut r = cmd_record();
            r.cmd = cmd.into();
            r.ts = ts;
            r.exit_code = Some(code);
            r.output_head = format!("{cmd} said");
            r
        };
        db.record_command(&rec("cargo test", 100, 1)).unwrap();
        db.record_command(&rec("cargo test --all", 200, 0)).unwrap();
        db.record_command(&rec("cargo build", 300, 101)).unwrap();

        let fails = db.recent_failures("repo", 0, 10).unwrap();
        assert_eq!(
            fails.iter().map(|f| f.cmd.as_str()).collect::<Vec<_>>(),
            ["cargo build", "cargo test"],
            "newest first, successes excluded"
        );
        assert_eq!(fails[0].output_head, "cargo build said");
        // The limit clamps to at least one row.
        assert_eq!(db.recent_failures("repo", 0, 0).unwrap().len(), 1);

        assert_eq!(
            db.last_success_like("repo", "cargo test", "cargo test", 0)
                .unwrap()
                .as_deref(),
            Some("cargo test --all")
        );
        assert_eq!(
            db.last_success_like("repo", "cargo test", "cargo test", 250)
                .unwrap(),
            None
        );
    }

    #[test]
    fn program_for_message_reads_the_first_tool_call_code_and_swallows_corruption() {
        let db = mem();
        db.create_session(session("s")).unwrap();
        db.create_message(Message {
            id: "m".into(),
            session_id: "s".into(),
            role: Role::Supervisor,
            parts: vec![
                Part::Text {
                    text: "running".into(),
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({"code": "await bash('ls', 'ls')"}),
                },
                Part::ToolCall {
                    id: "c2".into(),
                    name: "run_steps".into(),
                    input: json!({"code": "second program"}),
                },
            ],
            pending: false,
            created_at: 1,
        })
        .unwrap();
        // One program per round is the whole design, so first is the one.
        assert_eq!(
            db.program_for_message("m").unwrap().as_deref(),
            Some("await bash('ls', 'ls')")
        );
        assert_eq!(db.program_for_message("missing").unwrap(), None);
        db.create_message(message("plain", "s", "no program here", 2))
            .unwrap();
        assert_eq!(db.program_for_message("plain").unwrap(), None);
        // A part list that will not parse is a corrupt row, not a crash.
        db.conn
            .execute("UPDATE messages SET parts = 'not json' WHERE id = 'm'", [])
            .unwrap();
        assert_eq!(db.program_for_message("m").unwrap(), None);
    }

    /// One row under `repo` with the given tag list.
    fn seed_tagged(db: &SqliteDb, repo: &str, cmd: &str, tags: &[&str], ts: i64) {
        let mut r = cmd_record();
        r.repo = repo.into();
        r.cmd = cmd.into();
        r.tags = tags.join(":");
        r.tag_list = tags.iter().map(|t| t.to_string()).collect();
        r.ts = ts;
        db.record_command(&r).unwrap();
    }

    #[test]
    fn tag_spread_counts_distinct_repos_overall_and_per_tag() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        seed_tagged(&db, "r1", "git status", &["git"], 1_000);
        seed_tagged(&db, "r2", "git push", &["git", "push"], 1_000);
        seed_tagged(&db, "r2", "git pull", &["git"], 1_000);
        seed_tagged(&db, "r3", "bun test", &["bun"], 50);
        let (repos, by_tag) = db.tag_spread(None).unwrap();
        assert_eq!(repos, 3);
        assert_eq!(
            by_tag.get("git"),
            Some(&2),
            "two repos use git, not three uses"
        );
        assert_eq!(by_tag.get("push"), Some(&1));
        assert_eq!(by_tag.get("bun"), Some(&1));
        // The window filters both halves from the same scan.
        let (recent_repos, recent_by_tag) = db.tag_spread(Some(100)).unwrap();
        assert_eq!(recent_repos, 2);
        assert_eq!(recent_by_tag.get("bun"), None);
    }

    #[test]
    fn repo_tag_counts_is_coined_words_only_references_are_keys() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        seed_tagged(&db, "repo", "gh pr view 19", &["gh", "pr.19"], 1_000);
        seed_tagged(&db, "repo", "gh pr list", &["gh"], 1_000);
        seed_tagged(&db, "other", "gh api", &["gh"], 1_000);
        seed_tagged(&db, "repo", "old cmd", &["stale"], 10);
        let counts = db.repo_tag_counts("repo", 100).unwrap();
        assert_eq!(counts.get("gh"), Some(&2), "scoped to the repo");
        assert_eq!(counts.get("pr.19"), None, "a reference is never vocabulary");
        assert_eq!(counts.get("stale"), None, "the window floors the lookback");
    }

    #[test]
    fn commands_for_tag_is_newest_first_repo_scoped_and_the_limit_clamps_to_one() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        seed_tagged(&db, "repo", "first", &["t"], 100);
        seed_tagged(&db, "repo", "second", &["t"], 200);
        seed_tagged(&db, "other", "elsewhere", &["t"], 300);
        let all = db.commands_for_tag("t", None, None).unwrap();
        assert_eq!(
            all.iter().map(|c| c.cmd.as_str()).collect::<Vec<_>>(),
            ["elsewhere", "second", "first"]
        );
        assert_eq!(all[0].repo, "other");
        assert_eq!(all[0].session_id, "s1");
        let scoped = db.commands_for_tag("t", Some("repo"), None).unwrap();
        assert_eq!(
            scoped.iter().map(|c| c.cmd.as_str()).collect::<Vec<_>>(),
            ["second", "first"]
        );
        // The TS `Math.max(1, trunc)` clamp: a zero limit still returns one.
        assert_eq!(db.commands_for_tag("t", None, Some(0)).unwrap().len(), 1);
    }

    #[test]
    fn tag_diversity_by_day_partitions_coined_words_refs_and_singletons() {
        let db = mem();
        db.create_session(session("s1")).unwrap();
        db.create_session(session("s2")).unwrap();
        // One local day: three commands, one untagged, one ref, one
        // twice-used coined tag and one singleton.
        let t = 1_700_000_000_000i64;
        seed_tagged(&db, "repo", "git status", &["git"], t);
        seed_tagged(&db, "repo", "git push", &["git", "pr.19"], t + 1_000);
        let mut untagged = cmd_record();
        untagged.cmd = "sh leg".into();
        untagged.ts = t + 2_000;
        db.record_command(&untagged).unwrap();
        let mut other_session = cmd_record();
        other_session.session_id = "s2".into();
        other_session.cmd = "bun test".into();
        other_session.tags = "loner".into();
        other_session.tag_list = vec!["loner".into()];
        other_session.ts = t + 3_000;
        db.record_command(&other_session).unwrap();

        let days = db.tag_diversity_by_day(0, None).unwrap();
        assert_eq!(days.len(), 1, "one local day: {days:?}");
        let d = &days[0];
        assert_eq!(d.sessions, 2);
        assert_eq!(d.commands, 4);
        assert_eq!(d.tagged, 3, "the bare sh leg costs coverage");
        assert_eq!(d.distinct_tags, 2, "git + loner; pr.19 is a ref");
        assert_eq!(d.distinct_refs, 1);
        assert_eq!(d.tag_uses, 4);
        assert_eq!(d.singletons, 1, "loner; git has two uses, refs never count");
        // Repo scoping drops the day when nothing matches.
        assert!(db.tag_diversity_by_day(0, Some("nope")).unwrap().is_empty());
    }

    // ---- the 3 migrate reshapes against fabricated old files ----------------

    #[test]
    fn a_pre_session_id_schedules_table_is_altered_in_place_rows_kept() {
        let dir = temp_dir("sched-alter");
        let path = dir.join("old.db");
        let path_s = path.to_str().unwrap();
        {
            // Fabricate the old shape: schedules without session_id. These
            // rows are USER RECORDS and must survive.
            let raw = Connection::open(path_s).unwrap();
            raw.execute_batch(
                "CREATE TABLE schedules (id TEXT PRIMARY KEY, title TEXT NOT NULL,
                   prompt TEXT NOT NULL, workspace TEXT, spec TEXT NOT NULL, enabled INTEGER NOT NULL,
                   created_at INTEGER NOT NULL, last_run_at INTEGER, next_run_at INTEGER NOT NULL);
                 INSERT INTO schedules VALUES ('sc1', 'nightly', 'check the deploy', NULL,
                   'every:1h', 1, 1, NULL, 2);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let db = open_db(Some(path_s), DbOptions::default()).unwrap();
        let kept = db.get_schedule("sc1").unwrap().unwrap();
        assert_eq!(kept.title, "nightly");
        assert!(kept.enabled);
        // Reports to nobody, which is exactly what it did before the column.
        assert_eq!(kept.session_id, None);
        // …and the new shape round-trips through the added column.
        let mut updated = kept.clone();
        updated.enabled = false;
        db.update_schedule(&updated).unwrap();
        assert!(!db.get_schedule("sc1").unwrap().unwrap().enabled);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pre_output_head_command_history_is_rebuilt_empty_at_open_once() {
        let dir = temp_dir("rebuild");
        let path = dir.join("old.db");
        let path_s = path.to_str().unwrap();
        {
            // Fabricate the day-one shape: the table group without output_head.
            let raw = Connection::open(path_s).unwrap();
            raw.execute_batch(
                "CREATE TABLE command_history (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL,
                   ts INTEGER NOT NULL, repo TEXT NOT NULL, cmd TEXT NOT NULL, tags TEXT NOT NULL,
                   exit_code INTEGER, duration_ms INTEGER, source TEXT NOT NULL DEFAULT 'live');
                 CREATE TABLE command_tags (command_id INTEGER NOT NULL, tag TEXT NOT NULL);
                 CREATE TABLE command_dirs (command_id INTEGER NOT NULL, rel_dir TEXT NOT NULL);
                 CREATE VIRTUAL TABLE command_history_fts USING fts5(cmd, tags, command_id UNINDEXED);
                 INSERT INTO command_history (session_id, ts, repo, cmd, tags)
                   VALUES ('s', 1, 'r', 'old cmd', 't');",
            )
            .unwrap();
        }
        let db = open_db(Some(path_s), DbOptions::default()).unwrap();
        // The old rows are gone, the new shape accepts a full record.
        assert!(db
            .command_tag_rows("r", CommandTagOpts::default())
            .unwrap()
            .is_empty());
        db.create_session(session("s1")).unwrap();
        let mut r = cmd_record();
        r.tags = "a".into();
        r.tag_list = vec!["a".into()];
        r.output_head = "out".into();
        db.record_command(&r).unwrap();
        assert_eq!(
            db.command_tag_rows("repo", CommandTagOpts::default())
                .unwrap()
                .len(),
            1
        );
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pre_message_id_command_history_is_altered_in_place_rows_kept() {
        let dir = temp_dir("msgid-alter");
        let path = dir.join("old.db");
        let path_s = path.to_str().unwrap();
        {
            // Fabricate the post-output_head, pre-message_id shape. These rows
            // are the accumulated memory and must survive the ALTER.
            let raw = Connection::open(path_s).unwrap();
            raw.execute_batch(
                "CREATE TABLE command_history (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL,
                   ts INTEGER NOT NULL, repo TEXT NOT NULL, cmd TEXT NOT NULL, tags TEXT NOT NULL,
                   exit_code INTEGER, duration_ms INTEGER,
                   output_head TEXT NOT NULL DEFAULT '', spill_path TEXT,
                   source TEXT NOT NULL DEFAULT 'live');
                 CREATE TABLE command_tags (command_id INTEGER NOT NULL, tag TEXT NOT NULL);
                 CREATE TABLE command_dirs (command_id INTEGER NOT NULL, rel_dir TEXT NOT NULL);
                 CREATE VIRTUAL TABLE command_history_fts USING fts5(cmd, tags, output_head,
                   command_id UNINDEXED);
                 INSERT INTO command_history (session_id, ts, repo, cmd, tags, exit_code)
                   VALUES ('s', 1, 'r', 'kept cmd', 'kept', 0);
                 INSERT INTO command_tags VALUES (1, 'kept');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let db = open_db(Some(path_s), DbOptions::default()).unwrap();
        // The memory survived; old rows read message_id NULL through the
        // recall queries.
        assert_eq!(
            db.command_tag_rows("r", CommandTagOpts::default()).unwrap(),
            vec![CommandTagRow {
                tag: "kept".into(),
                ts: 1,
                exit_code: Some(0)
            }]
        );
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- the live install's file --------------------------------------------

    #[test]
    fn a_copy_of_the_live_bough_db_opens_clean_when_present() {
        // The gate: the user's real ~/.bough/bough.db must open under the Rust
        // migrate. Runs against a COPY; skips gracefully when there is none.
        let Some(home) = dirs::home_dir() else {
            eprintln!("skipping: no home directory");
            return;
        };
        let live = home.join(".bough").join("bough.db");
        if !live.exists() {
            eprintln!("skipping: {} does not exist", live.display());
            return;
        }
        let dir = temp_dir("live-copy");
        let copy = dir.join("bough.db");
        std::fs::copy(&live, &copy).unwrap();
        for ext in ["-wal", "-shm"] {
            let side = home.join(".bough").join(format!("bough.db{ext}"));
            if side.exists() {
                let _ = std::fs::copy(&side, dir.join(format!("bough.db{ext}")));
            }
        }
        let path_s = copy.to_str().unwrap();
        let db = open_db(Some(path_s), DbOptions::default())
            .expect("the live-db copy must open clean under the Rust migrate");
        let sessions = db
            .list_sessions()
            .expect("every stored session row must map to the wire shape");
        let _ = db.latest_turn_statuses().unwrap();
        let _ = db.list_schedules().unwrap();
        drop(db);
        // Idempotence on a real file: a second open is a no-op too.
        let again = open_db(Some(path_s), DbOptions::default()).unwrap();
        assert_eq!(again.list_sessions().unwrap().len(), sessions.len());
        {
            let raw = Connection::open(path_s).unwrap();
            assert_eq!(user_version(&raw).unwrap(), SCHEMA_VERSION);
        }
        drop(again);
        let _ = std::fs::remove_dir_all(dir);
    }
}
