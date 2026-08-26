//! V7, the production half: the LAUNCHER's live watch path — not a harness reproduction of it —
//! must leave the last good tree running and broadcast `config-update-failed` when the patch file
//! on disk stops composing.
//!
//! `crates/bough/tests/bad_patch.rs` emits the broadcast from its own harness, so it proves the
//! kernel's half (the tree survives) but not the launcher's. This test calls
//! `bough::watch::watch_user_patch` — the very function `boot()` calls — against a real kernel with
//! a real `$BOUGH_HOME`, and writes a real file.
//!
//! One test per binary: it sets `BOUGH_HOME`, which is process-global.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bough_kernel::{Catalog, FiberState, Kernel, KernelOptions};

fn find_state(rows: &[bough_kernel::RowSnapshot], id: &str) -> Option<FiberState> {
    for r in rows {
        if r.id.as_str() == id {
            return Some(r.state);
        }
        if let Some(s) = find_state(&r.children, id) {
            return Some(s);
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread")]
async fn a_patch_that_stops_composing_broadcasts_and_leaves_the_tree_running() {
    let home = tempfile::tempdir().unwrap();
    // Deliberately the UNCANONICALISED path: on macOS a TempDir is `/var/...`, a symlink to
    // `/private/var/...`, which is where the OS reports the change. A user's $BOUGH_HOME can be
    // symlinked the same way, so the watch has to cope.
    let home_path = home.path().to_path_buf();
    unsafe { std::env::set_var("BOUGH_HOME", &home_path) };

    let cli = Arc::new(bough::cli::Cli {
        profile: "tui".into(),
        patches: Vec::new(),
        dump_config: false,
        dump_format: bough::cli::DumpFormat::Yaml,
        check: false,
        no_watch: false,
        root: Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .unwrap(),
        ),
        command: None,
    });

    let catalog = Catalog::from_inventory().expect("catalog");
    let (profile, composition) = bough::compose::compose_plan(&cli, &catalog).expect("composes");
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: profile.name.clone(),
            invariants: profile.invariants,
        },
    );
    kernel.load(composition).await.expect("mounts");
    kernel.quiesce().await;

    let good = kernel.snapshot();
    assert_eq!(
        find_state(&good.rows, "hello.greeter"),
        Some(FiberState::Active),
        "the fixture must boot before the bad patch means anything"
    );

    let failures = Arc::new(AtomicUsize::new(0));
    let sink = failures.clone();
    kernel
        .root()
        .on::<bough_kernel::event::ConfigUpdateFailed, _, _>(move |_| {
            let sink = sink.clone();
            async move {
                sink.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await
        .expect("listener");

    let watch = bough::watch::watch_user_patch(Arc::clone(&kernel), Arc::clone(&cli));

    // A plugin name no catalog entry has: the candidate cannot compose.
    std::fs::write(
        home_path.join("bough.patch.yml"),
        "entries:\n  greeting.provider:\n    plugin: greeting-whisper\n",
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while failures.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the launcher's watch must broadcast config-update-failed for a patch that \
             names a plugin outside the catalog"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    kernel.quiesce().await;
    let now = kernel.snapshot();
    assert_eq!(
        now.fingerprint, good.fingerprint,
        "the last good tree must still be the running tree"
    );
    assert_eq!(
        find_state(&now.rows, "hello.greeter"),
        Some(FiberState::Active),
        "the rejected candidate must not have disturbed a running row"
    );

    watch.stop().await;
    kernel.shutdown().await;
}
