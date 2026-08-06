//! The Changes rail (port of `src/server/changes.ts`): a session's review
//! payload, and the one mutation over it.
//!
//! THE INVARIANT THIS HOLDS: **revert never touches a path the session did not
//! change.** That is enforced here rather than assumed of the caller. A revert
//! request is intersected with the change set the rail is showing right now,
//! and anything outside it is reported back as skipped instead of being
//! restored — because `git checkout <base> -- <path>` is perfectly happy to
//! rewrite a file this session never opened, and a client passing a stale or
//! hand-typed path would otherwise silently clobber the user's own
//! uncommitted work.
//!
//! Second: **a workspace that is not a repository degrades, it does not
//! fail.** No repo means no base, which means no change set — the rail says
//! exactly that, with the reason, and the session keeps working. The only 400
//! here is a revert asked of a session that has nothing to revert against,
//! and its message is that same reason.
//!
//! There is no apply. The agent edits the user's checkout in place, so the
//! work is already where an apply would have put it, and delivery is the
//! reviewer's own `git commit`.
//!
//! NO EVENT IS PUBLISHED on revert, deliberately. The event set is closed and
//! has no changes event; the rail is a fetch-on-demand surface, and the
//! response carries the whole outcome, so a client re-reads
//! `GET /sessions/:id/changes` and reconciles — the same rule as reconnect
//! (events are display transport, the database and the working tree are the
//! truth). The system NOTE below is transcript, not display.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::json;

use bough_core::errors::BoughError;
use bough_core::schema::events::{EventInput, EventType};
use bough_core::schema::parts::{Message, Part, Role};
use bough_core::schema::requests::RevertChangesBody;
use bough_core::types::AppCtx;
use bough_core::vcs::repodiff::{change_set, revert_paths, ChangeSet, RevertFailure};

use crate::http::{handler, json as json_res, parse_body, Handler};

/// The rail's payload: the git change set plus the checkout it was measured
/// in.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionChangeSet {
    #[serde(flatten)]
    pub set: ChangeSet,
    /// The session's checkout, or null when it never named one.
    pub workspace: Option<String>,
}

// ---- display noise -----------------------------------------------------------
//
// Build and cache artifacts clutter the review list and are never what a
// reviewer came to look at. Filtered from what the rail SHOWS — and
// therefore, since revert can only touch what was shown, from what a revert
// can delete. That direction is deliberate: leaving a stale `__pycache__`
// behind is a nuisance, deleting files the user was never shown is a
// surprise.

const NOISE_SEGMENTS: [&str; 4] = ["__pycache__", "node_modules", ".pytest_cache", ".mypy_cache"];
const NOISE_BASENAMES: [&str; 1] = [".DS_Store"];
const NOISE_SUFFIXES: [&str; 2] = [".pyc", ".pyo"];

fn is_noise(path: &str) -> bool {
    let segs: Vec<&str> = path.split('/').collect();
    let base = segs.last().copied().unwrap_or("");
    NOISE_SEGMENTS.iter().any(|s| segs.contains(s))
        || NOISE_BASENAMES.contains(&base)
        || NOISE_SUFFIXES.iter().any(|s| base.ends_with(s))
}

// ---- the change set ----------------------------------------------------------

fn require_session(ctx: &AppCtx, id: &str) -> Result<(), BoughError> {
    match ctx.db.lock().unwrap().get_session(id)? {
        Some(_) => Ok(()),
        None => Err(BoughError::not_found(format!(
            "no session {id} — changes are per session, so open one that exists \
             (GET /sessions lists them).",
        ))),
    }
}

/// A session's change set: `git diff <base>` plus untracked files, in the
/// session's own checkout.
///
/// A session with no workspace answers unavailable rather than falling back
/// to the server's own directory the way a turn does. The fallback is right
/// for RUNNING a program — something has to be the cwd — and wrong here:
/// attributing whatever is uncommitted in bough's own checkout to a session
/// that never named one would report a stranger's work as the agent's, and
/// offer to revert it.
pub async fn session_changes(ctx: &AppCtx, session_id: &str) -> Result<SessionChangeSet, BoughError> {
    // The lock is scoped: `change_set` shells out to git and must not hold
    // the one database mutex across that.
    let runtime = { ctx.db.lock().unwrap().get_session_runtime(session_id)? };
    let Some(workspace) = runtime.workspace else {
        return Ok(SessionChangeSet {
            set: ChangeSet {
                available: false,
                reason: Some(
                    "this session has no workspace, so there is no checkout to diff. \
                     Create a session with a `workspace` to get a Changes rail."
                        .to_string(),
                ),
                base: None,
                files: Vec::new(),
            },
            workspace: None,
        });
    };
    let mut set = change_set(&workspace, runtime.base.as_deref()).await;
    set.files.retain(|f| !is_noise(&f.path));
    Ok(SessionChangeSet { set, workspace: Some(workspace) })
}

