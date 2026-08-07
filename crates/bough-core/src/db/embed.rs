//! The optional vector layer over the command-history memory (port of
//! `src/history/embed.ts`): local embeddings generated INSIDE SQLite, in their
//! OWN database file (`~/.bough/embeddings.db`), never in bough.db.
//!
//! The whole layer is one SQL statement per direction:
//!
//! ```text
//! drain:    INSERT INTO vec_index SELECT id, lembed('embed', tags || cmd) …
//! similar:  … WHERE embedding MATCH lembed('embed', ?) ORDER BY distance
//! ```
//!
//! Vectors live in their own file because every other connection in the system
//! (the migrator, `bough tags sql`'s readonly handle, the drain's own reader)
//! has no business walking a virtual table it does not own; the embed
//! connection ATTACHes bough.db as `src` and treats it as read-only by
//! discipline. embeddings.db is fully derived state and can be deleted freely.
//!
//! ## The two extensions, and how the Rust port gets them (ARCHITECTURE.md §2)
//!
//! - **sqlite-vec** (the `vec0` KNN table): the `sqlite-vec` crate compiles the
//!   C extension into this binary and we register it with
//!   `sqlite3_auto_extension`. Static registration, not `load_extension` — there
//!   is no dylib to find and nothing to install. Together with rusqlite
//!   `bundled` this is what retires the `Database.setCustomSQLite` Homebrew
//!   dance (`db/extensions.rs`).
//! - **sqlite-lembed** (GGUF models as the `lembed()` SQL function): has no Rust
//!   crate, so it is DYLIB-LOADED from [`lembed_extension_path`] —
//!   `$BOUGH_LEMBED_PATH`, else `~/.bough/lib/lembed0.{dylib,so,dll}`. Absent →
//!   [`create_embed_layer`] returns `None`, exactly the shape
//!   macOS-without-Homebrew already has in TS, and tags + FTS carry recall
//!   alone.
//!
//!   The considered alternative was `fastembed` computing vectors in Rust and
//!   inserting the bytes directly. Not chosen: it drags in ONNX Runtime plus its
//!   own model download and tokenizer, i.e. a DIFFERENT embedding pipeline from
//!   the one the TS install has been filling `~/.bough/embeddings.db` with — the
//!   model-id check below would fire and throw that store away. The dylib reuses
//!   the same GGUF and the same SQL, so cutover keeps the vectors. If the dylib
//!   ever stops building, fastembed is the fallback and the rebuild is the
//!   documented cost.
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

/// ~25MB, 384 dims — plenty for a corpus of thousands of short command docs.
const MODEL_URL: &str = "https://huggingface.co/asg017/sqlite-lembed-model-examples/resolve/main/all-MiniLM-L6-v2/all-MiniLM-L6-v2.e4ce9877.q8_0.gguf";
const DEFAULT_MODEL_FILE: &str = "all-MiniLM-L6-v2.e4ce9877.q8_0.gguf";

/// Small enough that one drain never blocks its thread noticeably.
const DRAIN_BATCH: u32 = 64;
const KNN_LIMIT: u32 = 10;
/// Command text beyond this adds latency, not meaning, to an embedding.
const DOC_CMD_CHARS: u32 = 500;

/// `$BOUGH_EMBED_MODEL` — a GGUF of any width; the dimension is probed, never
/// assumed. Supplying it makes a missing file an error instead of a download.
pub const EMBED_MODEL_ENV: &str = "BOUGH_EMBED_MODEL";
/// `$BOUGH_LEMBED_PATH` — an explicit sqlite-lembed loadable extension.
pub const LEMBED_PATH_ENV: &str = "BOUGH_LEMBED_PATH";

