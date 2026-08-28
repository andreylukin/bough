//! Invariant: ONE writer. Every store call runs inside `tokio::task::spawn_blocking` over one
//! `Arc<Mutex<Connection>>` — that mutex IS §3's single writer — and `seq` is allocated by
//! `MAX(seq)+1` INSIDE the insert transaction, so two concurrent appends can neither collide nor
//! gap (P1-D9, P1-D15).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_ledger::{LedgerError, StepTypeMap};
use parking_lot::Mutex;
use rusqlite::Connection;

use crate::schema::store_err;
use crate::SqliteConfig;

/// The store behind the `ledger` binding.
pub struct SqliteStore {
    #[doc(hidden)]
    pub(crate) conn: Arc<Mutex<Connection>>,
    /// The merge-extensible step-type map, preloaded with the sixteen builtins.
    pub(crate) types: Arc<StepTypeMap>,
    /// The provider's captured context: `ledger/step` is emitted from it, post-commit.
    pub(crate) ctx: Context,
    /// Rows skipped on read because their type was unknown AND ignorable.
    pub(crate) skipped: Arc<AtomicU64>,
    /// Set by the row's teardown. A retired store refuses every call: an `Arc<dyn LedgerStore>`
    /// clone that outlives the row (the assembler captures one) must not keep writing through a
    /// retired Context whose `ledger/step` listener is already disposed (§0.2, "unload leaves no
    /// trace").
    pub(crate) retired: Arc<AtomicBool>,
}

/// How long a disposal-time checkpoint waits out another connection that is still holding the
/// ledger open. Long enough for a pane's last read to finish, short enough that a shutdown that
/// cannot checkpoint says so instead of hanging.
const CHECKPOINT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

impl SqliteStore {
    /// Open (or create) the db, check the format version, install the schema and the builtins.
    pub fn open(cfg: &SqliteConfig, ctx: Context) -> Result<Arc<SqliteStore>, LedgerError> {
        let path = cfg.path.to_string_lossy().to_string();
        let in_memory = path == ":memory:";
        let conn = if in_memory {
            Connection::open_in_memory()
        } else {
            Connection::open(&cfg.path)
        }
        .map_err(store_err)?;

        conn.busy_timeout(std::time::Duration::from_millis(cfg.busy_timeout_ms))
            .map_err(store_err)?;
        // WAL is a file-mode pragma; ":memory:" has no journal to move.
        if !in_memory {
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(store_err)?;
        }
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(store_err)?;

        crate::schema::open_and_migrate(&conn, &path)?;

        Ok(Arc::new(SqliteStore {
            conn: Arc::new(Mutex::new(conn)),
            types: Arc::new(StepTypeMap::with_builtins()),
            ctx,
            skipped: Arc::new(AtomicU64::new(0)),
            retired: Arc::new(AtomicBool::new(false)),
        }))
    }

    /// `PRAGMA wal_checkpoint(TRUNCATE)` (phase ux1 §2.10, M28). Called from the row's disposer
    /// BEFORE [`SqliteStore::retire`], so a relaunch always sees the whole ledger: a 231k WAL
    /// beside a 4.1k db is what an unclosed shutdown looks like, and the history the user typed
    /// is what it loses.
    pub async fn checkpoint(&self) -> Result<(), LedgerError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock();
            // `query_row` and not `execute`: the pragma RETURNS a row (busy, log, checkpointed),
            // and rusqlite refuses `execute` on a statement that yields one.
            //
            // And the FIRST column is the one that matters: a checkpoint blocked by another live
            // connection reports `busy = 1` and does NOT error, so reading the row away — which is
            // what `|_| Ok(())` did — reported success while leaving the whole WAL on disk. That is
            // M28's symptom with none of its evidence, and it is what
            // `scripts/tui/24-honesty.sh::the_shutdown_left_no_wal_over_a_page` caught under the
            // full suite's load (445 KB of WAL beside a 131 KB db, after `bough: bye.`). Retry
            // within a bounded window, then fail loudly so the disposer's `tracing::error!` fires.
            let deadline = std::time::Instant::now() + CHECKPOINT_WAIT;
            loop {
                let busy: i64 = c
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
                    .map_err(store_err)?;
                if busy == 0 {
                    return Ok(());
                }
                if std::time::Instant::now() >= deadline {
                    return Err(LedgerError::Store(anyhow::anyhow!(
                        "the WAL checkpoint stayed busy for {CHECKPOINT_WAIT:?}: another \
                         connection is still holding the ledger open, and the WAL is on disk"
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        })
        .await
        .map_err(|e| LedgerError::Store(anyhow::anyhow!("checkpoint task failed: {e}")))?
    }

    /// Poison the store. Called by the row's teardown; irreversible for this store instance.
    pub fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
    }

    /// Whether this store has been retired.
    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::SeqCst)
    }

    /// Run `f` against the single connection on a blocking thread.
    pub(crate) async fn with_conn<T, F>(&self, f: F) -> Result<T, LedgerError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, LedgerError> + Send + 'static,
    {
        if self.is_retired() {
            return Err(LedgerError::Store(anyhow::anyhow!(
                "ledger-sqlite: this store was retired with its row; the handle is no longer usable"
            )));
        }
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock();
            f(&mut guard)
        })
        .await
        .map_err(|e| LedgerError::Store(anyhow::Error::new(e)))?
    }
}