// ---- revert ------------------------------------------------------------------

/// What a revert did, said in full: nothing here is inferred by the client.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct RevertOutcome {
    /// Paths restored from the base sha, or deleted because the session
    /// created them.
    pub reverted: Vec<String>,
    /// Requested paths that are not in the session's change set — left
    /// untouched.
    pub skipped: Vec<String>,
    /// Paths that are the session's but could not be reverted, with git's
    /// reason.
    pub failed: Vec<RevertFailure>,
}

/// A requested path as it appears in the change set, or None if it is not
/// one.
///
/// Only cosmetic normalization (`./x` → `x`, trailing slash) is done. An
/// absolute path or a `..` escape is not resolved into a match — it is simply
/// not found, and lands in `skipped`. Resolving them would be re-implementing
/// path confinement in the one place where being lenient means writing
/// outside the change set.
fn match_path(requested: &str, changed: &HashSet<String>) -> Option<String> {
    let trimmed = requested.trim();
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    let trimmed = trimmed.trim_end_matches('/');
    if changed.contains(trimmed) { Some(trimmed.to_string()) } else { None }
}

/// Revert the session's work on `paths` — or on everything the rail is
/// showing when `paths` is ABSENT.
///
/// **An explicit `paths: []` selects nothing and is refused**, and the
/// difference between "absent" and "explicitly empty" is the whole point.
/// Revert is the only destructive operation in the product and it is
/// unbounded — the change set of a session opened in a dirty checkout is
/// every uncommitted file in it, because `base` is the sha the session
/// started from and nothing distinguishes work the agent did from work that
/// was already there. So the one input a caller produces by ACCIDENT — a
/// selection loop that yielded no rows, a UI with nothing highlighted, a
/// `paths` variable that came back empty — must not be the input that means
/// "destroy all of it".
///
/// Revert-all is still reachable and still one call: omit `paths` entirely.
/// That is a request nobody sends by mistake.
///
/// A 400 when the session has no change set to revert against carries the
/// reason the rail displays, so the human reads the same sentence in both
/// places.
pub async fn revert_changes(
    ctx: &AppCtx,
    session_id: &str,
    paths: Option<Vec<String>>,
) -> Result<RevertOutcome, BoughError> {
    if paths.as_ref().is_some_and(|p| p.is_empty()) {
        return Err(BoughError::bad_request(
            "revert was given an empty `paths` selection, so it reverted nothing. An empty \
             list is not a wildcard — it is almost always a client that selected no rows, \
             and revert deletes files. To revert one or more paths, name them; to revert \
             the WHOLE change set, omit `paths` from the body entirely.",
        ));
    }

    let set = session_changes(ctx, session_id).await?;
    let (Some(base), Some(workspace)) = (set.set.base.as_deref(), set.workspace.as_deref())
    else {
        return Err(nothing_to_revert(&set));
    };
    if !set.set.available {
        return Err(nothing_to_revert(&set));
    }

    let in_order: Vec<String> = set.set.files.iter().map(|f| f.path.clone()).collect();
    let changed: HashSet<String> = in_order.iter().cloned().collect();
    let mut targets: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    // The enforcement the old tree only claimed: the selection is intersected
    // with the change set the rail is showing — never a wildcard git
    // resolves, so a path that scrolled off the rail is unreachable.
    if let Some(paths) = &paths {
        for requested in paths {
            match match_path(requested, &changed) {
                Some(m) => targets.push(m),
                None => skipped.push(requested.clone()),
            }
        }
    }
    let selection = if paths.is_some() { targets } else { in_order };

    let result = revert_paths(workspace, base, &selection).await;
    Ok(RevertOutcome { reverted: result.reverted, skipped, failed: result.failed })
}

