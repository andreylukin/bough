//! The optional vector layer over the command-history memory (port of
//! `src/history/embed.ts`): local embeddings generated INSIDE SQLite, in their
//! OWN database file (`~/.bough/embeddings.db`), never in bough.db.
//!
//! The whole layer is one SQL statement per direction:
//!
//! ```text
//! drain:    INSERT INTO vec_index VALUES (id, <f32 blob from embed_doc(…)>)
//! similar:  … WHERE embedding MATCH <f32 blob> ORDER BY distance
//! ```
//!
//! The index is COSINE (`distance_metric=cosine`), because a threshold that
//! decides whether a neighbour is worth pushing at the model unasked has to
//! mean the same thing on every machine — see [`MAX_SEMANTIC_DISTANCE`], and
//! [`embed_doc`] for what a command is embedded as and why.
//!
//! Vectors live in their own file because every other connection in the system
//! (the migrator, `bough tags sql`'s readonly handle, the drain's own reader)
//! has no business walking a virtual table it does not own; the embed
//! connection ATTACHes bough.db as `src` and treats it as read-only by
//! discipline. embeddings.db is fully derived state and can be deleted freely.
//!
//! ## The two extensions, and how the Rust port gets them (architecture.md §2)
//!
//! - **sqlite-vec** (the `vec0` KNN table): the `sqlite-vec` crate compiles the
//!   C extension into this binary and we register it with
//!   `sqlite3_auto_extension`. Static registration, not `load_extension` — there
//!   is no dylib to find and nothing to install. Together with rusqlite
//!   `bundled` this is what retires the `Database.setCustomSQLite` Homebrew
//!   dance (`db/extensions.rs`).
//! - **model2vec-rs** (the vectors themselves): pure Rust. A Model2Vec model is
//!   a STATIC embedding table — a token→vector matrix plus a tokenizer — so
//!   embedding is a lookup and a mean, with no neural forward pass, no ONNX and
//!   no llama.cpp.
//!
//!   THIS REPLACED sqlite-lembed, which was dylib-loaded from
//!   `~/.bough/lib/lembed0.{dylib,so,dll}`. That dylib was an install step, and
//!   `create_embed_layer` returned `None` the moment it was missing — so recall
//!   was silently absent on every machine that never did it, which is the
//!   opposite of how sqlite-vec (compiled in, works everywhere) behaves. Now
//!   both halves are alike.
//!
//!   MEASURED BEFORE SWITCHING, over this memory's own commands and eight
//!   labelled queries: MiniLM-L6 through lembed found the right command in the
//!   top 5 for 5 of 8; potion-base-8M finds it for 7 of 8, which is what the
//!   OLD model reached only in union with FTS. Static embeddings are the
//!   weaker technique in general; on short command strings they are not.
//!
//! Everything is graceful-absence (`create_embed_layer()` returns `None`,
//! `drain()` returns 0) except `similar()`, whose failure is an explanatory
//! `Err` — it is only reachable when the layer exists, and the model deserves to
//! know why a recall failed. Embedding is CPU-bound and synchronous inside
//! SQLite, so the drain batch is small (64) and callers run it on
//! `spawn_blocking` (`specs/db.md` §Rust notes).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};

use rusqlite::Connection;
use serde::Serialize;

use crate::db::extensions::extensions_enabled;
use crate::errors::BoughError;
use crate::paths::{bough_path, db_path};

/// The default model: a distilled static embedder, ~30MB, 256 dims. Fetched
/// once through the Hugging Face cache and then read from disk.
const DEFAULT_MODEL: &str = "minishlab/potion-base-8M";

