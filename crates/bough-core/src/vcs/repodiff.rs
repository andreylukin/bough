//! The Changes rail's git layer (port of `src/vcs/repodiff.ts`): what a
//! session changed in the user's checkout.
//!
//! THE INVARIANT THIS HOLDS: **the working tree IS the tip, so the only thing
//! worth recording is where the session started.** There is no snapshot
//! substrate under this and nothing to materialize. The agent's programs edit
//! the real checkout, so a session's change set is exactly `git diff <base>`
//! plus whatever is untracked, and delivery is the reviewer's own `git commit`.
//!
//! Three consequences, each load-bearing:
//!
//!   - **A base is a real sha, always.** A repo with no commits records the git
//!     empty-tree object ([`EMPTY_TREE`]) rather than a sentinel, because
//!     `git diff` and `git cat-file` both accept it — so the diff path and the
//!     revert path have exactly one shape instead of a special case each.
//!   - **Not-a-repo is an answer, not an error.** A workspace outside git
//!     degrades to an unavailable [`ChangeSet`] carrying the reason. Nothing
//!     here fails for it, and the agent keeps working there — it simply
//!     produces no reviewable change set.
//!   - **Revert is the only mutation.** It restores tracked paths from the base
//!     sha and deletes the ones the session created, per path. This module does
//!     not decide WHICH paths are the session's — `server/changes.rs`
//!     intersects the request with the live change set before calling.
//!
//! Server-free: nothing here references `bough-server`, so the whole module is
//! exercised against a real temp repo with no socket and no ctx.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::types::SharedDb;

/// The git empty-tree object id — the base recorded for a repository with no
/// commits yet. git resolves it without it existing in the object database, so
/// `git diff <EMPTY_TREE>` reports the whole index as additions and
/// `git cat-file -e <EMPTY_TREE>:<path>` correctly says "not in the base".
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

// ---- the structured diff ----------------------------------------------------
//
// Wire shapes: `server/changes.rs` serializes them verbatim — field names are
// API.

/// Coarse git status. A rename surfaces as delete + add — git's default
/// without `-M`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
}

/// One `@@ … @@` block: the header verbatim plus the body lines with their
/// leading ` `/`+`/`-` markers intact, so a client colours them without
/// re-parsing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<String>,
}

/// One changed file. Binary or unreadable content yields no hunks, not a
/// failure.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    /// Repo-relative, forward slashes — the same string `revert` takes back.
    pub path: String,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
    /// The file is not text, so there are no hunks and none are coming.
    ///
    /// A separate fact from "no hunks": an empty file and a 200-byte blob both
    /// diff to nothing, and only one of them has content the reviewer cannot
    /// be shown. `Some(true)` or absent — never `false` on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<bool>,
}

/// A session's review payload.
///
/// `available: false` is a first-class answer with a stated `reason`, not an
/// error and not an empty diff. The two are different facts — "this workspace
/// is not a repository" versus "you changed nothing" — and a rail that
/// rendered both as an empty list would be lying about one of them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSet {
    pub available: bool,
    /// Present exactly when `available` is false. One plain sentence for the
    /// human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The sha the diff is measured from, or null when there is none. Always
    /// on the wire, null included.
    pub base: Option<String>,
    pub files: Vec<FileDiff>,
}

/// An unavailable change set — the shape every "no diff here" path returns.
fn unavailable(reason: String, base: Option<String>) -> ChangeSet {
    ChangeSet {
        available: false,
        reason: Some(reason),
        base,
        files: Vec::new(),
    }
}

// ---- git --------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct GitResult {
    pub ok: bool,
    pub out: String,
    pub err: String,
}