fn nothing_to_revert(set: &SessionChangeSet) -> BoughError {
    BoughError::bad_request(format!(
        "nothing to revert: {}",
        set.set.reason.as_deref().unwrap_or("no change set")
    ))
}

// ---- the no-wake note --------------------------------------------------------

/// Persist a system note without waking anything — the `wake: "never"` subset
/// of `agents/notes` (`postSystemNote`), inlined here until row 2.3 lands the
/// full wake rule; this callsite passes `never` regardless, so the behavior
/// is final. Never fails its caller: the revert already happened, and a note
/// that could not be written must not turn a successful revert into an error.
fn post_no_wake_note(ctx: &AppCtx, session_id: &str, text: String) {
    let db = ctx.db.lock().unwrap();
    let Ok(Some(_)) = db.get_session(session_id) else { return };
    let stored = db.create_message(Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        role: Role::System,
        parts: vec![Part::Text { text }],
        // Complete when it lands: `pending` is the supervisor's streaming
        // flag, and a note left pending is a session the UI shows as busy
        // forever.
        pending: false,
        created_at: (ctx.now)(),
    });
    let Ok(stored) = stored else { return };
    // Index quietly — the search index is never load-bearing.
    let _ = db.index_message(&stored);
    drop(db);
    ctx.bus.publish(EventInput {
        r#type: EventType::MessageStarted,
        session_id: Some(session_id.to_string()),
        data: serde_json::to_value(&stored).unwrap_or(serde_json::Value::Null),
    });
}

// ---- handlers ----------------------------------------------------------------

/// `GET /sessions/:id/changes` — the rail's payload.
///
/// Always 200, even with no change set: "not a repository" and "you changed
/// nothing" are both ordinary answers about a healthy session, and the
/// difference between them is `available` + `reason` rather than a status
/// code.
pub fn get_changes() -> Handler {
    handler(|_req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        Ok(json_res(&session_changes(&ctx, &id).await?, 200))
    })
}