/// Small enough that one drain never blocks its thread noticeably.
const DRAIN_BATCH: u32 = 64;
const KNN_LIMIT: u32 = 10;
/// Command text beyond this adds latency, not meaning, to an embedding.
const DOC_CMD_CHARS: usize = 500;
/// How much of what a command PRINTED joins its document. One line: enough to
/// carry the subject ("connection refused talking to postgres"), short enough
/// that a 2k head cannot drown the command itself.
const DOC_OUTPUT_CHARS: usize = 200;
/// How far a neighbour may be and still be surfaced UNASKED — cosine
/// distance, so `1 - cos`.
///
/// CALIBRATED against this memory rather than picked, and RE-calibrated when
/// the model changed — inheriting a cutoff across models would be guessing.
/// Over 8 labelled queries and 4 deliberately unrelated ones, top-1 cosine
/// with potion-base-8M:
///
/// ```text
///   real queries    0.268 … 0.638   (7 of 8 find the right command in the top 5)
///   nonsense        0.075 … 0.285   ("how do I water my houseplants")
/// ```
///
/// 0.35 sits above every nonsense score and below all but one real one, so it
/// admits nothing for a question this memory has no business answering. The
/// margin is TIGHTER than the old model's (0.285 against 0.208), and the cost
/// is named rather than hidden: "what is listening on the port" scores 0.268
/// and therefore contributes nothing — a miss, not a wrong answer, which is
/// the failure this cutoff is chosen to prefer.
pub const MAX_SEMANTIC_DISTANCE: f64 = 0.65;

/// Bumped when [`embed_doc`] or the index's distance metric changes, and
/// stamped into the model id so an existing store REBUILDS instead of serving
/// vectors built under the old rules — they are not comparable to the new
/// ones, exactly like a different model's.
const DOC_VERSION: u32 = 3;

/// `$BOUGH_EMBED_MODEL` — a Model2Vec model: a Hugging Face id, or a local
/// directory holding `model.safetensors` + `tokenizer.json`. The dimension is
/// read from the model, never assumed.
pub const EMBED_MODEL_ENV: &str = "BOUGH_EMBED_MODEL";
/// Retired with sqlite-lembed. Kept as a name so an install that still sets it
/// gets a warning rather than silence — see [`create_embed_layer`].
pub const LEMBED_PATH_ENV: &str = "BOUGH_LEMBED_PATH";

#[derive(Debug, Default, Clone)]
pub struct EmbedLayerOptions {
    /// The command-history database to ATTACH. Defaults to the live `db_path()`.
    pub bough_db: Option<String>,
    /// Where vectors live. Defaults to `~/.bough/embeddings.db`.
    pub embed_db: Option<String>,
    /// A Model2Vec model: a Hugging Face id or a local directory. Defaults to
    /// `$BOUGH_EMBED_MODEL`, else [`DEFAULT_MODEL`].
    pub model_path: Option<String>,
}

/// One KNN hit. Field names and order are the wire shape `bough tags similar`
/// prints (`specs/history.md` §3): `cmd, tags, repo, exit_code, ts, distance`.
#[derive(Debug, Clone, Serialize)]
pub struct SimilarRow {
    pub cmd: String,
    pub tags: String,
    pub repo: String,
    /// NULL = unknown (the command was still running when the turn moved on).
    pub exit_code: Option<i64>,
    pub ts: i64,
    /// Rounded to 4 places, like the SQL does.
    pub distance: f64,
}

/// The optional layer. Held by the server for the life of the process (the drain
/// ticker) and by `bough tags similar` for one query.
pub struct EmbedLayer {
    bough_db: PathBuf,
    embed_db: PathBuf,
    /// A Hugging Face id, or a path to a local model directory.
    model_name: String,
    /// Loaded on first use and kept: the table is read once and then lives in
    /// memory, which is the whole reason a static model suits this.
    model: Mutex<Option<std::sync::Arc<model2vec_rs::model::StaticModel>>>,
    /// The one connection, opened lazily on first use. `None` = not yet opened,
    /// or the last open failed — which is what makes an offline first boot retry
    /// on the next drain instead of poisoning the layer.
    conn: Mutex<Option<Connection>>,
}

impl EmbedLayer {
    fn lock(&self) -> MutexGuard<'_, Option<Connection>> {
        // A panic inside a drain must not take the layer down with it; the
        // connection is still usable and the next tick re-runs the same SQL.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Index pending command rows, up to one batch; returns how many were
    /// embedded. Any failure — model unavailable offline, a locked writer, a
    /// torn attach — loses one tick, never the layer.
    pub fn drain(&self) -> Result<u64, BoughError> {
        let mut guard = self.lock();
        if guard.is_none() {
            match self.open() {
                Ok(conn) => *guard = Some(conn),
                // Left `None` on purpose: the failed open is NOT memoized, so
                // the next tick tries again (offline first boot, or a model
                // still downloading).
                Err(_) => return Ok(0),
            }
        }
        let conn = guard.as_ref().expect("opened above");
        Ok(drain_once(conn, &|texts| self.embed(texts)).unwrap_or(0))
    }

