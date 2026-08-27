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
    debouncer: Option<
        notify_debouncer_full::Debouncer<
            notify::RecommendedWatcher,
            notify_debouncer_full::RecommendedCache,
        >,
    >,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// How long [`WatchHandle::stop`] waits for an in-flight recompose to finish.
const STOP_CEILING: Duration = Duration::from_secs(10);

impl WatchHandle {
    /// Stop watching, COOPERATIVELY: drop the watcher so the channel closes, then await the
    /// recompose task.
    ///
    /// Never `abort()`. `Kernel::update_tree` is not cancellation-safe — aborting inside it would
    /// leave disposed fibers, a stale recorded tree and orphaned row bookkeeping, which is exactly
    /// the "a rejected or interrupted candidate never disturbs the running tree" rule (§0.3). This
    /// is also the normal SIGINT path, not an edge case.
    pub async fn stop(mut self) {
        drop(self.debouncer.take());
        if let Some(task) = self.task.take() {
            if tokio::time::timeout(STOP_CEILING, task).await.is_err() {
                tracing::error!("bough: a recompose was still running after {STOP_CEILING:?}");
            }
        }
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
    let debouncer = notify_debouncer_full::new_debouncer(
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
    );

    // The OS can refuse a watcher (inotify/FSEvents limits, fd exhaustion). This whole path exists
    // so that a failure here never disturbs the running tree, so it degrades to "no live reload"
    // exactly as a failed `watch()` below does — it does not panic the launcher.
    let mut debouncer = match debouncer {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                "bough: could not create the patch-file watcher ({e}); \
                 live recomposition is off for this run"
            );
            return WatchHandle {
                debouncer: None,
                task: None,
            };
        }
    };

    if let Err(e) = debouncer.watch(&dir, notify::RecursiveMode::NonRecursive) {
        tracing::warn!("bough: cannot watch {}: {e}", dir.display());
    }

    let task = tokio::spawn(async move {
        // Ends when the channel closes, which is what `WatchHandle::stop` does by dropping the
        // debouncer: the loop finishes whatever recompose is in flight and then returns.
        while rx.recv().await.is_some() {
            let _ = recompose_once(&kernel, &cli).await;
        }
    });

    WatchHandle {
        debouncer: Some(debouncer),
        task: Some(task),
    }
}

/// One recompose attempt. Every failure path logs and returns: the last good tree keeps running.
///
/// `pub` so the launcher's integration tests drive the REAL live path rather than a reproduction
/// of it; `boot()` reaches it only through the watch task.
pub async fn recompose_once(kernel: &Kernel, cli: &Cli) -> Result<(), BootError> {
    let before = row_uids(kernel);
    let catalog = match Catalog::from_inventory() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("bough: catalog: {e}");
            reload(
                kernel,
                ConfigReload::Rejected {
                    detail: e.to_string(),
                },
            );
            return Err(BootError::Catalog(e));
        }
    };
    match compose_for(cli, &catalog) {
        Ok(c) => {
            // §0.2: a patch naming an absent row id is a warning, never an error — and the live
            // path is the one a user actually edits patches through, so it must say so too.
            crate::boot::report_warnings(&c.warnings);
            match kernel.update(c).await {
                Ok(()) => {
                    reload(
                        kernel,
                        ConfigReload::Applied {
                            rows_changed: changed(&before, &row_uids(kernel)),
                        },
                    );
                    Ok(())
                }
                Err(e) => {
                    // The kernel has already broadcast `config-update-failed` from inside
                    // `update_tree`; it is not re-broadcast here.
                    tracing::warn!("bough: patch rejected, last good tree still running: {e}");
                    reload(
                        kernel,
                        ConfigReload::Rejected {
                            detail: e.to_string(),
                        },
                    );
                    Err(BootError::Kernel(e))
                }
            }
        }
        Err(BootError::Compose(c)) => {
            tracing::warn!("bough: patch rejected, last good tree still running: {c}");
            let shared = Arc::new(c);
            kernel.report_config_update_failed(shared.clone());
            reload(
                kernel,
                ConfigReload::Rejected {
                    detail: shared.to_string(),
                },
            );
            Err(BootError::ComposeShared(shared))
        }
        Err(other) => {
            // A missing bundle, an unreadable file: not a `ComposeError`, but still a rejected
            // candidate, so §0.3's broadcast is unconditional.
            tracing::warn!("bough: patch rejected, last good tree still running: {other}");
            kernel.report_config_update_failed(Arc::new(bough_kernel::ComposeError::BadYaml {
                layer: bough_kernel::LayerId::new("user"),
                detail: other.to_string(),
            }));
            reload(
                kernel,
                ConfigReload::Rejected {
                    detail: other.to_string(),
                },
            );
            Err(other)
        }
    }
}

