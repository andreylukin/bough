//! Workspace file listing for the composer's `@` completion (port of
//! `src/server/fs.ts`).
//!
//! THE INVARIANT: **the candidate list is what git tracks, plus what git
//! would let you add** — `git ls-files` honours `.gitignore`, is one process
//! rather than a stat storm, and agrees with the Changes rail. A workspace
//! that is not a repository answers an empty list rather than an error: the
//! composer degrades to no suggestions, which is not worth a modal. Ranking
//! is NOT here — the client ranks; this returns paths.
//!
//! TRACKED FIRST, in two passes: `--cached --others` in one call interleaves
//! alphabetically, so one large untracked directory would reach the cap
//! before the source files do. A committed file is the file you mean.

use std::path::{Path, PathBuf};

use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::types::AppCtx;

use crate::http::{handler, json as json_res, Handler, Params};

/// The most paths one listing returns. Generous — the client fuzzy-filters
/// the whole list, so a cap that bites makes completion silently incomplete.
pub const MAX_FILES: usize = 20_000;

/// The most entries one directory listing returns.
pub const MAX_ENTRIES: usize = 2_000;

/// One git invocation, as trimmed lines. Empty on ANY failure — a keystroke
/// is not worth an error.
async fn git(dir: &str, args: &[&str]) -> Vec<String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await;
    match out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Candidates for `@`, repo-relative, tracked first. Empty when `dir` is not
/// a repo (or does not exist — the Command fails, which is the same answer).
pub async fn list_workspace_files(dir: &str) -> Vec<String> {
    if !Path::new(dir).is_dir() {
        return Vec::new();
    }
    let tracked = git(dir, &["ls-files", "--cached"]).await;
    let mut out: Vec<String> = tracked.into_iter().take(MAX_FILES).collect();
    if out.len() >= MAX_FILES {
        return out;
    }
    let seen: std::collections::HashSet<String> = out.iter().cloned().collect();
    for p in git(dir, &["ls-files", "--others", "--exclude-standard"]).await {
        if seen.contains(&p) {
            continue;
        }
        out.push(p);
        if out.len() >= MAX_FILES {
            break;
        }
    }
    out
}

/// `~` and `~/x` against the real home; everything else is left alone.
pub fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// One directory's entries, names only, directories suffixed with `/`,
/// dotfiles included, sorted, capped. Unreadable or missing → empty: a
/// half-typed path is not a mistake, it is the middle of typing.
pub fn list_dir_entries(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(if is_dir { format!("{name}/") } else { name });
        if out.len() >= MAX_ENTRIES {
            break;
        }
    }
    out.sort();
    out
}