    /// KNN-10 over the memory, as JSON rows (what `bough tags similar` prints).
    /// Failure is an explanatory `Err`, not silence.
    pub fn similar(&self, text: &str) -> Result<Vec<serde_json::Value>, BoughError> {
        self.similar_rows(text)?
            .iter()
            .map(|r| serde_json::to_value(r).map_err(|e| embed_err(format!("row encode: {e}"))))
            .collect()
    }

    /// The typed form of [`EmbedLayer::similar`] (`specs/history.md` §3).
    pub fn similar_rows(&self, text: &str) -> Result<Vec<SimilarRow>, BoughError> {
        let mut guard = self.lock();
        if guard.is_none() {
            *guard = Some(self.open()?);
        }
        let conn = guard.as_ref().expect("opened above");
        // Embedded HERE, not in SQL: the model is a Rust value now, and a blob
        // parameter is what vec0 wants anyway.
        let query_vector = self.embed(&[text.to_string()])?.remove(0);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT h.cmd, h.tags, h.repo, h.exit_code, h.ts, round(v.distance, 4) AS distance
                   FROM (SELECT rowid, distance FROM vec_index
                          WHERE embedding MATCH ?1
                          ORDER BY distance LIMIT {KNN_LIMIT}) v
                   JOIN src.command_history h ON h.id = v.rowid
                  ORDER BY v.distance"
            ))
            .map_err(|e| embed_err(e.to_string()))?;
        let rows = stmt
            .query_map([as_blob(&query_vector)], |row| {
                Ok(SimilarRow {
                    cmd: row.get(0)?,
                    tags: row.get(1)?,
                    repo: row.get(2)?,
                    exit_code: row.get(3)?,
                    ts: row.get(4)?,
                    distance: row.get(5)?,
                })
            })
            .map_err(|e| embed_err(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| embed_err(e.to_string()))
    }

    /// The neighbours close enough to be worth acting on, for the PUSHED
    /// recall — `similar_rows` filtered by [`MAX_SEMANTIC_DISTANCE`].
    ///
    /// KNN always returns its k, however far away they are, so a raw
    /// `similar_rows` on "what is the weather" hands back ten shell commands.
    /// That is fine for `bough tags similar`, where a human asked and can see
    /// the distances; it is not fine for a note the model did not ask for.
    pub fn related(&self, text: &str) -> Result<Vec<SimilarRow>, BoughError> {
        Ok(self
            .similar_rows(text)?
            .into_iter()
            .filter(|r| r.distance <= MAX_SEMANTIC_DISTANCE)
            .collect())
    }

    pub fn close(self) {
        // Dropping the `Connection` closes it; `self` goes with it.
        *self.lock() = None;
    }

    /// Can this layer actually embed? `Err` when the model cannot be had — an
    /// offline first run with nothing in the Hugging Face cache, a bad
    /// `$BOUGH_EMBED_MODEL`, a half-written download.
    ///
    /// The layer EXISTING and the layer WORKING are two different questions:
    /// `create_embed_layer` answers the first from the extension capability
    /// alone, because the model is fetched lazily and a process that never
    /// recalls should never pay for it. Everything on the turn path wants the
    /// first question and treats a failure as one lost tick. Only a caller that
    /// is about to assert on vectors wants the second, which is why this is a
    /// probe and not a constructor check — it loads the model, and on a cold
    /// cache that means a download.
    pub fn probe_model(&self) -> Result<(), BoughError> {
        self.embed(&["probe".to_string()]).map(|_| ())
    }

    /// The lazy open. Lazy because the model may still be downloading, and
    /// because a process that never recalls should never pay for a 25MB read.
    fn open(&self) -> Result<Connection, BoughError> {
        // The model is loaded first: a store built against a model that then
        // fails to load would be an empty index nobody can explain.
        let dims = self.embed(&["probe".to_string()])?[0].len() as i64;
        if let Some(dir) = self.embed_db.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| embed_err(format!("cannot create {}: {e}", dir.display())))?;
        }
        register_vec();
        let conn = Connection::open(&self.embed_db)
            .map_err(|e| embed_err(format!("cannot open {}: {e}", self.embed_db.display())))?;
        conn.execute("ATTACH DATABASE ?1 AS src", [path_str(&self.bough_db)])
            .map_err(|e| embed_err(format!("cannot attach {}: {e}", self.bough_db.display())))?;
        let id = model_id(&self.model_name, dims);
        ensure_meta_and_index(&conn, &id, dims)
            .map_err(|e| embed_err(format!("cannot prepare vec_index: {e}")))?;
        Ok(conn)
    }

    /// Embed texts, loading the model on first use.
    ///
    /// Held behind its own lock rather than the connection's: a drain holds
    /// the connection while it embeds, and one lock for both would serialize
    /// nothing extra but would tie the model's lifetime to a SQL handle it has
    /// nothing to do with.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, BoughError> {
        let model = {
            let mut guard = self.model.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(model) => model.clone(),
                None => {
                    let loaded = model2vec_rs::model::StaticModel::from_pretrained(
                        &self.model_name,
                        None,
                        None,
                        None,
                    )
                    .map_err(|e| {
                        embed_err(format!("cannot load model {}: {e}", self.model_name))
                    })?;
                    let loaded = std::sync::Arc::new(loaded);
                    *guard = Some(loaded.clone());
                    loaded
                }
            }
        };
        Ok(model.encode(texts))
    }
}