#[derive(Debug, Default, Clone)]
pub struct EmbedLayerOptions {
    /// The command-history database to ATTACH. Defaults to the live `db_path()`.
    pub bough_db: Option<String>,
    /// Where vectors live. Defaults to `~/.bough/embeddings.db`.
    pub embed_db: Option<String>,
    /// A GGUF embedding model. Defaults to `$BOUGH_EMBED_MODEL`, else the
    /// downloaded-on-first-use MiniLM.
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
    model_path: PathBuf,
    /// A model named by opt or env is a REQUEST for that file — missing is an
    /// error, not a cue to download something else.
    model_supplied: bool,
    lembed_path: PathBuf,
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
        Ok(drain_once(conn).unwrap_or(0))
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
        let mut stmt = conn
            .prepare(&format!(
                "SELECT h.cmd, h.tags, h.repo, h.exit_code, h.ts, round(v.distance, 4) AS distance
                   FROM (SELECT rowid, distance FROM vec_index
                          WHERE embedding MATCH lembed('embed', ?1)
                          ORDER BY distance LIMIT {KNN_LIMIT}) v
                   JOIN src.command_history h ON h.id = v.rowid
                  ORDER BY v.distance"
            ))
            .map_err(|e| embed_err(e.to_string()))?;
        let rows = stmt
            .query_map([text], |row| {
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

    pub fn close(self) {
        // Dropping the `Connection` closes it; `self` goes with it.
        *self.lock() = None;
    }

    /// The lazy open. Lazy because the model may still be downloading, and
    /// because a process that never recalls should never pay for a 25MB read.
    fn open(&self) -> Result<Connection, BoughError> {
        self.ensure_model()?;
        if let Some(dir) = self.embed_db.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| embed_err(format!("cannot create {}: {e}", dir.display())))?;
        }
        register_vec();
        let conn = Connection::open(&self.embed_db)
            .map_err(|e| embed_err(format!("cannot open {}: {e}", self.embed_db.display())))?;
        load_lembed(&conn, &self.lembed_path)?;
        conn.execute("ATTACH DATABASE ?1 AS src", [path_str(&self.bough_db)])
            .map_err(|e| embed_err(format!("cannot attach {}: {e}", self.bough_db.display())))?;
        conn.execute(
            "INSERT INTO temp.lembed_models(name, model)
              SELECT 'embed', lembed_model_from_file(?1)",
            [path_str(&self.model_path)],
        )
        .map_err(|e| {
            embed_err(format!(
                "cannot load model {}: {e}",
                self.model_path.display()
            ))
        })?;
        // The model decides the dimension; probe it rather than hardcode, so an
        // env-supplied model of any width just works.
        let dims = probe_dims(&conn).map_err(|e| embed_err(format!("cannot probe dims: {e}")))?;
        let id = model_id(&self.model_path, dims);
        ensure_meta_and_index(&conn, &id, dims)
            .map_err(|e| embed_err(format!("cannot prepare vec_index: {e}")))?;
        Ok(conn)
    }

    /// Download the default model on first use. A model named by opt or env is
    /// never downloaded — a missing one is an error naming the path.
    fn ensure_model(&self) -> Result<(), BoughError> {
        if self.model_path.exists() {
            return Ok(());
        }
        if self.model_supplied {
            return Err(embed_err(format!(
                "embedding model not found at {}",
                self.model_path.display()
            )));
        }
        if let Some(dir) = self.model_path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| embed_err(format!("cannot create {}: {e}", dir.display())))?;
        }
        // Download to a sibling temp name and rename, so a killed download can
        // never leave a half-written file that every later boot trusts.
        let tmp = self.model_path.with_extension("download");
        download_to(MODEL_URL, &tmp)?;
        std::fs::rename(&tmp, &self.model_path)
            .map_err(|e| embed_err(format!("cannot install model: {e}")))?;
        Ok(())
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
    let lembed_path = lembed_extension_path()?;
    let env_model = std::env::var(EMBED_MODEL_ENV)
        .ok()
        .filter(|m| !m.is_empty());
    let model_supplied = opts.model_path.is_some() || env_model.is_some();
    let model_path = opts
        .model_path
        .or(env_model)
        .map(PathBuf::from)
        .unwrap_or_else(|| bough_path(&["models", DEFAULT_MODEL_FILE]));
    Some(EmbedLayer {
        bough_db: opts.bough_db.map(PathBuf::from).unwrap_or_else(db_path),
        embed_db: opts
            .embed_db
            .map(PathBuf::from)
            .unwrap_or_else(|| bough_path(&["embeddings.db"])),
        model_path,
        model_supplied,
        lembed_path,
        conn: Mutex::new(None),
    })
}

