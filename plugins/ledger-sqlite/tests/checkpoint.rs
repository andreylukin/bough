//! phase ux1 §2.10 (M28): a ledger that was written to and then disposed is COMPLETE on disk.
//!
//! The audit relaunched into an empty transcript and found a 231k `-wal` beside a 4.1k `.db`:
//! every step of the session was in the write-ahead log and nothing had folded it back. The
//! disposer now checkpoints with `TRUNCATE` before retiring the store, so a reopen — by this
//! binary, by `sqlite3`, by anything — sees every row.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{Append, Class, LedgerHandle, StepType, TrajId, WakeId};
use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};

fn append(i: u32) -> Append {
    Append {
        traj: TrajId::new("t1"),
        wake: WakeId::new("w1"),
        kind: StepType::new("step/start"),
        class: Class::Thought,
        body: serde_json::json!({ "index": i }),
        cites: vec![],
        at: chrono::Utc::now(),
        id: None,
    }
}

#[tokio::test]
async fn disposal_checkpoints_and_a_reopen_sees_every_step() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bough.db");
    let cfg = SqliteConfig {
        path: path.clone(),
        busy_timeout_ms: 5_000,
    };

    // A session: open, write, dispose.
    {
        let ctx = Context::root(KernelCore::new());
        let store = SqliteStore::open(&cfg, ctx.clone()).expect("the ledger opens");
        let ledger = LedgerHandle(store.clone() as Arc<_>);
        for i in 0..64 {
            ledger.0.append(append(i)).await.expect("append");
        }
        // What the row's disposer does, in the order it does it.
        store.checkpoint().await.expect("the checkpoint runs");
        store.retire();
    }

    // The WAL is folded back: what is left is at most a page of header, never a session.
    let wal = path.with_extension("db-wal");
    if wal.exists() {
        let len = std::fs::metadata(&wal).unwrap().len();
        assert!(
            len <= 4_096,
            "a checkpointed WAL is at most a page; found {len} bytes"
        );
    }

    // The relaunch.
    let ctx = Context::root(KernelCore::new());
    let store = SqliteStore::open(&cfg, ctx).expect("the ledger reopens");
    let ledger = LedgerHandle(store as Arc<_>);
    let steps = ledger.0.tail(&TrajId::new("t1"), 1_000).await.unwrap();
    assert_eq!(steps.len(), 64, "every step of the last session is there");
    assert_eq!(steps[0].body.get("index").unwrap(), 0);
    assert_eq!(steps[63].body.get("index").unwrap(), 63);
}