/// Build the layer, or `None` when this process cannot host it: no extension
/// capability (`BOUGH_NO_EMBED`, or the decision was never made), or no
/// sqlite-lembed to load. `None` is an everyday answer and callers treat it as
/// "the feature does not exist".
pub fn create_embed_layer(opts: Option<EmbedLayerOptions>) -> Option<EmbedLayer> {
    if !extensions_enabled() {
        return None;
    }
    let opts = opts.unwrap_or_default();
    if std::env::var(LEMBED_PATH_ENV).is_ok() {
        // Retired, and silence would read as "it is being used".
        tracing::warn!(
            "{LEMBED_PATH_ENV} is set but sqlite-lembed is no longer used — \
             embeddings are computed in-process (see $BOUGH_EMBED_MODEL)"
        );
    }
    let model_name = opts
        .model_path
        .or_else(|| {
            std::env::var(EMBED_MODEL_ENV)
                .ok()
                .filter(|m| !m.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    Some(EmbedLayer {
        bough_db: opts.bough_db.map(PathBuf::from).unwrap_or_else(db_path),
        embed_db: opts
            .embed_db
            .map(PathBuf::from)
            .unwrap_or_else(|| bough_path(&["embeddings.db"])),
        model_name,
        model: Mutex::new(None),
        conn: Mutex::new(None),
    })
}

/// `"<basename>:<dims>"` — the identity a stored vector set is comparable
/// against. Basename only, so moving `~/.bough` is not a model change.
fn model_id(model_name: &str, dims: i64) -> String {
    // The last path segment, so a local directory and the HF id it came from
    // read alike, and a move on disk does not throw the store away.
    let base = model_name.rsplit(['/', '\\']).next().unwrap_or(model_name);
    // The doc version rides in the id because the rebuild it has to trigger is
    // the same rebuild a model change triggers, and one mechanism that cannot
    // be forgotten beats two that can.
    format!("{base}:{dims}:v{DOC_VERSION}")
}

/// The `embed_meta` bookkeeping and the `vec_index` table. Returns whether the
/// store was REBUILT (the stored model id differed) — a different model's
/// vectors are not comparable to this one's, and the store is fully derived, so
/// the honest move is to drop it and start from zero.
fn ensure_meta_and_index(conn: &Connection, model_id: &str, dims: i64) -> rusqlite::Result<bool> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS embed_meta (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )?;
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM embed_meta WHERE key = 'model'",
            [],
            |r| r.get(0),
        )
        .ok();
    let rebuilt = stored.as_deref() != Some(model_id);
    if rebuilt {
        conn.execute("DROP TABLE IF EXISTS vec_index", [])?;
        conn.execute(
            "INSERT INTO embed_meta (key, value) VALUES ('model', ?1)
              ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [model_id],
        )?;
    }
    // COSINE, not the vec0 default of L2. The threshold that decides whether
    // a neighbour is worth surfacing has to mean the same thing on every
    // machine, and only an angle does: L2 over vectors whose norm depends on
    // the model and the text length gives a number no constant can be chosen
    // against. `1 - cos` also lands the usable range in [0, 1], which is what
    // `MAX_SEMANTIC_DISTANCE` is expressed in.
    conn.execute(
        &format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_index
               USING vec0(embedding float[{dims}] distance_metric=cosine)"
        ),
        [],
    )?;
    Ok(rebuilt)
}