/// Run git in `dir`. A missing git binary comes back as `ok: false` rather
/// than failing: every caller here already has a "git could not answer" path,
/// and the Changes rail must degrade to "no change set" instead of 500-ing a
/// session that is otherwise working fine.
pub async fn git(dir: &str, args: &[&str]) -> GitResult {
    let spawned = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;
    match spawned {
        Ok(output) => GitResult {
            ok: output.status.success(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(e) => GitResult {
            ok: false,
            out: String::new(),
            err: e.to_string(),
        },
    }
}

/// Whether `dir` is inside a git work tree.
pub async fn is_repo(dir: &str) -> bool {
    let r = git(dir, &["rev-parse", "--is-inside-work-tree"]).await;
    r.ok && r.out.trim() == "true"
}

/// The checkout's current HEAD sha, or None (no commits yet, or not a repo).
pub async fn head_sha(dir: &str) -> Option<String> {
    let r = git(dir, &["rev-parse", "--verify", "-q", "HEAD"]).await;
    let sha = r.out.trim();
    if r.ok && !sha.is_empty() {
        Some(sha.to_string())
    } else {
        None
    }
}

/// The sha to record as a session's `base` for `dir`, or None when `dir` is
/// not a repository (no repo ⇒ no base ⇒ no change set, and that is fine).
///
/// A repository with no commits answers [`EMPTY_TREE`], not None: the session
/// started from nothing, which is a real starting point and diffs correctly.
pub async fn base_for(dir: &str) -> Option<String> {
    if !is_repo(dir).await {
        return None;
    }
    Some(
        head_sha(dir)
            .await
            .unwrap_or_else(|| EMPTY_TREE.to_string()),
    )
}

/// Record the sha a session starts from, best-effort.
///
/// Best-effort is the whole point: a broken git install, a workspace that
/// vanished between validation and here, or a repository someone is
/// mid-`rebase` in must cost the user their Changes rail, never their session.
/// A session with no base is a session the rail reports as unreviewable.
///
/// Returns what was stored, or None if nothing was.
pub async fn record_base(db: &SharedDb, session_id: &str, dir: &str) -> Option<String> {
    let base = base_for(dir).await?;
    let stored = {
        let db = match db.lock() {
            Ok(db) => db,
            Err(_) => return None,
        };
        db.set_session_base(session_id, &base).is_ok()
    };
    if stored {
        Some(base)
    } else {
        None
    }
}

// ---- parsing ----------------------------------------------------------------

/// Parse a `git diff` into `Vec<FileDiff>`. Pure and dependency-free — the
/// heaviest unit-tested surface in this module.
///
/// Names come from the b-side, which is already repo-relative.
pub fn parse_git_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;

    fn flush_hunk(cur: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
        if let (Some(cur), Some(h)) = (cur.as_mut(), hunk.take()) {
            cur.hunks.push(h);
        }
        *hunk = None;
    }
    fn flush_file(files: &mut Vec<FileDiff>, cur: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
        flush_hunk(cur, hunk);
        if let Some(c) = cur.take() {
            files.push(c);
        }
    }

    for line in text.split('\n') {
        if line.starts_with("diff --git ") {
            flush_file(&mut files, &mut cur, &mut hunk);
            // "diff --git a/foo b/foo" — the b-side is the canonical path.
            let last = line.split(' ').next_back().unwrap_or("");
            cur = Some(FileDiff {
                path: last.strip_prefix("b/").unwrap_or(last).to_string(),
                status: FileStatus::Modified,
                hunks: Vec::new(),
                binary: None,
            });
            continue;
        }
        let Some(current) = cur.as_mut() else {
            continue;
        };

        if line.starts_with("new file mode") {
            current.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            current.status = FileStatus::Deleted;
        } else if line.starts_with("rename from") || line.starts_with("rename to") {
            // A pure rename carries no content change and no hunks; leave it
            // modified.
        } else if line.starts_with("--- ") || line.starts_with("+++ ") {
            // File headers — the b-side name is already captured.
        } else if line.starts_with("@@") {
            flush_hunk(&mut cur, &mut hunk);
            hunk = Some(Hunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
        } else if let Some(h) = hunk.as_mut() {
            // Context/added/removed lines and git's no-newline marker are the
            // hunk body (src/vcs/repodiff.ts:234-238 keeps these as two arms
            // with the same body; the conditions are disjoint, so one arm with
            // the union reads the same lines).
            if line.starts_with(' ')
                || line.starts_with('+')
                || line.starts_with('-')
                || line == "\\ No newline at end of file"
            {
                h.lines.push(line.to_string());
            }
        }
    }
    flush_file(&mut files, &mut cur, &mut hunk);
    files
}

// ---- the change set ---------------------------------------------------------

/// The largest untracked file whose body is worth inlining as an added hunk.
///
/// A change set is a REVIEW, and nobody reviews a 4 MiB blob by scrolling it.
/// The entry still appears — you must be able to see that the file is new — it
/// just carries no body.
const MAX_ADDED_BYTES: u64 = 512 * 1024;

/// How many files a wholly-untracked directory may hold before the rail shows
/// the DIRECTORY instead of its contents.
///
/// A new `src/feature/` with four files in it IS the work under review, and
/// collapsing it would hide exactly what the user opened the rail to see. A
/// `bench/` with 50,899 files in it is not under review by anybody. So:
/// itemize until a directory is plainly not hand-written, then say its name
/// the way `git status` would.
const MAX_DIR_FILES: usize = 25;

async fn ls_files(dir: &str, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["ls-files", "--others", "--exclude-standard"];
    args.extend_from_slice(extra);
    let r = git(dir, &args).await;
    if !r.ok {
        return Vec::new();
    }
    r.out
        .split('\n')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Paths git neither tracks nor ignores, relative to the repo root — with a
/// runaway directory collapsed to one entry.
///
/// Two passes, because git will give either shape but not the choice between
/// them: `--directory` names the wholly-untracked directories, the bare
/// listing names every file. A directory over the threshold contributes its
/// own name; everything else contributes its files.
async fn untracked(dir: &str) -> Vec<String> {
    let files = ls_files(dir, &[]).await;
    let collapsed = ls_files(dir, &["--directory", "--no-empty-directory"]).await;
    let dirs: Vec<&String> = collapsed.iter().filter(|p| p.ends_with('/')).collect();
    if dirs.is_empty() {
        return files;
    }

    let bulky: Vec<&String> = dirs
        .into_iter()
        .filter(|d| files.iter().filter(|f| f.starts_with(d.as_str())).count() > MAX_DIR_FILES)
        .collect();
    let mut out: Vec<String> = bulky.iter().map(|d| d.to_string()).collect();
    for f in &files {
        if !bulky.iter().any(|d| f.starts_with(d.as_str())) {
            out.push(f.clone());
        }
    }
    out
}

/// An untracked file as an all-added [`FileDiff`]. Binary, huge or unreadable
/// ⇒ no hunks.
async fn added_file(dir: &str, path: &str) -> FileDiff {
    // `--directory` yields collapsed directories with a trailing slash. There
    // is no body to read and no point stat-ing it: it is one entry meaning
    // "all of this is new", which is the same thing `git status` shows.
    if path.ends_with('/') {
        return FileDiff {
            path: path.to_string(),
            status: FileStatus::Added,
            hunks: Vec::new(),
            binary: None,
        };
    }

    let mut lines: Vec<String> = Vec::new();
    let mut binary = false;
    let full = Path::new(dir).join(path);
    if let Ok(info) = tokio::fs::metadata(&full).await {
        if info.len() <= MAX_ADDED_BYTES {
            // READ THE BYTES FIRST. Decoding to text does not fail on binary —
            // it substitutes U+FFFD and returns happily, so a 200-byte blob
            // was once diffed as two "+" lines of replacement characters and
            // painted into the review pane. git refuses to do this for a
            // reason ("Binary files a/x and b/x differ"), and raw bytes on a
            // terminal are not merely unreadable: an escape sequence among
            // them is executed.
            //
            // NUL in the first 8000 bytes is git's own heuristic, and it is
            // the cheap half of a read we are doing anyway.
            if let Ok(buf) = tokio::fs::read(&full).await {
                binary = buf[..buf.len().min(8000)].contains(&0);
                if !binary {
                    let text = String::from_utf8_lossy(&buf);
                    let mut body: Vec<&str> = text.split('\n').collect();
                    // A trailing "" from a final newline is not a line; a file
                    // without a final newline keeps its last one.
                    if body.last() == Some(&"") {
                        body.pop();
                    }
                    lines = body.into_iter().map(|l| format!("+{l}")).collect();
                }
            }
        }
    }
    FileDiff {
        path: path.to_string(),
        status: FileStatus::Added,
        hunks: if lines.is_empty() {
            Vec::new()
        } else {
            vec![Hunk {
                header: format!("@@ -0,0 +1,{} @@", lines.len()),
                lines,
            }]
        },
        binary: if binary { Some(true) } else { None },
    }
}

/// What changed in `dir` since `base`: tracked edits from `git diff` plus
/// untracked files as additions.
///
/// Untracked files are appended rather than obtained with `--no-index` passes
/// because `git diff <base>` already covers everything git knows about, staged
/// or not — so the two lists are disjoint by construction and nothing is
/// counted twice.
pub async fn change_set(dir: &str, base: Option<&str>) -> ChangeSet {
    if !is_repo(dir).await {
        // THE FACT, and only the fact. The Changes tab renders this reason
        // with its own hint directly underneath; the client's hint owns the
        // consequences, this owns what is true.
        return unavailable(
            format!("{dir} is not a git repository, so there is nothing to diff."),
            None,
        );
    }
    let Some(base) = base else {
        return unavailable(
            format!(
                "no starting commit was recorded for this session in {dir}, so there is \
                 nothing to diff against. Sessions record one when they are created; a \
                 session that predates that — or whose workspace was not a repository then \
                 — has no change set."
            ),
            None,
        );
    };

    let r = git(dir, &["diff", "--no-color", "--no-ext-diff", base]).await;
    if !r.ok {
        let why = r.err.trim();
        return unavailable(
            format!(
                "git diff {base} failed in {dir}: {}. The commit the session started from \
                 may have been dropped by a rebase or a prune, which leaves nothing to \
                 measure this session's work against.",
                if why.is_empty() {
                    "git reported no reason"
                } else {
                    why
                }
            ),
            Some(base.to_string()),
        );
    }

    let mut files = parse_git_diff(&r.out);
    for path in untracked(dir).await {
        files.push(added_file(dir, &path).await);
    }
    ChangeSet {
        available: true,
        reason: None,
        base: Some(base.to_string()),
        files,
    }
}

// ---- revert -----------------------------------------------------------------

/// One path that could not be reverted, with git's own reason.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RevertFailure {
    pub path: String,
    pub error: String,
}

/// What a revert actually did. `failed` carries git's own reason, per path.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RevertResult {
    pub reverted: Vec<String>,
    pub failed: Vec<RevertFailure>,
}

/// Undo the session's work on `paths`: tracked files are restored to their
/// content at `base`, files the session created are deleted.
///
/// PER PATH, and per path in both directions — one path that cannot be
/// restored fails alone and the rest still revert. A reviewer un-picking one
/// file out of twelve must not lose the other eleven to a single permission
/// error.
///
/// The caller is responsible for passing only paths the session actually
/// changed (`server/changes.rs` intersects with the live change set first).
/// `git checkout <base> -- <path>` would happily rewrite a file this session
/// never touched.
pub async fn revert_paths(dir: &str, base: &str, paths: &[String]) -> RevertResult {
    let mut result = RevertResult::default();
    for path in paths {
        // Present in the base commit ⇒ restore that content. `git checkout
        // <sha> -- <path>` also stages it, which is what a reviewer means by
        // "put it back".
        let known = git(dir, &["cat-file", "-e", &format!("{base}:{path}")]).await;
        if known.ok {
            let r = git(dir, &["checkout", base, "--", path]).await;
            if r.ok {
                result.reverted.push(path.clone());
            } else {
                let why = r.err.trim();
                result.failed.push(RevertFailure {
                    path: path.clone(),
                    error: if why.is_empty() {
                        "git checkout failed".to_string()
                    } else {
                        why.to_string()
                    },
                });
            }
            continue;
        }
        // Absent from the base commit ⇒ the session created it. Delete it,
        // then prune the directories that existed only to hold it.
        match tokio::fs::remove_file(Path::new(dir).join(path)).await {
            Ok(()) => {
                result.reverted.push(path.clone());
                prune_empty_parents(dir, path);
            }
            // Already gone is a success: the reviewer asked for it not to be
            // there.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                result.reverted.push(path.clone());
            }
            Err(e) => {
                result.failed.push(RevertFailure {
                    path: path.clone(),
                    error: e.to_string(),
                });
            }
        }
    }
    result
}