// ---------------------------------------------------------------------------
// phase ux1 §2.9 (M15): the reload result reaches the SCREEN, not only the log
// ---------------------------------------------------------------------------

/// `config/reload` — EMIT. The launcher raises it after every recompose attempt; `tui-shell`
/// listens and renders the SAME TEXT the log gets, which is M15's whole complaint. A headless
/// profile simply has no listener, so the behaviour there is unchanged.
pub struct ConfigReloadEvent;

impl bough_kernel::EmitEvent for ConfigReloadEvent {
    const NAME: &'static str = "config/reload";
    type Payload = ConfigReload;
}

/// What one recompose attempt did.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigReload {
    Applied { rows_changed: usize },
    Rejected { detail: String },
}

impl ConfigReload {
    /// PURE: the ONE LINE the screen shows, which is the same text the log gets (M15). A user who
    /// edited a patch and saw nothing happen could not tell a no-op from a rejection.
    pub fn line(&self) -> String {
        match self {
            ConfigReload::Applied { rows_changed: 0 } => {
                "config reloaded — no row changed".to_string()
            }
            ConfigReload::Applied { rows_changed: 1 } => {
                "config reloaded — 1 row changed".to_string()
            }
            ConfigReload::Applied { rows_changed } => {
                format!("config reloaded — {rows_changed} rows changed")
            }
            ConfigReload::Rejected { detail } => {
                format!("config rejected, last good tree still running: {detail}")
            }
        }
    }

    pub fn is_rejection(&self) -> bool {
        matches!(self, ConfigReload::Rejected { .. })
    }
}

/// Raise `config/reload`. EMIT, so a profile with no listener pays nothing and the watch task is
/// never blocked by a surface.
fn reload(kernel: &Kernel, what: ConfigReload) {
    kernel.root().emit::<ConfigReloadEvent>(what);
}

/// The live rows, by fiber uid: the coordinate that changes when a row is reloaded, added or
/// removed. Counting THIS rather than trusting the patch means "no row changed" is a measurement.
fn row_uids(kernel: &Kernel) -> std::collections::BTreeMap<String, Option<u64>> {
    kernel
        .rows_snapshot()
        .into_iter()
        .map(|r| (r.id.to_string(), r.uid.map(|u| u.0)))
        .collect()
}

fn changed(
    before: &std::collections::BTreeMap<String, Option<u64>>,
    after: &std::collections::BTreeMap<String, Option<u64>>,
) -> usize {
    let mut n = 0;
    for (id, uid) in after {
        if before.get(id) != Some(uid) {
            n += 1;
        }
    }
    for id in before.keys() {
        if !after.contains_key(id) {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod reload_tests {
    use super::*;

    #[test]
    fn the_line_says_what_happened_in_the_logs_own_words() {
        assert_eq!(
            ConfigReload::Applied { rows_changed: 0 }.line(),
            "config reloaded — no row changed"
        );
        assert_eq!(
            ConfigReload::Applied { rows_changed: 1 }.line(),
            "config reloaded — 1 row changed"
        );
        assert!(ConfigReload::Applied { rows_changed: 3 }
            .line()
            .contains("3 rows changed"));
        let r = ConfigReload::Rejected {
            detail: "unknown field `wat`".into(),
        };
        assert!(r.line().contains("unknown field `wat`"), "{}", r.line());
        assert!(r.line().contains("last good tree still running"));
        assert!(r.is_rejection());
    }

    #[test]
    fn changed_counts_reloads_additions_and_removals() {
        use std::collections::BTreeMap;
        let before: BTreeMap<String, Option<u64>> = [
            ("a".to_string(), Some(1)),
            ("b".to_string(), Some(2)),
            ("gone".to_string(), Some(3)),
        ]
        .into_iter()
        .collect();
        let after: BTreeMap<String, Option<u64>> = [
            ("a".to_string(), Some(1)),   // untouched
            ("b".to_string(), Some(9)),   // reloaded
            ("new".to_string(), Some(4)), // added
        ]
        .into_iter()
        .collect();
        assert_eq!(changed(&before, &after), 3);
        assert_eq!(changed(&before, &before), 0);
    }
}