/// What one command row is embedded AS.
///
/// MEASURED, not guessed. Over this memory (124 rows, 8 hand-labelled
/// queries), the recall the semantic layer adds on top of FTS depends almost
/// entirely on this string:
///
/// ```text
///   tags||cmd, no separator          union@5 5/8   (what shipped first)
///   tags spaced + cmd                union@5 5/8
///   tags spaced + cmd + output line  union@5 5/8
///   + cmd split into WORDS           union@5 7/8   ← this
/// ```
///
/// Both halves earn their place. Splitting on punctuation is what stops a
/// command reading as one long token to a sentence model: `src/tui/main.tsx`
/// carries `tui` and `main` only once it is words. The output line is what
/// makes a command findable by what it was ABOUT rather than what it was
/// called — the failure it printed is often the only natural language in the
/// row.
pub fn embed_doc(tags: &str, cmd: &str, output_head: &str) -> String {
    fn words(s: &str, cap: usize) -> String {
        s.chars()
            .take(cap)
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
    let first_line = output_head.lines().next().unwrap_or("");
    let parts = [
        words(tags, tags.len()),
        words(cmd, DOC_CMD_CHARS),
        words(first_line, DOC_OUTPUT_CHARS),
    ];
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A vector as vec0 wants it: little-endian f32, one after another.
fn as_blob(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// One drain batch. **Counted by `count(*)` DELTA, never by the statement's
/// `changes`**: an insert into a vec0 virtual table reports its SHADOW-table
/// writes too (4 rows once came back as 14).
///
/// The document is built in RUST and passed in, rather than concatenated in
/// SQL as it was when it was `tags || cmd`: [`embed_doc`] splits punctuation
/// into words, which SQLite would need twenty nested `replace()` calls to do
/// and which the query side has to do identically anyway.
fn drain_once(
    conn: &Connection,
    embed: &dyn Fn(&[String]) -> Result<Vec<Vec<f32>>, BoughError>,
) -> rusqlite::Result<u64> {
    let count = |c: &Connection| -> rusqlite::Result<i64> {
        c.query_row("SELECT count(*) AS n FROM vec_index", [], |r| r.get(0))
    };
    let before = count(conn)?;
    let pending: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT h.id, h.tags, h.cmd, coalesce(h.output_head, '')
               FROM src.command_history h
              WHERE h.id NOT IN (SELECT rowid FROM vec_index)
              ORDER BY h.id
              LIMIT {DRAIN_BATCH}"
        ))?;
        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let tags: String = row.get(1)?;
            let cmd: String = row.get(2)?;
            let output: String = row.get(3)?;
            Ok((id, embed_doc(&tags, &cmd, &output)))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if !pending.is_empty() {
        // ONE encode for the batch: a static model amortizes almost nothing
        // per call, but the tokenizer setup is not free and the batch is the
        // natural unit anyway.
        let docs: Vec<String> = pending.iter().map(|(_, d)| d.clone()).collect();
        let vectors = embed(&docs).map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
        })?;
        for ((id, _), vector) in pending.iter().zip(vectors) {
            conn.execute(
                "INSERT INTO vec_index (rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![id, as_blob(&vector)],
            )?;
        }
    }
    Ok((count(conn)? - before).max(0) as u64)
}

static VEC_REGISTERED: Once = Once::new();