/// Remove the now-empty directories a deleted file left behind, stopping at
/// the first one that still holds something. Synchronous and swallowing: this
/// is tidiness, and a failure to tidy must never turn a successful revert into
/// a reported failure.
fn prune_empty_parents(dir: &str, path: &str) {
    let mut parent: PathBuf = match Path::new(path).parent() {
        Some(p) => p.to_path_buf(),
        None => return,
    };
    loop {
        let rel = parent.to_string_lossy();
        if rel.is_empty() || rel == "." || rel == "/" {
            return;
        }
        // remove_dir throws once non-empty — that is the stop.
        if std::fs::remove_dir(Path::new(dir).join(&parent)).is_err() {
            return;
        }
        parent = match parent.parent() {
            Some(p) => p.to_path_buf(),
            None => return,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the parser (pure) --------------------------------------------------

    #[test]
    fn parse_git_diff_statuses_hunk_bodies_and_the_no_newline_marker() {
        let files = parse_git_diff(
            &[
                "diff --git a/keep.ts b/keep.ts",
                "index 111..222 100644",
                "--- a/keep.ts",
                "+++ b/keep.ts",
                "@@ -1,2 +1,2 @@",
                " const a = 1;",
                "-const b = 2;",
                "+const b = 3;",
                "\\ No newline at end of file",
                "diff --git a/gone.ts b/gone.ts",
                "deleted file mode 100644",
                "--- a/gone.ts",
                "+++ /dev/null",
                "@@ -1 +0,0 @@",
                "-was here",
                "diff --git a/fresh.ts b/fresh.ts",
                "new file mode 100644",
                "--- /dev/null",
                "+++ b/fresh.ts",
                "@@ -0,0 +1 @@",
                "+brand new",
                "",
            ]
            .join("\n"),
        );

        assert_eq!(
            files
                .iter()
                .map(|f| (f.path.as_str(), f.status))
                .collect::<Vec<_>>(),
            vec![
                ("keep.ts", FileStatus::Modified),
                ("gone.ts", FileStatus::Deleted),
                ("fresh.ts", FileStatus::Added),
            ]
        );
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(
            files[0].hunks[0].lines,
            vec![
                " const a = 1;",
                "-const b = 2;",
                "+const b = 3;",
                "\\ No newline at end of file",
            ]
        );
        assert_eq!(files[2].hunks[0].header, "@@ -0,0 +1 @@");
    }

    #[test]
    fn parse_git_diff_empty_input_is_an_empty_change_set_not_a_panic() {
        assert_eq!(parse_git_diff(""), Vec::new());
    }

    #[test]
    fn wire_shapes_serialize_verbatim() {
        // Field names are API: `server/changes.rs` serializes these directly.
        let set = ChangeSet {
            available: true,
            reason: None,
            base: Some("abc".into()),
            files: vec![FileDiff {
                path: "a.ts".into(),
                status: FileStatus::Added,
                hunks: vec![Hunk {
                    header: "@@ -0,0 +1 @@".into(),
                    lines: vec!["+x".into()],
                }],
                binary: None,
            }],
        };
        let v = serde_json::to_value(&set).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "available": true,
                "base": "abc",
                "files": [{
                    "path": "a.ts",
                    "status": "added",
                    "hunks": [{"header": "@@ -0,0 +1 @@", "lines": ["+x"]}],
                }],
            })
        );
        // Unavailable: `reason` present, `base` explicit null, `binary` only
        // when true.
        let un = unavailable("why".into(), None);
        let v = serde_json::to_value(&un).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"available": false, "reason": "why", "base": null, "files": []})
        );
    }

    // ---- against a real repository ------------------------------------------
    //
    // These run against a REAL git repository in a temp dir rather than a
    // fake, because every bug this file has caught was a disagreement between
    // what bough reported and what `git status` reports, and a fake git agrees
    // with itself.

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A throwaway repo with one commit. The caller removes it.
    fn repo() -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("bough-repodiff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]);
        run(&dir, &["config", "user.email", "t@t"]);
        run(&dir, &["config", "user.name", "t"]);
        run(&dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("tracked.txt"), "one\n").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-qm", "init"]);
        let head = run(&dir, &["rev-parse", "HEAD"]);
        (dir, head)
    }

    #[tokio::test]
    async fn a_runaway_untracked_directory_collapses_to_one_entry() {
        if !git_available() {
            return;
        }
        let (dir, head) = repo();
        let d = dir.to_string_lossy();
        // The shape that broke the rail in bough's own checkout: one untracked
        // directory holding far more files than a reviewer will ever scroll.
        std::fs::create_dir_all(dir.join("bench/state")).unwrap();
        for i in 0..60 {
            std::fs::write(
                dir.join(format!("bench/state/r{i}.json")),
                format!("{{\"i\":{i}}}\n"),
            )
            .unwrap();
        }
        std::fs::write(dir.join("loose.txt"), "new\n").unwrap();
        // A small new directory is the agent's actual work and stays itemized.
        std::fs::create_dir_all(dir.join("feature")).unwrap();
        std::fs::write(dir.join("feature/a.ts"), "export const a = 1;\n").unwrap();
        std::fs::write(dir.join("feature/b.ts"), "export const b = 2;\n").unwrap();

        let set = change_set(&d, Some(&head)).await;
        assert!(set.available);
        let mut paths: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        // One entry for the whole directory — not 60 — plus the loose file.
        assert_eq!(
            paths,
            vec!["bench/", "feature/a.ts", "feature/b.ts", "loose.txt"]
        );
        // The collapsed directory carries no body: there is no single file to
        // show.
        assert!(set
            .files
            .iter()
            .find(|f| f.path == "bench/")
            .unwrap()
            .hunks
            .is_empty());
        // The loose file still gets its contents, because that IS reviewable.
        assert_eq!(
            set.files
                .iter()
                .find(|f| f.path == "loose.txt")
                .unwrap()
                .hunks[0]
                .lines,
            vec!["+new"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_tracked_edit_and_an_untracked_file_are_both_reported_and_not_twice() {
        if !git_available() {
            return;
        }
        let (dir, head) = repo();
        let d = dir.to_string_lossy();
        std::fs::write(dir.join("tracked.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("added.txt"), "fresh\n").unwrap();
        let set = change_set(&d, Some(&head)).await;
        assert!(set.available);
        let mut paths: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["added.txt", "tracked.txt"]);
        assert_eq!(
            set.files
                .iter()
                .find(|f| f.path == "tracked.txt")
                .unwrap()
                .status,
            FileStatus::Modified
        );
        assert_eq!(
            set.files
                .iter()
                .find(|f| f.path == "added.txt")
                .unwrap()
                .status,
            FileStatus::Added
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_untracked_binary_file_is_flagged_never_decoded_into_the_review_pane() {
        if !git_available() {
            return;
        }
        let (dir, head) = repo();
        let d = dir.to_string_lossy();
        // The bug this pins: decoding does NOT fail on binary — it substitutes
        // U+FFFD — so a 200-byte blob was diffed as "+" lines of replacement
        // characters and painted into the diff pane. Raw bytes on a terminal
        // are not merely unreadable: an escape sequence among them is executed.
        std::fs::write(
            dir.join("blob.bin"),
            [0x89u8, 0x50, 0, 0x1b, 0x5b, 0x32, 0x4a, 0xff],
        )
        .unwrap();
        std::fs::write(dir.join("text.txt"), "still text\n").unwrap();
        let set = change_set(&d, Some(&head)).await;
        let blob = set.files.iter().find(|f| f.path == "blob.bin").unwrap();
        assert_eq!(blob.status, FileStatus::Added);
        assert_eq!(blob.binary, Some(true));
        assert!(blob.hunks.is_empty());
        // A text file beside it is unaffected, and is NOT flagged.
        let text = set.files.iter().find(|f| f.path == "text.txt").unwrap();
        assert_eq!(text.binary, None);
        assert!(!text.hunks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_huge_untracked_file_is_listed_but_not_inlined() {
        if !git_available() {
            return;
        }
        let (dir, head) = repo();
        let d = dir.to_string_lossy();
        // Over MAX_ADDED_BYTES. It must still appear — you have to be able to
        // see that the file is new — but nobody reviews a megabyte by
        // scrolling it.
        std::fs::write(dir.join("big.txt"), "x\n".repeat(400_000)).unwrap();
        let set = change_set(&d, Some(&head)).await;
        assert!(set.available);
        let big = set.files.iter().find(|f| f.path == "big.txt");
        assert!(big.is_some(), "the file must still be listed");
        assert!(big.unwrap().hunks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_repo_answers_rather_than_failing() {
        if !git_available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("bough-norepo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let set = change_set(&dir.to_string_lossy(), None).await;
        // Unavailable is an ANSWER, and it says why.
        assert!(!set.available);
        assert!(set
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("not a git repository"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn base_for_answers_head_empty_tree_or_none() {
        if !git_available() {
            return;
        }
        let (dir, head) = repo();
        assert_eq!(base_for(&dir.to_string_lossy()).await, Some(head));
        let _ = std::fs::remove_dir_all(&dir);

        // A repo with no commits records the empty tree, not nothing.
        let bare = std::env::temp_dir().join(format!("bough-nocommit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&bare).unwrap();
        run(&bare, &["init", "-q"]);
        assert_eq!(
            base_for(&bare.to_string_lossy()).await,
            Some(EMPTY_TREE.to_string())
        );
        let _ = std::fs::remove_dir_all(&bare);

        // Not a repository records nothing at all.
        let plain = std::env::temp_dir().join(format!("bough-plain-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(base_for(&plain.to_string_lossy()).await, None);
        let _ = std::fs::remove_dir_all(&plain);
    }
}
