//! Invariant: the file view is a PURE FUNCTION of the ledger plus ONE write (§2.7, V8). Both
//! providers render the same trajectory to the same bytes, and `write_file_view` puts exactly
//! those bytes on disk.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Class, LedgerHandle, Ref, StepId, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};
use bough_plugin_projection::{FileViewRequest, Projector};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use chrono::{DateTime, TimeZone, Utc};

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

fn traj() -> TrajId {
    TrajId::new("t-sol")
}

fn cfg(dir: std::path::PathBuf) -> AssemblerConfig {
    AssemblerConfig {
        budget_tokens: 100_000,
        headroom: 1.0,
        tail_steps: 12,
        tail_floor_steps: 3,
        mail_newest_n: 2,
        max_tiers: 3,
        file_view_dir: dir,
    }
}

/// Seed the same three-step trajectory into a ledger and return an assembler over it.
async fn assembler(ledger: LedgerHandle, ctx: Context, dir: std::path::PathBuf) -> Arc<Assembler> {
    ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new("sol"),
            traj: traj(),
            routing_refs: BTreeSet::from([Ref::new("gh:bough/rebuild#1")]),
            wake_classes: BTreeSet::new(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("agents is mutable config");
    for (n, wake) in [("s1", "w1"), ("s2", "w2"), ("s3", "w1")] {
        ledger
            .0
            .append(Append {
                traj: traj(),
                wake: WakeId::new(wake),
                kind: StepType::new("step/start"),
                class: Class::Thought,
                body: serde_json::json!({ "index": 1u32 }),
                cites: Vec::new(),
                at: at(),
                id: Some(StepId::new(n)),
            })
            .await
            .unwrap_or_else(|e| panic!("append {n}: {e}"));
    }
    Assembler::new(Arc::new(cfg(dir)), ledger, ctx)
}

async fn memory(dir: std::path::PathBuf) -> Arc<Assembler> {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
    assembler(ledger, ctx, dir).await
}

async fn sqlite(dir: std::path::PathBuf, db: &std::path::Path) -> Arc<Assembler> {
    let ctx = Context::root(KernelCore::new());
    let store = SqliteStore::open(
        &SqliteConfig {
            path: db.to_path_buf(),
            busy_timeout_ms: 5_000,
        },
        ctx.clone(),
    )
    .expect("a fresh db opens");
    assembler(LedgerHandle(store), ctx, dir).await
}

#[tokio::test]
async fn file_view_writes_the_trajectory_to_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("views");
    let a = memory(out.clone()).await;
    let req = FileViewRequest {
        traj: traj(),
        at: at(),
    };
    let text = a.file_view(&req).await.expect("the render is a read");
    assert!(!out.exists(), "file_view alone writes nothing");

    let path = a
        .write_file_view(&req, None)
        .await
        .expect("write_file_view creates its directory");
    assert_eq!(
        path,
        out.join("t-sol.txt"),
        "the default directory is the configured one"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        text,
        "the file holds exactly what file_view returned"
    );

    // An explicit directory wins over the config.
    let elsewhere = tmp.path().join("elsewhere");
    let path2 = a.write_file_view(&req, Some(&elsewhere)).await.unwrap();
    assert_eq!(path2, elsewhere.join("t-sol.txt"));
    assert_eq!(std::fs::read_to_string(&path2).unwrap(), text);
}

#[tokio::test]
async fn file_view_is_byte_identical_on_both_providers() {
    let tmp = tempfile::tempdir().unwrap();
    let req = FileViewRequest {
        traj: traj(),
        at: at(),
    };
    let m = memory(tmp.path().join("m"))
        .await
        .file_view(&req)
        .await
        .unwrap();
    let s = sqlite(tmp.path().join("s"), &tmp.path().join("ledger.db"))
        .await
        .file_view(&req)
        .await
        .unwrap();
    assert_eq!(s, m, "the file view differs between the two providers");
}

/// A `TrajId` is an unvalidated branded string and the codebase's own fixtures use slash-bearing
/// ids (`lane/sol`). The write must land inside the view dir whatever the id says.
#[tokio::test]
async fn a_slash_bearing_traj_id_writes_inside_the_view_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("views");
    let a = memory(out.clone()).await;
    for id in ["lane/sol", "/etc/passwd", "../escape"] {
        let req = FileViewRequest {
            traj: TrajId::new(id),
            at: at(),
        };
        let path = a
            .write_file_view(&req, None)
            .await
            .unwrap_or_else(|e| panic!("`{id}`: {e}"));
        assert_eq!(
            path.parent(),
            Some(out.as_path()),
            "`{id}` wrote to {}",
            path.display()
        );
        assert!(path.exists(), "`{id}` named a file that was not written");
    }
}
