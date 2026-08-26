//! Invariant: a rejected candidate never disturbs the running tree. The watch recomposes and calls
//! `kernel.update`; a failed recompose is logged, broadcast as `config-update-failed`, and
//! watching continues, because the last good tree is still running (§0.3). An `update` that fails
//! has already broadcast from inside the kernel; a candidate that fails to COMPOSE never reaches
//! the kernel, so the broadcast is issued here.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Catalog, Kernel};

use crate::cli::{BootError, Cli};
use crate::compose::compose_for;

/// Debounce window for the patch file: an editor's write-truncate-rename dance is one change.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// A running watch. Dropping it, or calling [`WatchHandle::stop`], stops watching.
///
/// Not a `bough_kernel::EffectHandle`: an `EffectHandle` is minted by the kernel for a fiber's
/// accumulator, and the launcher owns no fiber. The deviation is recorded in the phase notes.
pub struct WatchHandle {
    _debouncer: notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    task: tokio::task::JoinHandle<()>,
}

impl WatchHandle {
    /// Stop watching and drop the recompose task.
    pub fn stop(self) {
        self.task.abort();
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Watch `bough_util::user_patch_path()` with notify + a debouncer; on change, recompose through
/// [`compose_for`] and hand the result to `kernel.update`.
pub fn watch_user_patch(kernel: Arc<Kernel>, cli: Arc<Cli>) -> WatchHandle {
    let path = bough_util::user_patch_path();
    // Watch the DIRECTORY: the file may not exist yet, and editors replace it by rename.
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let _ = bough_util::ensure_dir(&dir);
    // Canonicalise: on macOS `$BOUGH_HOME` under `/var` is a symlink and the OS reports events
    // under `/private/var`, so an uncanonicalised comparison never matches and the watch is silent.
    let dir = dir.canonicalize().unwrap_or(dir);
    let watched = dir.join(
        path.file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("bough.patch.yml")),
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut debouncer = notify_debouncer_full::new_debouncer(
        DEBOUNCE,
        None,
        move |res: notify_debouncer_full::DebounceEventResult| match res {
            Ok(events) => {
                if events.iter().any(|e| e.paths.contains(&watched)) {
                    let _ = tx.send(());
                }
            }
            Err(errs) => {
                for e in errs {
                    tracing::warn!("bough: patch watch error: {e}");
                }
            }
        },
    )
    .expect("notify: could not create the patch-file watcher");

    if let Err(e) = debouncer.watch(&dir, notify::RecursiveMode::NonRecursive) {
        tracing::warn!("bough: cannot watch {}: {e}", dir.display());
    }

    let task = tokio::spawn(async move {
        while rx.recv().await.is_some() {
            recompose_once(&kernel, &cli).await;
        }
    });

    WatchHandle {
        _debouncer: debouncer,
        task,
    }
}

/// One recompose attempt. Every failure path logs and returns: the last good tree keeps running.
async fn recompose_once(kernel: &Kernel, cli: &Cli) {
    let catalog = match Catalog::from_inventory() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("bough: catalog: {e}");
            return;
        }
    };
    match compose_for(cli, &catalog) {
        Ok(c) => {
            if let Err(e) = kernel.update(c).await {
                tracing::warn!("bough: patch rejected, last good tree still running: {e}");
            }
        }
        Err(e) => {
            tracing::warn!("bough: patch rejected, last good tree still running: {e}");
            kernel.report_config_update_failed(Arc::new(compose_error(e)));
        }
    }
}

/// The broadcast payload is an `Arc<ComposeError>`; a `BootError` that already carries one hands
/// it over, and anything else (a missing bundle, an unreadable file) is reported by its message.
fn compose_error(e: BootError) -> bough_kernel::ComposeError {
    match e {
        BootError::Compose(c) => c,
        other => bough_kernel::ComposeError::BadYaml {
            layer: bough_kernel::LayerId::new("user"),
            detail: other.to_string(),
        },
    }
}
