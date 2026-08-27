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

/// The WAL is folded back: what is left is at most a page of header, never a session.
///
/// UNCONDITIONAL. It used to be wrapped in `if wal.exists()`, which made the one assertion that
/// actually tests M28's fix skippable — and the "a relaunch sees every step" half below would
/// pass with no checkpoint at all, because a SQLite reader reads the WAL.
fn assert_wal_folded(path: &std::path::Path) {
    let wal = path.with_extension("db-wal");
    let len = if wal.exists() {
        std::fs::metadata(&wal).unwrap().len()
    } else {
        0
    };
    assert!(
        len <= 4_096,
        "a checkpointed WAL is at most a page; found {len} bytes at {}",
        wal.display()
    );
}

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
async fn a_hand_run_checkpoint_then_retire_leaves_a_folded_wal() {
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

    assert_wal_folded(&path);

    // The relaunch.
    let ctx = Context::root(KernelCore::new());
    let store = SqliteStore::open(&cfg, ctx).expect("the ledger reopens");
    let ledger = LedgerHandle(store as Arc<_>);
    let steps = ledger.0.tail(&TrajId::new("t1"), 1_000).await.unwrap();
    assert_eq!(steps.len(), 64, "every step of the last session is there");
    assert_eq!(steps[0].body.get("index").unwrap(), 0);
    assert_eq!(steps[63].body.get("index").unwrap(), 63);
}

/// The same fact, through the REAL disposer. The test above calls `checkpoint()` and `retire()`
/// by hand under a comment that says "what the row's disposer does" — which proves the store's
/// methods, not the row's teardown. This one mounts the `ledger.sqlite` row in a kernel, writes
/// through the service it provides, and unloads the tree: if the disposer stops checkpointing, or
/// is never scheduled, this fails and the hand-run test does not.
#[tokio::test]
async fn the_rows_own_disposal_checkpoints_and_a_relaunch_sees_every_step() {
    use bough_kernel::{
        Catalog, Composer, Composition, ExprEnv, Kernel, KernelOptions, LayerId, Patch,
    };
    use bough_plugin_ledger::Ledger;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bough.db");
    let yaml = format!(
        "- id: ledger.sqlite\n  plugin: ledger-sqlite\n  config: {{ path: {:?}, busy_timeout_ms: 5000 }}\n",
        path.to_string_lossy()
    );

    let catalog = Catalog::from_inventory().expect("catalog");
    let patch: Patch = serde_yaml::from_str(&yaml).expect("the row parses");
    let mut composer = Composer::new(&catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    let composition: Composition = composer.compose().expect("the row composes");
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: true,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;

    {
        let ledger = kernel
            .root()
            .peek_live::<Ledger>()
            .expect("the row provides `ledger`");
        for i in 0..64 {
            ledger.0.append(append(i)).await.expect("append");
        }
    }

    // Teardown, exactly as the launcher runs it.
    kernel.shutdown().await;

    assert_wal_folded(&path);

    let cfg = SqliteConfig {
        path: path.clone(),
        busy_timeout_ms: 5_000,
    };
    let ctx = Context::root(KernelCore::new());
    let store = SqliteStore::open(&cfg, ctx).expect("the ledger reopens");
    let ledger = LedgerHandle(store as Arc<_>);
    let steps = ledger.0.tail(&TrajId::new("t1"), 1_000).await.unwrap();
    assert_eq!(steps.len(), 64, "every step of the last session is there");
}