fn query_param(req: &axum::extract::Request, name: &str) -> Option<String> {
    let query = req.uri().query()?;
    for kv in query.split('&') {
        if let Some(v) = kv.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Query-string values carry paths (`/`, `~`, spaces) — decode `%xx` and `+`.
fn percent_decode(v: &str) -> String {
    let mut out = Vec::with_capacity(v.len());
    let bytes = v.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let decoded =
                    v.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok());
                if let Some(b) = decoded {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn session_workspace(ctx: &AppCtx, params: &Params) -> Result<Option<String>, BoughError> {
    let id = params.get("id").map(String::as_str).unwrap_or("");
    let db = ctx.db.lock().unwrap();
    if db.get_session(id)?.is_none() {
        return Err(BoughError::not_found(format!("session {id} not found")));
    }
    Ok(db.get_session_runtime(id)?.workspace)
}

/// `GET /sessions/:id/files` — the `@` candidates for that session's
/// workspace. A session with no workspace has no files to offer.
pub fn list_files() -> Handler {
    handler(|_req, ctx, params| async move {
        let dir = session_workspace(&ctx, &params)?;
        let files = match dir {
            Some(dir) => list_workspace_files(&dir).await,
            None => Vec::new(),
        };
        Ok(json_res(&json!({ "files": files }), 200))
    })
}

/// `GET /files?workspace=<dir>` — the same listing for a directory with no
/// session yet (the new-conversation screen).
pub fn list_files_for_workspace() -> Handler {
    handler(|req, _ctx, _params| async move {
        let dir = query_param(&req, "workspace").unwrap_or_default();
        if dir.is_empty() {
            return Err(BoughError::bad_request("workspace is required"));
        }
        Ok(json_res(&json!({ "files": list_workspace_files(&dir).await }), 200))
    })
}

/// `GET /fs/entries?dir=<path>[&base=<workspace>]` — one directory, for `@`
/// paths that leave the workspace. One level deep on purpose.
pub fn list_dir_entries_h() -> Handler {
    handler(|req, _ctx, _params| async move {
        let raw = query_param(&req, "dir").unwrap_or_default();
        if raw.is_empty() {
            return Err(BoughError::bad_request("dir is required"));
        }
        let base = query_param(&req, "base").unwrap_or_default();
        let expanded = expand_tilde(&raw);
        let dir = if expanded.is_absolute() {
            expanded
        } else if !base.is_empty() {
            PathBuf::from(base).join(expanded)
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(expanded)
        };
        Ok(json_res(&json!({ "entries": list_dir_entries(&dir) }), 200))
    })
}

/// `GET /fs/branch?dir=<path>` — the branch that checkout is on; `""` for a
/// detached HEAD or a directory that is not a repository.
pub fn branch() -> Handler {
    handler(|req, _ctx, _params| async move {
        let dir = query_param(&req, "dir").unwrap_or_default();
        if dir.is_empty() {
            return Err(BoughError::bad_request("dir is required"));
        }
        let name = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .into_iter()
            .next()
            .unwrap_or_default();
        let branch = if name.is_empty() || name == "HEAD" { String::new() } else { name };
        Ok(json_res(&json!({ "branch": branch }), 200))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::schema::parts::{Session, SessionKind};
    use serde_json::json as j;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_session(fx: &testutil::Fixture, workspace: Option<&str>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: id.clone(),
                title: "t".into(),
                kind: SessionKind::Root,
                created_at: (fx.ctx.now)(),
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: workspace.map(str::to_string),
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
        id
    }

    #[tokio::test]
    async fn files_requires_the_workspace_param_and_a_non_repo_answers_empty() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/files")).await;
        assert_eq!(res.status(), 400);
        assert_eq!(testutil::body_json(res).await, j!({"error": "workspace is required"}));

        let dir = temp_dir();
        let res = call
            .call(testutil::get(&format!("/files?workspace={}", dir.display())))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"files": []}));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn session_files_404_unknown_and_no_workspace_answers_empty() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/sessions/nope/files")).await;
        assert_eq!(res.status(), 404);

        let id = seed_session(&fx, None);
        let res = call.call(testutil::get(&format!("/sessions/{id}/files"))).await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"files": []}));
    }

    #[tokio::test]
    async fn dir_entries_lists_one_level_sorted_with_directories_suffixed() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/fs/entries")).await;
        assert_eq!(res.status(), 400, "missing dir is a 400");

        let dir = temp_dir();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        std::fs::write(dir.join(".hidden"), "h").unwrap();
        let res = call
            .call(testutil::get(&format!("/fs/entries?dir={}", dir.display())))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            testutil::body_json(res).await,
            j!({"entries": [".hidden", "b.txt", "sub/"]}),
            "dotfiles included, sorted, dirs suffixed"
        );
        // An unreadable/missing directory is empty, not an error.
        let res = call
            .call(testutil::get(&format!("/fs/entries?dir={}/nope", dir.display())))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"entries": []}));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_relative_dir_resolves_against_base() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let dir = temp_dir();
        std::fs::create_dir(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner").join("x.txt"), "x").unwrap();
        let res = call
            .call(testutil::get(&format!("/fs/entries?dir=inner&base={}", dir.display())))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"entries": ["x.txt"]}));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn branch_requires_dir_and_answers_empty_for_a_non_repo() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/fs/branch")).await;
        assert_eq!(res.status(), 400);
        assert_eq!(testutil::body_json(res).await, j!({"error": "dir is required"}));

        let dir = temp_dir();
        let res = call
            .call(testutil::get(&format!("/fs/branch?dir={}", dir.display())))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await, j!({"branch": ""}));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_real_repo_lists_tracked_before_untracked_and_names_its_branch() {
        let dir = temp_dir();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-b", "trunk"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("tracked.txt"), "t").unwrap();
        run(&["add", "tracked.txt"]);
        run(&["commit", "-m", "seed", "--no-gpg-sign"]);
        std::fs::write(dir.join("aaa-untracked.txt"), "u").unwrap();

        let files = list_workspace_files(&dir.to_string_lossy()).await;
        assert_eq!(
            files,
            vec!["tracked.txt".to_string(), "aaa-untracked.txt".to_string()],
            "tracked first even though the untracked name sorts earlier"
        );

        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::get(&format!("/fs/branch?dir={}", dir.display())))
            .await;
        assert_eq!(testutil::body_json(res).await, j!({"branch": "trunk"}));
        std::fs::remove_dir_all(&dir).ok();
    }
}