/// `POST /sessions/:id/changes/revert` — restore tracked paths from the base
/// sha and delete the ones the session created. No body (or a body without
/// `paths`) reverts everything the rail is showing; an explicit `{paths: []}`
/// is refused rather than treated as a wildcard (see [`revert_changes`]).
///
/// REST exists so a revert does not cost a turn: this is the human's verb,
/// and asking the agent to undo its own work would be an LLM round-trip to
/// run `git checkout`.
pub fn revert_changes_h() -> Handler {
    handler(|req, ctx, params| async move {
        let id = params.get("id").cloned().unwrap_or_default();
        require_session(&ctx, &id)?;
        let body: RevertChangesBody = parse_body(req, Some(json!({}))).await?;
        let outcome = revert_changes(&ctx, &id, body.paths).await?;
        // AND TELL THE MODEL. Reverting wrote nothing anywhere the agent can
        // read, so the next turn replayed its own successful patch, its green
        // test run and its summary — and then found the old code back. The
        // only reading available to it was that its write had silently
        // failed, so it "fixed" the regression by applying the edit again:
        // the one gesture that means "no, not that" was the one the model was
        // guaranteed to undo. Observed twice, verbatim ("No—the cart.js fix
        // didn't apply. Let me check the file and re-apply it").
        //
        // A system note is exactly the shape the schema already has for this
        // — a harness-injected fact that replays as user-side text. It does
        // NOT wake a turn: reverting is the human's verb and must stay free
        // (the reason this is REST at all).
        if !outcome.reverted.is_empty() {
            post_no_wake_note(
                &ctx,
                &id,
                format!(
                    "The human reverted {} to the state before this session. Those edits \
                     are gone from the working tree on purpose — this is not a failed \
                     write and not a regression to repair. Do not re-apply them unless \
                     you are asked to.",
                    outcome.reverted.join(", ")
                ),
            );
        }
        Ok(json_res(&outcome, 200))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::vcs::repodiff::{git, EMPTY_TREE};
    use serde_json::json as j;
    use std::path::{Path, PathBuf};

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn run(dir: &str, args: &[&str]) {
        let r = git(dir, args).await;
        assert!(r.ok, "git {} failed: {}", args.join(" "), r.err);
    }

    /// A repo with one commit. Identity and signing are forced per command so
    /// the test passes on a machine with no git config and on one that signs
    /// every commit.
    async fn temp_repo(commit: bool) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-changes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().into_owned();
        run(&d, &["init", "-q", "."]).await;
        if !commit {
            return dir;
        }
        std::fs::write(dir.join("README.md"), "base\n").unwrap();
        std::fs::write(dir.join("vendor.txt"), "untouched\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        run(&d, &["add", "-A"]).await;
        run(
            &d,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "init",
            ],
        )
        .await;
        dir
    }

    /// Create the session over HTTP — the path that must record the base.
    async fn start_session(fx: &testutil::Fixture, workspace: &str) -> serde_json::Value {
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                "/sessions",
                Some(j!({"title": "s", "workspace": workspace})),
            ))
            .await;
        assert_eq!(res.status(), 201);
        testutil::body_json(res).await
    }

    async fn changes_of(fx: &testutil::Fixture, id: &str) -> serde_json::Value {
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get(&format!("/sessions/{id}/changes"))).await;
        assert_eq!(res.status(), 200);
        testutil::body_json(res).await
    }

    async fn revert(
        fx: &testutil::Fixture,
        id: &str,
        body: Option<serde_json::Value>,
    ) -> (u16, serde_json::Value) {
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res =
            call.call(testutil::req("POST", &format!("/sessions/{id}/changes/revert"), body)).await;
        let status = res.status().as_u16();
        (status, testutil::body_json(res).await)
    }

    fn paths_of(set: &serde_json::Value) -> Vec<String> {
        let mut paths: Vec<String> = set["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap().to_string())
            .collect();
        paths.sort();
        paths
    }

    fn exists(p: &Path) -> bool {
        p.exists()
    }

    // ---- AC: base recorded at creation ---------------------------------------

    #[tokio::test]
    async fn ac_post_sessions_records_the_workspaces_head_as_the_sessions_base() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let head = git(&d, &["rev-parse", "HEAD"]).await.out.trim().to_string();
        let s = start_session(&fx, &d).await;
        // On the wire AND in the row: the response is what the database kept.
        assert_eq!(s["base"], head.as_str());
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .get_session_runtime(s["id"].as_str().unwrap())
                .unwrap()
                .base
                .as_deref(),
            Some(head.as_str())
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn base_a_repo_with_no_commits_records_the_empty_tree_so_it_still_has_a_diff() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(false).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        assert_eq!(s["base"], EMPTY_TREE);

        // Everything the session writes is its work — there is no earlier
        // state.
        std::fs::write(repo.join("first.txt"), "hello\n").unwrap();
        let set = changes_of(&fx, s["id"].as_str().unwrap()).await;
        assert_eq!(set["available"], true);
        assert_eq!(paths_of(&set), vec!["first.txt"]);
        assert_eq!(set["files"][0]["status"], "added");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn base_a_workspace_that_is_not_a_repository_records_nothing() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let dir = std::env::temp_dir().join(format!("bough-plain-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = start_session(&fx, &dir.to_string_lossy()).await;
        assert!(s.get("base").is_none() || s["base"].is_null(), "{s}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- AC: the diff after edits --------------------------------------------

    #[tokio::test]
    async fn ac_the_change_set_is_git_diff_base_plus_untracked_files() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        let id = s["id"].as_str().unwrap();
        // The agent works in the checkout, so "an edit" is just writing there.
        std::fs::write(repo.join("README.md"), "base\nmore\n").unwrap();
        std::fs::write(repo.join("new.txt"), "hi\n").unwrap();

        let set = changes_of(&fx, id).await;
        assert_eq!(set["available"], true);
        assert_eq!(
            set["base"].as_str(),
            fx.ctx.db.lock().unwrap().get_session_runtime(id).unwrap().base.as_deref()
        );
        assert_eq!(set["workspace"], d.as_str());

        assert_eq!(paths_of(&set), vec!["README.md", "new.txt"]);
        let by_path = |p: &str| {
            set["files"].as_array().unwrap().iter().find(|f| f["path"] == p).cloned().unwrap()
        };
        assert_eq!(by_path("README.md")["status"], "modified");
        // Untracked ⇒ all-added, with real content so the rail can render it.
        assert_eq!(by_path("new.txt")["status"], "added");
        assert_eq!(by_path("new.txt")["hunks"][0]["lines"], j!(["+hi"]));
        // vendor.txt was committed and never touched: not this session's work.
        assert!(!paths_of(&set).contains(&"vendor.txt".to_string()));

        // A staged edit is still the same change set — `git diff <commit>`
        // covers the index and the worktree both, so nothing is
        // double-counted or lost.
        run(&d, &["add", "new.txt"]).await;
        let staged = changes_of(&fx, id).await;
        assert_eq!(paths_of(&staged), vec!["README.md", "new.txt"]);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn changes_a_deleted_tracked_file_is_part_of_the_change_set() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        std::fs::remove_file(repo.join("vendor.txt")).unwrap();
        let set = changes_of(&fx, s["id"].as_str().unwrap()).await;
        assert_eq!(paths_of(&set), vec!["vendor.txt"]);
        assert_eq!(set["files"][0]["status"], "deleted");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn changes_build_cache_noise_is_filtered_from_what_the_rail_shows() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        std::fs::write(repo.join("real.py"), "x = 1\n").unwrap();
        std::fs::create_dir_all(repo.join("__pycache__")).unwrap();
        std::fs::write(repo.join("__pycache__/real.cpython-312.pyc"), "junk\n").unwrap();
        std::fs::write(repo.join("mod.pyc"), "junk\n").unwrap();
        std::fs::write(repo.join(".DS_Store"), "junk\n").unwrap();

        let set = changes_of(&fx, s["id"].as_str().unwrap()).await;
        assert_eq!(paths_of(&set), vec!["real.py"]);
        let _ = std::fs::remove_dir_all(&repo);
    }

    // ---- AC: per-path revert -------------------------------------------------

    #[tokio::test]
    async fn ac_per_path_revert_restores_one_file_and_leaves_its_siblings_edits_intact() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        let id = s["id"].as_str().unwrap();
        std::fs::write(repo.join("README.md"), "clobbered\n").unwrap(); // tracked edit
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(repo.join("sub/created.txt"), "made by the agent\n").unwrap(); // untracked
        std::fs::write(repo.join("kept.txt"), "keep me\n").unwrap(); // the sibling

        let before = changes_of(&fx, id).await;
        assert_eq!(paths_of(&before), vec!["README.md", "kept.txt", "sub/created.txt"]);

        let (status, outcome) =
            revert(&fx, id, Some(j!({"paths": ["README.md", "sub/created.txt"]}))).await;
        assert_eq!(status, 200);
        let mut reverted: Vec<&str> =
            outcome["reverted"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        reverted.sort();
        assert_eq!(reverted, vec!["README.md", "sub/created.txt"]);
        assert_eq!(outcome["skipped"], j!([]));
        assert_eq!(outcome["failed"], j!([]));

        // The tracked file is back at its base content; the created one is
        // gone along with the directory that existed only to hold it…
        assert_eq!(std::fs::read_to_string(repo.join("README.md")).unwrap(), "base\n");
        assert!(!exists(&repo.join("sub/created.txt")));
        assert!(!exists(&repo.join("sub")));
        // …and the sibling edit the reviewer did not pick is untouched.
        assert_eq!(std::fs::read_to_string(repo.join("kept.txt")).unwrap(), "keep me\n");

        let after = changes_of(&fx, id).await;
        assert_eq!(paths_of(&after), vec!["kept.txt"]);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn revert_tells_the_model_or_the_next_turn_re_applies_the_edit() {
        // The bug this pins, measured end to end: reverting wrote nothing the
        // agent could read — no message, no event, no session state — so the
        // next turn replayed its own successful patch, its green test run and
        // its summary, found the old code back, and concluded its write had
        // failed. It then "fixed the regression" by applying the edit again.
        // The one gesture that means "no, not that" was the one the model was
        // guaranteed to undo.
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        let id = s["id"].as_str().unwrap();
        std::fs::write(repo.join("README.md"), "the agent's edit\n").unwrap();
        let before = fx.ctx.db.lock().unwrap().messages_for(id).unwrap().len();

        let (status, _) = revert(&fx, id, Some(j!({"paths": ["README.md"]}))).await;
        assert_eq!(status, 200);

        let messages = fx.ctx.db.lock().unwrap().messages_for(id).unwrap();
        assert_eq!(messages.len(), before + 1, "a revert must leave a record the model replays");
        let note = messages.last().unwrap();
        assert_eq!(note.role, Role::System);
        let text: String = note
            .parts
            .iter()
            .map(|p| match p {
                Part::Text { text } => text.as_str(),
                _ => "",
            })
            .collect();
        // It must name the file, and say the two things that stop a re-apply:
        // this was deliberate, and it is not yours to repair.
        assert!(text.contains("README.md"), "{text}");
        assert!(text.to_lowercase().contains("revert"), "{text}");
        assert!(text.contains("Do not re-apply them unless you are asked to."), "{text}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn revert_a_revert_that_changed_nothing_says_nothing_to_the_model() {
        // Skipped paths revert nothing, and a note about an edit that is
        // still on disk would be a lie the model acts on — the mirror image
        // of the bug above.
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        let id = s["id"].as_str().unwrap();
        std::fs::write(repo.join("README.md"), "the agent's edit\n").unwrap();
        let before = fx.ctx.db.lock().unwrap().messages_for(id).unwrap().len();
        let (status, outcome) = revert(&fx, id, Some(j!({"paths": ["nope.txt"]}))).await;
        assert_eq!(status, 200);
        assert_eq!(outcome["reverted"], j!([]));
        assert_eq!(outcome["skipped"], j!(["nope.txt"]));
        assert_eq!(fx.ctx.db.lock().unwrap().messages_for(id).unwrap().len(), before);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn revert_an_absent_paths_reverts_everything_the_rail_is_showing() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        let id = s["id"].as_str().unwrap();
        std::fs::write(repo.join("README.md"), "clobbered\n").unwrap();
        std::fs::write(repo.join("new.txt"), "hi\n").unwrap();

        let (status, outcome) = revert(&fx, id, Some(j!({}))).await;
        assert_eq!(status, 200);
        let mut reverted: Vec<&str> =
            outcome["reverted"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        reverted.sort();
        assert_eq!(reverted, vec!["README.md", "new.txt"]);

        assert_eq!(std::fs::read_to_string(repo.join("README.md")).unwrap(), "base\n");
        assert!(!exists(&repo.join("new.txt")));
        assert_eq!(changes_of(&fx, id).await["files"], j!([]));
        let _ = std::fs::remove_dir_all(&repo);
    }

    // THE REGRESSION THIS PINS. `{paths: []}` used to mean "revert the whole
    // change set", identically to omitting the field — and an empty list is
    // what a caller produces by ACCIDENT: a selection loop that matched no
    // rows, a rail with nothing highlighted, a variable that came back empty.
    // That made the one request nobody types on purpose the most destructive
    // request in the API, against a change set that is every uncommitted file
    // in the checkout. It cost a real tree.
    #[tokio::test]
    async fn revert_an_explicit_empty_paths_is_refused_not_read_as_a_wildcard() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        let id = s["id"].as_str().unwrap();
        std::fs::write(repo.join("README.md"), "clobbered\n").unwrap();
        std::fs::write(repo.join("new.txt"), "hi\n").unwrap();

        let (status, body) = revert(&fx, id, Some(j!({"paths": []}))).await;
        assert_eq!(status, 400);
        let error = body["error"].as_str().unwrap();
        // The message has to carry the move, or the caller just retries the
        // same body.
        assert!(error.to_lowercase().contains("empty"), "{error}");
        assert!(error.contains("omit `paths`"), "{error}");

        // Nothing was touched — this is the whole point.
        assert_eq!(std::fs::read_to_string(repo.join("README.md")).unwrap(), "clobbered\n");
        assert_eq!(std::fs::read_to_string(repo.join("new.txt")).unwrap(), "hi\n");
        assert_eq!(paths_of(&changes_of(&fx, id).await), vec!["README.md", "new.txt"]);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn revert_a_path_outside_the_change_set_is_skipped_never_restored_or_deleted() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        let id = s["id"].as_str().unwrap();
        std::fs::write(repo.join("real.py"), "x = 1\n").unwrap();
        // Two paths the rail deliberately does not show: build noise, and a
        // file the repository ignores. Neither is the session's reviewable
        // work, so neither is revertable — and `git checkout <base> --
        // vendor.txt` would happily rewrite a file nobody in this session
        // ever opened.
        std::fs::write(repo.join("mod.pyc"), "junk\n").unwrap();
        std::fs::write(repo.join("ignored.txt"), "user's own\n").unwrap();

        let (status, outcome) = revert(
            &fx,
            id,
            Some(j!({"paths": ["real.py", "mod.pyc", "ignored.txt", "vendor.txt", "../escape.txt"]})),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(outcome["reverted"], j!(["real.py"]));
        let mut skipped: Vec<&str> =
            outcome["skipped"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        skipped.sort();
        assert_eq!(skipped, vec!["../escape.txt", "ignored.txt", "mod.pyc", "vendor.txt"]);

        assert!(!exists(&repo.join("real.py")));
        assert_eq!(std::fs::read_to_string(repo.join("mod.pyc")).unwrap(), "junk\n");
        assert_eq!(std::fs::read_to_string(repo.join("ignored.txt")).unwrap(), "user's own\n");
        assert_eq!(std::fs::read_to_string(repo.join("vendor.txt")).unwrap(), "untouched\n");
        let _ = std::fs::remove_dir_all(&repo);
    }

    // ---- AC: a non-repo workspace --------------------------------------------

    #[tokio::test]
    async fn ac_a_workspace_that_is_not_a_repository_answers_cleanly_200_with_a_reason() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let dir = std::env::temp_dir().join(format!("bough-plain-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = start_session(&fx, &dir.to_string_lossy()).await;
        let id = s["id"].as_str().unwrap();
        std::fs::write(dir.join("work.txt"), "the agent still works here\n").unwrap();

        let set = changes_of(&fx, id).await;
        // Not an empty diff and not an error: an ANSWER, with the reason
        // spelled out. The distinction is the whole point — "not a
        // repository" and "you changed nothing" are different facts.
        assert_eq!(set["available"], false);
        assert!(set["reason"].as_str().unwrap().contains("not a git repository"), "{set}");
        assert_eq!(set["files"], j!([]));
        assert!(set["base"].is_null());
        assert_eq!(set["workspace"], dir.to_string_lossy().as_ref());

        // Revert is unavailable, and says why in the same words.
        let (status, body) = revert(&fx, id, Some(j!({}))).await;
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("not a git repository"), "{body}");
        // The file the agent wrote is still there — a refused revert deletes
        // nothing.
        assert!(exists(&dir.join("work.txt")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_session_with_no_workspace_has_no_change_set_and_says_so() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req("POST", "/sessions", Some(j!({"title": "no workspace"}))))
            .await;
        let s = testutil::body_json(res).await;
        let id = s["id"].as_str().unwrap();

        let set = changes_of(&fx, id).await;
        assert_eq!(set["available"], false);
        assert!(set["workspace"].is_null());
        assert!(set["reason"].as_str().unwrap().contains("no workspace"), "{set}");
        let (status, _) = revert(&fx, id, Some(j!({}))).await;
        assert_eq!(status, 400);
    }

    #[tokio::test]
    async fn both_routes_404_on_an_unknown_session() {
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/sessions/nope/changes")).await;
        assert_eq!(res.status(), 404);
        let res =
            call.call(testutil::req("POST", "/sessions/nope/changes/revert", Some(j!({})))).await;
        assert_eq!(res.status(), 404);
    }

    #[tokio::test]
    async fn a_session_whose_base_was_never_recorded_reports_that_rather_than_the_whole_tree() {
        if !git_available() {
            return;
        }
        let fx = testutil::fixture();
        let repo = temp_repo(true).await;
        let d = repo.to_string_lossy().into_owned();
        let s = start_session(&fx, &d).await;
        // The pre-T8.5 state: a real repo workspace with a null base. The
        // tree must not be reported as this session's work.
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(bough_core::schema::parts::Session {
                id: "legacy".into(),
                title: "legacy".into(),
                kind: bough_core::schema::parts::SessionKind::Root,
                created_at: (fx.ctx.now)(),
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some(d.clone()),
                origin_dir: Some(d.clone()),
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
        std::fs::write(repo.join("README.md"), "base\nmore\n").unwrap();

        let legacy = changes_of(&fx, "legacy").await;
        assert_eq!(legacy["available"], false);
        assert!(legacy["reason"].as_str().unwrap().contains("no starting commit"), "{legacy}");
        assert_eq!(legacy["files"], j!([]));
        // …while the session that DID record one sees the same edit fine.
        assert_eq!(paths_of(&changes_of(&fx, s["id"].as_str().unwrap()).await), vec!["README.md"]);
        let _ = std::fs::remove_dir_all(&repo);
    }
}