/// Where sqlite-lembed lives, if anywhere. `$BOUGH_LEMBED_PATH` wins outright (a
/// named path is a request, and a wrong one should fail loudly at open rather
/// than silently disable recall); otherwise `~/.bough/lib/lembed0.<ext>`, the
/// documented install location — copy it out of the npm
/// `sqlite-lembed-<os>-<arch>` package or build asg017/sqlite-lembed.
pub fn lembed_extension_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var(LEMBED_PATH_ENV) {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let p = bough_path(&["lib", LEMBED_FILE]);
    p.exists().then_some(p)
}

/// The platform's sqlite-lembed filename, as the npm package ships it.
const LEMBED_FILE: &str = if cfg!(target_os = "macos") {
    "lembed0.dylib"
} else if cfg!(target_os = "windows") {
    "lembed0.dll"
} else {
    "lembed0.so"
};

/// `"<basename>:<dims>"` — the identity a stored vector set is comparable
/// against. Basename only, so moving `~/.bough` is not a model change.
fn model_id(model_path: &Path, dims: i64) -> String {
    let base = model_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{base}:{dims}")
}

/// `length(lembed(…)) / 4` — one float32 per dimension. NEVER hardcoded.
fn probe_dims(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT length(lembed('embed', 'probe')) / 4 AS d",
        [],
        |r| r.get(0),
    )
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
    conn.execute(
        &format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_index USING vec0(embedding float[{dims}])"
        ),
        [],
    )?;
    Ok(rebuilt)
}