/// The tail query, spelled once so the plan test and the read path cannot drift apart.
pub(crate) const TAIL_SQL: &str =
    "SELECT id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable \
     FROM steps WHERE traj_id = ?1 ORDER BY seq DESC LIMIT ?2";

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use bough_kernel::{Context, KernelCore};
    use bough_plugin_ledger::{Append, Class, LedgerStore, StepType, TrajId, WakeId};
    use chrono::Utc;

    use super::*;

    fn store() -> Arc<SqliteStore> {
        let ctx = Context::root(KernelCore::new());
        SqliteStore::open(
            &SqliteConfig {
                path: ":memory:".into(),
                busy_timeout_ms: 5000,
            },
            ctx,
        )
        .expect("in-memory ledger opens")
    }

    fn note(traj: &TrajId, index: u32) -> Append {
        Append {
            traj: traj.clone(),
            wake: WakeId::new("w1"),
            kind: StepType::new("step/start"),
            class: Class::Thought,
            body: serde_json::json!({ "index": index }),
            cites: vec![],
            at: Utc::now(),
            id: None,
        }
    }

    /// The seq a caller sees is the seq the row got, and it is handed out under the write
    /// transaction — not read before it, which is what would let two appends agree on the same
    /// number.
    #[tokio::test]
    async fn seq_is_allocated_inside_the_transaction() {
        let s = store();
        let traj = TrajId::new("t");
        let a = s.append(note(&traj, 0)).await.expect("first append");
        let b = s.append(note(&traj, 1)).await.expect("second append");
        assert_eq!((a.seq.0, b.seq.0), (1, 2));

        // The rows carry exactly those seqs, and the UNIQUE(traj_id, seq) constraint means a
        // duplicate could not have been committed silently.
        let seqs: BTreeSet<u64> = s
            .steps(&Default::default())
            .await
            .expect("read back")
            .into_iter()
            .map(|st| st.seq.0)
            .collect();
        assert_eq!(seqs, BTreeSet::from([1, 2]));
    }

    #[tokio::test]
    async fn thirty_two_concurrent_appends_produce_seqs_one_to_thirty_two() {
        let s = store();
        let traj = TrajId::new("t");
        let mut tasks = Vec::new();
        for i in 0..32u32 {
            let s = s.clone();
            let traj = traj.clone();
            tasks.push(tokio::spawn(async move {
                s.append(note(&traj, i)).await.expect("concurrent append")
            }));
        }
        let mut seqs = Vec::new();
        for t in tasks {
            seqs.push(t.await.expect("task joins").seq.0);
        }
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=32).collect::<Vec<u64>>(), "no collision, no gap");
    }

    /// `tail` must be an index seek on (traj_id, seq), not a scan of the whole table: the
    /// projection's verbatim band calls it on every assembly.
    #[tokio::test]
    async fn tail_uses_the_seq_index() {
        let s = store();
        let plan: Vec<String> = s
            .with_conn(|conn| {
                let mut stmt = conn
                    .prepare(&format!("EXPLAIN QUERY PLAN {TAIL_SQL}"))
                    .map_err(store_err)?;
                let rows = stmt
                    .query_map(rusqlite::params!["t", 10i64], |r| r.get::<_, String>(3))
                    .map_err(store_err)?
                    .collect::<Result<Vec<String>, _>>()
                    .map_err(store_err)?;
                Ok(rows)
            })
            .await
            .expect("query plan");
        let plan = plan.join(" | ");
        assert!(
            plan.contains("USING INDEX") || plan.contains("USING COVERING INDEX"),
            "tail is not index-driven: {plan}"
        );
        assert!(
            !plan.contains("SCAN steps"),
            "tail scans the whole table: {plan}"
        );
    }
}
