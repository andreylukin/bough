//! The journal's on-disk half: the script mirror, and which script a relaunch
//! runs (port of `src/workflow/journal.ts`).
//!
//! THE INVARIANT THIS HOLDS: **the mirror is a working copy that may differ
//! from the row, and the difference is what the next relaunch consumes.**
//!
//!   - An existing mirror is never read-compared-rewritten by
//!     [`sync_script_mirrors`], so a restart cannot clobber an edit the user
//!     has not run yet.
//!   - [`resolve_rerun_script`] prefers the mirror over the stored row, because
//!     a relaunch that silently ran the row would replay the user's edit away.
//!   - The row stays canonical, so a relaunch is still possible after
//!     `~/.bough` has been cleaned out.
//!
//! PATH CONFINEMENT. A run id arrives from a URL and from a program's
//! `workflow.rerun({id})`, so it is not trusted to be a uuid: [`mirror_path`]
//! hands the RELATIVE name to `confine()`. The relative name and not the joined
//! path, because joining swallows a leading slash — `/etc/crontab` would land
//! back under the workflows directory and pass a check made after the join.

use std::path::{Path, PathBuf};

use crate::errors::BoughError;
use crate::paths::{confine, workflow_script_path, workflows_dir};
use crate::schema::parts::WorkflowRun;
use crate::types::SharedDb;

/// `~/.bough/workflows/<id>.js`, confined.
///
/// Confinement is on the server's own path construction, not on the program —
/// programs already write any file they like with the user's authority.
pub fn mirror_path(run_id: &str) -> Result<PathBuf, BoughError> {
    // The RELATIVE name is what is confined, not the already-joined path.
    confine(&workflows_dir(), Path::new(&format!("{run_id}.js")))?;
    Ok(workflow_script_path(run_id))
}

/// Write a run's script to its mirror. Returns whether the file is now on disk.
///
/// Best-effort by contract: the database row is canonical and a run must not
/// fail to start because `~/.bough` is read-only or full. The boolean is for
/// callers that report the surface, not for control flow.
pub async fn mirror_script(run_id: &str, script: &str) -> bool {
    let Ok(path) = mirror_path(run_id) else {
        return false;
    };
    if tokio::fs::create_dir_all(workflows_dir()).await.is_err() {
        return false;
    }
    tokio::fs::write(path, script).await.is_ok()
}

/// A run's mirrored script, or `None` when there is no readable file.
pub async fn read_mirror(run_id: &str) -> Option<String> {
    let path = mirror_path(run_id).ok()?;
    tokio::fs::read_to_string(path).await.ok()
}

/// Options for [`sync_script_mirrors`]. Bounded to the most recent runs: the
/// mirror is an editing surface for work someone is still iterating on, not an
/// export of every run ever made.
pub struct SyncOptions {
    pub limit: usize,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions { limit: 50 }
    }
}

/// Recreate MISSING mirrors so "edit the script on disk" is true for every run
/// the database knows about. Returns the ids it wrote.
///
/// Boot wiring. Idempotent and cheap in the steady state: an existing file is
/// never read, compared or rewritten, so a user's edit is never clobbered by a
/// restart — the whole point of the file is that it may differ from the row.
pub async fn sync_script_mirrors(db: &SharedDb, opts: SyncOptions) -> Vec<String> {
    // `list_workflows(None)` is newest-first, so this is the N most recent runs.
    let runs: Vec<WorkflowRun> = {
        let guard = db.lock().expect("db mutex");
        match guard.list_workflows(None) {
            Ok(rows) => rows.into_iter().take(opts.limit).collect(),
            Err(_) => return vec![],
        }
    };
    let mut written = Vec::new();
    for run in runs {
        // An id that cannot name a file has no mirror; not fatal at boot.
        let Ok(path) = mirror_path(&run.id) else {
            continue;
        };
        // Present — never overwritten, it may hold the user's edit.
        if tokio::fs::metadata(&path).await.is_ok() {
            continue;
        }
        if mirror_script(&run.id, &run.script).await {
            written.push(run.id);
        }
    }
    written
}

/// Where a relaunch's script came from — reported, because it decides what runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptSource {
    Explicit,
    Mirror,
    Stored,
}

impl ScriptSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScriptSource::Explicit => "explicit",
            ScriptSource::Mirror => "mirror",
            ScriptSource::Stored => "stored",
        }
    }
}