/// One drain batch. **Counted by `count(*)` DELTA, never by the statement's
/// `changes`**: an insert into a vec0 virtual table reports its SHADOW-table
/// writes too (4 rows once came back as 14).
fn drain_once(conn: &Connection) -> rusqlite::Result<u64> {
    let count = |c: &Connection| -> rusqlite::Result<i64> {
        c.query_row("SELECT count(*) AS n FROM vec_index", [], |r| r.get(0))
    };
    let before = count(conn)?;
    conn.execute(
        &format!(
            "INSERT INTO vec_index (rowid, embedding)
              SELECT h.id, lembed('embed', h.tags || ' ' || substr(h.cmd, 1, {DOC_CMD_CHARS}))
                FROM src.command_history h
               WHERE h.id NOT IN (SELECT rowid FROM vec_index)
               ORDER BY h.id
               LIMIT {DRAIN_BATCH}"
        ),
        [],
    )?;
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

/// Load sqlite-lembed into one connection. Extension loading is enabled only for
/// the duration of the load — the handle then goes back to refusing it, so
/// nothing downstream (the ATTACHed bough.db, a later query) can be steered into
/// loading anything else.
fn load_lembed(conn: &Connection, path: &Path) -> Result<(), BoughError> {
    // SAFETY: loading a native extension is inherently unsafe; the path is a
    // machine-local install (env or `~/.bough/lib`), never user input.
    let loaded = unsafe {
        conn.load_extension_enable()
            .map_err(|e| embed_err(format!("cannot enable extension loading: {e}")))?;
        let loaded = conn.load_extension(path, None::<&str>).map_err(|e| {
            embed_err(format!(
                "cannot load sqlite-lembed from {}: {e}",
                path.display()
            ))
        });
        // Disabled again whether or not the load worked: while it is enabled,
        // plain SQL can call `load_extension(...)` too, and this handle goes on
        // to run queries built from the ATTACHed history.
        conn.load_extension_disable()
            .map_err(|e| embed_err(format!("cannot disable extension loading: {e}")))?;
        loaded
    };
    loaded?;
    Ok(())
}

/// Blocking model download, on a thread of its own so it is safe to call from
/// inside `spawn_blocking`: a tokio worker context refuses to host the nested
/// runtime `reqwest::blocking` builds.
fn download_to(url: &str, dest: &Path) -> Result<(), BoughError> {
    let url = url.to_string();
    let dest = dest.to_path_buf();
    std::thread::spawn(move || -> Result<(), String> {
        let res = reqwest::blocking::Client::builder()
            .build()
            .map_err(|e| e.to_string())?
            .get(&url)
            .send()
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!(
                "model download failed: HTTP {} for {url}",
                res.status().as_u16()
            ));
        }
        let bytes = res.bytes().map_err(|e| e.to_string())?;
        std::fs::write(&dest, &bytes).map_err(|e| e.to_string())
    })
    .join()
    .map_err(|_| embed_err("model download thread panicked".to_string()))?
    .map_err(embed_err)
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
    fn model_id_is_basename_and_probed_dims() {
        assert_eq!(
            model_id(Path::new("/a/b/mini.q8_0.gguf"), 384),
            "mini.q8_0.gguf:384"
        );
        assert_eq!(
            model_id(Path::new("/other/mini.q8_0.gguf"), 384),
            "mini.q8_0.gguf:384"
        );
        assert_eq!(model_id(Path::new("/a/b/wide.gguf"), 768), "wide.gguf:768");
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

    /// `~/.bough/lib/lembed0.<ext>` is the documented install location, and an
    /// explicit `$BOUGH_LEMBED_PATH` wins outright. Absent → the feature does
    /// not exist, which is what makes `create_embed_layer` answer `None`.
    #[test]
    fn lembed_path_prefers_the_env_override() {
        let dir = tmpdir("lembed-path");
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("lib")).unwrap();
        std::fs::write(home.join("lib").join(LEMBED_FILE), b"x").unwrap();
        let explicit = dir.join("elsewhere").join(LEMBED_FILE);

        let prev_home = std::env::var("BOUGH_HOME").ok();
        std::env::set_var("BOUGH_HOME", &home);
        std::env::set_var(LEMBED_PATH_ENV, &explicit);
        assert_eq!(
            lembed_extension_path(),
            Some(explicit),
            "a named path wins even if missing"
        );
        std::env::remove_var(LEMBED_PATH_ENV);
        assert_eq!(
            lembed_extension_path(),
            Some(home.join("lib").join(LEMBED_FILE))
        );
        std::fs::remove_dir_all(home.join("lib")).unwrap();
        assert_eq!(
            lembed_extension_path(),
            None,
            "absent → the feature does not exist"
        );
        match prev_home {
            Some(v) => std::env::set_var("BOUGH_HOME", v),
            None => std::env::remove_var("BOUGH_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE FIXTURE (port of `embed_fixture.ts` + `embed.test.ts`): four seeded
    /// commands, one drain, and a query no keyword search could answer — "how do
    /// I get into the running container" must retrieve
    /// `docker exec -it myapp-dev-1 bash`.
    ///
    /// Skipped without a real model + a real sqlite-lembed, exactly as the TS
    /// test skips without the GGUF: the layer is optional by design, and a test
    /// that downloaded 25MB would not belong in `cargo test`. Unlike TS this
    /// needs NO subprocess — there is no `setCustomSQLite` window to lose.
    #[test]
    fn drain_embeds_recorded_commands_and_similar_answers_a_fuzzy_query() {
        use crate::schema::parts::{Session, SessionKind};
        use crate::types::{CommandRecord, Db};

        enable_sqlite_extensions();
        let model = bough_path(&["models", DEFAULT_MODEL_FILE]);
        match lembed_extension_path() {
            Some(p) if p.exists() => {}
            _ => {
                eprintln!(
                    "skipped: no sqlite-lembed (set {LEMBED_PATH_ENV} or install ~/.bough/lib/{LEMBED_FILE})"
                );
                return;
            }
        }
        if !model.exists() {
            eprintln!("skipped: no local model at {}", model.display());
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
            model_path: Some(path_str(&model)),
        }))
        .expect("layer exists when model + lembed are both present");

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