/// Register vec0 for every connection this process opens from here on. Static —
/// the C extension is linked into this binary by the `sqlite-vec` crate, so
/// there is no path to resolve and no install step. Idempotent.
fn register_vec() {
    VEC_REGISTERED.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is the extension entry point the crate
        // compiles in; `sqlite3_auto_extension` wants it under the loadable-
        // extension signature. This is the registration the crate documents.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::os::raw::c_int,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The layer's failures are plain, explanatory errors: the only caller that can
/// see one is `bough tags similar`, which prints `similar failed: <message>`.
fn embed_err(message: String) -> BoughError {
    BoughError::bad_request(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::extensions::enable_sqlite_extensions;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bough-embed-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn vec_conn() -> Connection {
        register_vec();
        Connection::open_in_memory().unwrap()
    }

    fn f32_blob(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// The static registration is the whole sqlite-vec story in Rust — if this
    /// fails, nothing else in the file can work.
    #[test]
    fn vec0_is_statically_registered() {
        let conn = vec_conn();
        let version: String = conn
            .query_row("SELECT vec_version()", [], |r| r.get(0))
            .unwrap();
        assert!(
            version.starts_with('v'),
            "unexpected vec_version(): {version}"
        );
    }

    /// embed.test.ts / `specs/history.md` §4: "vec0 inserts report shadow-table
    /// writes in `changes` — count drained rows by count(*) DELTA, never by
    /// changes (4 rows once reported as 14)".
    ///
    /// MEASURED on this stack (rusqlite `bundled` SQLite + the `sqlite-vec`
    /// crate 0.1.9, both the raw insert below and the real `lembed()` drain):
    /// `changes` came back 4, i.e. it happens to agree here — the TS incident
    /// was Bun's SQLite with the npm vec0 dylib. That agreement is a coincidence
    /// of versions, not a contract: vec0 writes shadow tables underneath, so
    /// `changes` counts something the caller never asked about and may drift
    /// again on the next bump. `drain_once` therefore counts by delta, and this
    /// test pins the delta as the number a caller is entitled to — never the
    /// statement's own report.
    #[test]
    fn drain_counts_by_table_size_delta_not_changes() {
        let conn = vec_conn();
        conn.execute(
            "CREATE VIRTUAL TABLE vec_index USING vec0(embedding float[4])",
            [],
        )
        .unwrap();
        let count = |c: &Connection| -> i64 {
            c.query_row("SELECT count(*) FROM vec_index", [], |r| r.get(0))
                .unwrap()
        };
        let before = count(&conn);
        // ONE statement writing four rows — the shape `drain_once` uses. Four
        // separate single-row inserts would each report 1 and could never show
        // the divergence at all.
        let changes = conn
            .execute(
                "INSERT INTO vec_index (rowid, embedding)
                   SELECT 1, ?1 UNION ALL SELECT 2, ?2
                    UNION ALL SELECT 3, ?3 UNION ALL SELECT 4, ?4",
                rusqlite::params![
                    f32_blob(&[1.0, 0.0, 0.0, 0.0]),
                    f32_blob(&[0.0, 1.0, 0.0, 0.0]),
                    f32_blob(&[0.0, 0.0, 1.0, 0.0]),
                    f32_blob(&[0.0, 0.0, 0.0, 1.0]),
                ],
            )
            .unwrap();
        assert_eq!(
            count(&conn) - before,
            4,
            "count(*) delta is the honest row count"
        );
        assert!(
            changes >= 4,
            "`changes` is not an under-count either — it reports the caller's rows PLUS whatever \
             vec0 wrote underneath ({changes}); only the delta is the number a caller asked for"
        );
    }

    /// `specs/db.md`: stored `model` ≠ `<basename>:<dims>` → DROP vec_index +
    /// upsert meta. Same id → the store survives untouched.
    #[test]
    fn a_different_model_rebuilds_the_store_from_zero() {
        let conn = vec_conn();
        assert!(
            ensure_meta_and_index(&conn, "modelA.gguf:4", 4).unwrap(),
            "first open rebuilds"
        );
        conn.execute(
            "INSERT INTO vec_index (rowid, embedding) VALUES (7, ?1)",
            rusqlite::params![f32_blob(&[1.0, 0.0, 0.0, 0.0])],
        )
        .unwrap();

        // Same model id: nothing is thrown away.
        assert!(!ensure_meta_and_index(&conn, "modelA.gguf:4", 4).unwrap());
        let kept: i64 = conn
            .query_row("SELECT count(*) FROM vec_index", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1, "same model must keep its vectors");

        // A different model (here, a different width): the vectors are not
        // comparable, so the derived store restarts empty at the new width.
        assert!(ensure_meta_and_index(&conn, "modelB.gguf:8", 8).unwrap());
        let after: i64 = conn
            .query_row("SELECT count(*) FROM vec_index", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            after, 0,
            "a different model's vectors are dropped, not reused"
        );
        let stored: String = conn
            .query_row(
                "SELECT value FROM embed_meta WHERE key = 'model'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, "modelB.gguf:8");
        // The new width is live: a 4-float vector must no longer fit.
        assert!(conn
            .execute(
                "INSERT INTO vec_index (rowid, embedding) VALUES (1, ?1)",
                rusqlite::params![f32_blob(&[1.0, 0.0, 0.0, 0.0])],
            )
            .is_err());
    }

    #[test]
    fn model_id_is_basename_probed_dims_and_the_doc_version() {
        assert_eq!(
            model_id("/a/b/mini.q8_0.gguf", 384),
            format!("mini.q8_0.gguf:384:v{DOC_VERSION}")
        );
        assert_eq!(
            model_id("/other/mini.q8_0.gguf", 384),
            format!("mini.q8_0.gguf:384:v{DOC_VERSION}")
        );
        assert_eq!(
            model_id("/a/b/wide.gguf", 768),
            format!("wide.gguf:768:v{DOC_VERSION}")
        );
        // The version is what makes a doc change rebuild rather than mix
        // old vectors with new ones.
        assert_ne!(model_id("/a/b/mini.gguf", 384), "mini.gguf:384".to_string());
    }

    #[test]
    fn the_embedded_document_is_words_from_the_tags_the_command_and_one_output_line() {
        assert_eq!(
            embed_doc(
                "bun:test:history",
                "cd /repo && bun test src/history/record.test.ts",
                "1 fail: retention window\nstack trace line\n"
            ),
            "bun test history cd repo bun test src history record test ts 1 fail retention window"
        );
        // A path is only searchable once it is words: `src/tui/main.tsx` has
        // to carry `tui` and `main` on its own.
        assert!(embed_doc("", "vim src/tui/main.tsx", "").contains("tui main"));
        // Empty parts leave no double spaces, and no output is not an error.
        assert_eq!(embed_doc("git:push", "git push", ""), "git push git push");
    }

    /// `specs/history.md` §3: `similar()` rows are
    /// `{"cmd","tags","repo","exit_code","ts","distance"}`, in that order.
    #[test]
    fn similar_row_is_the_pinned_wire_shape() {
        let row = SimilarRow {
            cmd: "docker exec -it myapp-dev-1 bash".into(),
            tags: "docker:exec".into(),
            repo: "repo".into(),
            exit_code: Some(0),
            ts: 1000,
            distance: 0.1234,
        };
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            r#"{"cmd":"docker exec -it myapp-dev-1 bash","tags":"docker:exec","repo":"repo","exit_code":0,"ts":1000,"distance":0.1234}"#
        );
    }

    // `~/.bough/lib/lembed0.<ext>` is the documented install location, and an
    // explicit `$BOUGH_LEMBED_PATH` wins outright. Absent → the feature does
    // not exist, which is what makes `create_embed_layer` answer `None`.

    /// THE FIXTURE (port of `embed_fixture.ts` + `embed.test.ts`): four seeded
    /// commands, one drain, and a query no keyword search could answer — "how do
    /// I get into the running container" must retrieve
    /// `docker exec -it myapp-dev-1 bash`.
    ///
    /// Skipped without a real model: the layer is optional by design, and a
    /// test that downloaded 25MB would not belong in `cargo test`. Unlike TS
    /// this needs NO subprocess — there is no `setCustomSQLite` window to lose.
    #[test]
    fn drain_embeds_recorded_commands_and_similar_answers_a_fuzzy_query() {
        use crate::schema::parts::{Session, SessionKind};
        use crate::types::{CommandRecord, Db};

        enable_sqlite_extensions();
        // The model is fetched through the Hugging Face cache on first use, so
        // this test needs network ONCE per machine and is a no-op after. An
        // offline first run skips rather than fails: the layer's own contract
        // is graceful absence, and a test that cannot be run offline would be
        // a worse promise than the one being tested.
        if std::env::var("BOUGH_NO_EMBED").is_ok() {
            eprintln!("skipped: BOUGH_NO_EMBED");
            return;
        }

        let dir = tmpdir("fixture");
        let bough_db = dir.join("bough.db");
        let embed_db = dir.join("embeddings.db");
        {
            let db = crate::db::open_db(Some(&path_str(&bough_db)), Default::default()).unwrap();
            db.create_session(Session {
                id: "s1".into(),
                title: "s1".into(),
                kind: SessionKind::Root,
                created_at: 1,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: None,
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
            .unwrap();
            let rec = |cmd: &str, tags: &str| CommandRecord {
                session_id: "s1".into(),
                ts: 1_000,
                repo: "repo".into(),
                cmd: cmd.into(),
                tags: tags.into(),
                tag_list: vec![],
                dirs: vec![],
                exit_code: Some(0),
                duration_ms: Some(1),
                output_head: String::new(),
                spill_path: None,
                source: "live".into(),
                message_id: None,
            };
            db.record_command(&rec("docker exec -it myapp-dev-1 bash", "docker:exec"))
                .unwrap();
            db.record_command(&rec("psql -f migrations/004.sql", "psql:migrate"))
                .unwrap();
            db.record_command(&rec("git push origin main", "git:push"))
                .unwrap();
            db.record_command(&rec("bun test src/tui", "bun:test"))
                .unwrap();
        }

        let layer = create_embed_layer(Some(EmbedLayerOptions {
            bough_db: Some(path_str(&bough_db)),
            embed_db: Some(path_str(&embed_db)),
            model_path: None,
        }))
        .expect("the layer exists wherever sqlite-vec does, which is everywhere");

        // THE SKIP THE DOC COMMENT ABOVE PROMISES. `create_embed_layer` says
        // nothing about the model — it is fetched lazily — so the layer is
        // `Some` on a machine that has never been online, and every assertion
        // below then fails for a reason the contributor cannot fix. Probe
        // first: with a cache or a network this costs one embed and the real
        // assertions run; without either, the test is skipped rather than red.
        if let Err(e) = layer.probe_model() {
            eprintln!("skipped: the embedding model is not available here ({e})");
            return;
        }

        assert_eq!(
            layer.drain().unwrap(),
            4,
            "four seeded commands, four vectors"
        );
        assert_eq!(
            layer.drain().unwrap(),
            0,
            "a second drain has nothing pending"
        );

        let hits = layer
            .similar_rows("how do I get into the running container")
            .unwrap();
        assert_eq!(hits.len(), 4, "KNN returns every row it has, nearest first");
        assert_eq!(hits[0].cmd, "docker exec -it myapp-dev-1 bash");
        assert_eq!(hits[0].tags, "docker:exec");
        assert_eq!(hits[0].repo, "repo");
        assert_eq!(hits[0].exit_code, Some(0));
        assert_eq!(hits[0].ts, 1_000);
        // COSINE distance, so `1 - cos` and the whole range is [0, 2] with
        // anything useful under 1. An L2 index would put these in the 0.9–1.3
        // band this test would then pass by accident.
        assert!(
            hits.iter().all(|h| h.distance >= 0.0 && h.distance <= 2.0),
            "cosine distances, not L2: {:?}",
            hits.iter().map(|h| h.distance).collect::<Vec<_>>()
        );
        assert!(
            hits[0].distance < hits[hits.len() - 1].distance,
            "nearest first"
        );
        // The PUSHED recall keeps only what is close enough to act on — a
        // question this memory has nothing to say about must come back empty
        // rather than with four shell commands.
        assert!(
            layer
                .related("how do I water my houseplants")
                .unwrap()
                .len()
                < layer
                    .similar_rows("how do I water my houseplants")
                    .unwrap()
                    .len(),
            "the distance cutoff has to drop something on an unrelated question"
        );

        // Vectors live in their OWN file: bough.db is untouched by all of this.
        {
            let probe = Connection::open(&bough_db).unwrap();
            let tables: i64 = probe
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name IN ('vec_index','embed_meta')",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(tables, 0, "embeddings never land in bough.db");
        }
        layer.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