/// Resolve the script a relaunch should run: an explicit one wins, else the
/// mirror the user may have edited, else the stored row.
///
/// The mirror before the row is the whole "edit the file, relaunch" loop. The
/// row as the last resort keeps a relaunch possible after `~/.bough/workflows`
/// has been cleaned out. A blank override is not an override — an empty string
/// is what a form posts when the user cleared the box, not an instruction to
/// run nothing.
pub async fn resolve_rerun_script(
    run: &WorkflowRun,
    override_script: Option<&str>,
) -> (String, ScriptSource) {
    if let Some(script) = override_script {
        if !script.trim().is_empty() {
            return (script.to_string(), ScriptSource::Explicit);
        }
    }
    if let Some(mirrored) = read_mirror(&run.id).await {
        if !mirrored.trim().is_empty() {
            return (mirrored, ScriptSource::Mirror);
        }
    }
    (run.script.clone(), ScriptSource::Stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parts::WorkflowStatus;

    fn run_row(id: &str, script: &str) -> WorkflowRun {
        WorkflowRun {
            id: id.into(),
            session_id: "s".into(),
            name: "w".into(),
            description: "d".into(),
            script: script.into(),
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

    /// `BOUGH_HOME` is process-global and cargo runs tests in parallel threads,
    /// so every test that relocates it takes the CRATE-WIDE lock in
    /// `paths::test_env` — a module-local lock only serializes this file against
    /// itself and still races `paths`, `scratch`, `saved` and every other module
    /// that moves the same variable. The body is async, so it runs on a
    /// current-thread runtime built INSIDE the guarded closure.
    fn with_home<F>(f: impl FnOnce(std::path::PathBuf) -> F)
    where
        F: std::future::Future<Output = ()>,
    {
        let home = std::env::temp_dir().join(format!("bough-wfjournal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("temp home");
        crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(f(home.clone()));
        });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_run_id_can_only_ever_name_a_file_inside_the_workflows_dir() {
        with_home(|_home| async move {
            assert!(mirror_path("../../etc/crontab").is_err());
            assert!(mirror_path("/etc/crontab").is_err());
            let ok = mirror_path("abc-123").expect("a plain id");
            assert!(ok.ends_with("workflows/abc-123.js"), "{}", ok.display());
        });
    }

    /// The relaunch resolution order IS the iteration loop: explicit → mirror →
    /// stored, and a blank override is not an override.
    #[test]
    fn resolve_prefers_explicit_then_the_mirror_then_the_row() {
        with_home(|_home| async move {
            let run = run_row("res-1", "// stored");
            // No mirror yet.
            assert_eq!(
                resolve_rerun_script(&run, None).await,
                ("// stored".to_string(), ScriptSource::Stored)
            );
            assert!(mirror_script("res-1", "// edited on disk").await);
            assert_eq!(
                resolve_rerun_script(&run, None).await,
                ("// edited on disk".to_string(), ScriptSource::Mirror)
            );
            // A blank override is what a cleared form posts — not an instruction.
            assert_eq!(
                resolve_rerun_script(&run, Some("   ")).await,
                ("// edited on disk".to_string(), ScriptSource::Mirror)
            );
            assert_eq!(
                resolve_rerun_script(&run, Some("// explicit")).await,
                ("// explicit".to_string(), ScriptSource::Explicit)
            );
        });
    }

    /// An existing mirror is never rewritten — the user's unrun edit survives a
    /// restart.
    #[test]
    fn sync_writes_only_missing_mirrors_and_never_clobbers_an_edit() {
        with_home(|home| async move {
            let db = crate::agents::testkit::shared_db();
            let session = crate::agents::testkit::seed_session(&db, Default::default());
            {
                let guard = db.lock().unwrap();
                for id in ["sync-a", "sync-b"] {
                    let mut row = run_row(id, "// stored");
                    row.session_id = session.id.clone();
                    guard.create_workflow(row).unwrap();
                }
            }
            // One mirror exists already, and holds an edit.
            assert!(mirror_script("sync-a", "// the user's edit").await);

            let written = sync_script_mirrors(&db, SyncOptions::default()).await;
            assert_eq!(written, vec!["sync-b".to_string()]);
            assert_eq!(
                read_mirror("sync-a").await.as_deref(),
                Some("// the user's edit")
            );
            assert_eq!(read_mirror("sync-b").await.as_deref(), Some("// stored"));
            assert!(home.join("workflows/sync-b.js").exists());

            // Idempotent: a second pass writes nothing.
            assert!(sync_script_mirrors(&db, SyncOptions::default())
                .await
                .is_empty());
        });
    }

    /// Best effort by contract: a read-only home must not stop a run starting.
    #[test]
    fn mirroring_into_an_unwritable_home_is_false_not_an_error() {
        with_home(|home| async move {
            // A FILE where the workflows directory should be: mkdir fails, write
            // fails, and the caller carries on.
            std::fs::write(home.join("workflows"), "not a directory").unwrap();
            assert!(!mirror_script("nope", "// script").await);
            assert_eq!(read_mirror("nope").await, None);
        });
    }
}
