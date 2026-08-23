//! Named workflows: a run whose script did what you wanted, kept as a command.
//!
//! Port of `src/workflow/saved.ts` (row 3.12).
//!
//! WHY THIS EXISTS. Spec §8, "Saving a run": a run whose script did what you
//! wanted can be saved at `~/.bough/workflows/saved/<name>.js`, invoked by name
//! and parameterized through `args`, so an orchestration worth repeating
//! becomes a command rather than a script you re-derive. `rerun` is the wrong
//! verb for "do this again on a different branch", because a rerun replays the
//! journal it was told to seed from.
//!
//! THE INVARIANT THIS HOLDS: **a name can only ever address a file inside
//! `~/.bough/workflows/saved/`.** Names arrive in a URL path and in a request
//! body — the two least trustworthy inputs the server has — and every one of
//! them is spent building a filesystem path. So every path here is produced by
//! exactly one function, [`saved_path`], which validates the shape of the name
//! for a good error message and then hands the RELATIVE name to
//! [`crate::paths::confine`] as the backstop that decides. Both, in that order,
//! because they fail differently: the charset check tells a caller what a name
//! may contain, and `confine` catches everything a charset check forgets —
//! `..`, an absolute path, a separator that only becomes one after decoding.
//!
//! The relative name is what is confined, never the joined path: joining
//! swallows a leading slash, so `/etc/crontab` would land back under the saved
//! directory and pass a check made after the join.
//!
//! WHAT THIS IS NOT. Not a security boundary — programs run as the user and
//! write any file they like. What it stops is the case it can stop: a name in a
//! request steering the SERVER's own path construction out of its own store.
//!
//! WHAT IS NOT HERE. Starting a run. This module reads and writes files and
//! reads the database; the engine and the meta validation live behind
//! `workflow::control`, and the route composes the two. That keeps this file
//! pure filesystem math — drivable with no worker, no LLM and no engine.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::BoughError;
use crate::paths::{confine, workflow_script_path, workflows_dir};
use crate::types::SharedDb;

/// `~/.bough/workflows/saved` — beside the per-run mirrors, not inside them.
pub fn saved_dir() -> Result<PathBuf, BoughError> {
    confine(&workflows_dir(), Path::new("saved"))
}

/// The longest name that still reads as a command. Arbitrary, and stated once.
const MAX_NAME: usize = 64;

/// A name that may be typed, stored and logged: letters, digits, `.`, `_`, `-`,
/// starting with a letter or digit. Everything else — separators, spaces,
/// leading dots, control characters — is refused by name rather than silently
/// rewritten, because a saved workflow is addressed by the string the user typed.
///
/// The TS source states this as `/^[A-Za-z0-9][A-Za-z0-9._-]*$/`; spelled out
/// here so the port carries no regex engine for one predicate.
fn name_shape_ok(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Normalize a caller's name: trims, drops ONE trailing `.js` so `save("audit.js")`
/// and `save("audit")` name the same workflow rather than `audit.js.js`.
pub fn normalize_name(raw: &str) -> String {
    let name = raw.trim();
    if name.to_ascii_lowercase().ends_with(".js") {
        name[..name.len() - 3].to_string()
    } else {
        name.to_string()
    }
}

/// The absolute path for a saved workflow, or a 400 naming what is wrong with
/// the name.
///
/// Validate, then confine. The validation is the message; the confinement is
/// the answer.
pub fn saved_path(raw: &str) -> Result<PathBuf, BoughError> {
    let name = normalize_name(raw);
    if name.is_empty() {
        return Err(BoughError::bad_request(
            "a saved workflow needs a name — POST {name: \"branch-review\"}. It becomes \
             ~/.bough/workflows/saved/<name>.js and is how the workflow is invoked.",
        ));
    }
    // TS measures with `String.length` — UTF-16 code units, not bytes.
    let len = name.encode_utf16().count();
    if len > MAX_NAME {
        return Err(BoughError::bad_request(format!(
            "saved workflow name is {len} characters, longer than the {MAX_NAME} allowed — \
             it is a command name, not a description. The description belongs in the \
             script's `meta`."
        )));
    }
    if !name_shape_ok(&name) {
        return Err(BoughError::bad_request(format!(
            "saved workflow name {} is not usable — it may contain letters, digits, '.', \
             '_' and '-', and must start with a letter or digit. Path separators and '..' \
             are refused: a name addresses one file inside ~/.bough/workflows/saved/, \
             never a path.",
            json_quote(&name),
        )));
    }
    // The backstop, and the one that decides. Relative name, never the joined path.
    confine(&saved_dir()?, Path::new(&format!("{name}.js")))
}

/// `JSON.stringify(s)` for a string — the quoting the TS messages carry.
fn json_quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

/// A saved workflow as the API lists it. The script itself is only in the
/// detail read.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedWorkflow {
    pub name: String,
    pub path: String,
    /// From the script's `meta`, when it has one. Empty otherwise — a listing
    /// never fails on one malformed file.
    pub description: String,
    pub bytes: u64,
    pub updated_at: i64,
}

/// The saved workflow plus its script — what an invocation and an edit both need.
///
/// TS models this as `SavedWorkflow & {script}`; the flattened struct keeps the
/// wire shape identical (`{name, path, description, bytes, updatedAt, script}`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedWorkflowDetail {
    pub name: String,
    pub path: String,
    pub description: String,
    pub bytes: u64,
    pub updated_at: i64,
    pub script: String,
}

// ---------------------------------------------------------------------------
// meta.description
// ---------------------------------------------------------------------------

/// `meta.description` if the script has a valid one, else `""`.
///
/// Deliberately swallowing: `meta` is validated when a run STARTS
/// (`workflow::meta`), which is where a bad one must be refused. A listing that
/// threw on one malformed file would hide every other saved workflow behind it.
fn describe(script: &str) -> String {
    crate::workflow::meta::extract_meta(script)
        .map(|m| m.description)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// `(size, mtime-ms)`; `mtime` 0 when the platform will not say.
fn stat_of(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    Some((meta.len(), mtime))
}

/// Save a script under a name. Overwrites — a name is a command, not a version.
pub fn save_workflow(name: &str, script: &str, now: i64) -> Result<SavedWorkflow, BoughError> {
    let path = saved_path(name)?;
    if script.trim().is_empty() {
        return Err(BoughError::bad_request(
            "a saved workflow needs a script — pass {script} directly, or {runId} to save \
             the script a finished run actually executed.",
        ));
    }
    let dir = saved_dir()?;
    std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&path, script))
        .map_err(|e| BoughError::bad_request(format!("could not save the workflow: {e}")))?;
    let (bytes, mtime) = stat_of(&path).ok_or_else(|| {
        BoughError::bad_request("the saved workflow vanished between write and read")
    })?;
    Ok(SavedWorkflow {
        name: normalize_name(name),
        path: path.to_string_lossy().into_owned(),
        description: describe(script),
        bytes,
        // TS: `stat.mtime?.getTime() ?? Date.now()`.
        updated_at: if mtime == 0 { now } else { mtime },
    })
}

/// Save the script a run actually ran, under a name.
///
/// The MIRROR first, then the row — the same order a relaunch resolves them. A
/// user who edited `~/.bough/workflows/<id>.js` and relaunched is saving the
/// script that produced the result they liked; saving the stored row instead
/// would quietly save the version they replaced.
pub fn save_run_as(
    db: &SharedDb,
    run_id: &str,
    name: &str,
    now: i64,
) -> Result<SavedWorkflow, BoughError> {
    let run = db
        .lock()
        .unwrap()
        .get_workflow(run_id)?
        .ok_or_else(|| BoughError::not_found(format!("workflow {run_id} not found")))?;
    // `saved_path` first, so a bad name fails before anything is read.
    saved_path(name)?;
    // `journal_fs::read_mirror` (row 3.9) is exactly this read; inlined until
    // it lands, because the mirror is the file `paths::workflow_script_path`
    // names and nothing else.
    let script = std::fs::read_to_string(workflow_script_path(run_id)).unwrap_or(run.script);
    save_workflow(name, &script, now)
}

/// Every saved workflow, by name. An absent directory lists empty, not an error.
pub fn list_saved_workflows() -> Vec<SavedWorkflow> {
    let Ok(dir) = saved_dir() else {
        return Vec::new();
    };
    // Nothing saved yet — the directory is created at boot and on first save.
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        let file = entry.file_name().to_string_lossy().into_owned();
        if is_file && file.ends_with(".js") {
            names.push(file[..file.len() - 3].to_string());
        }
    }
    let mut out = Vec::new();
    for name in names {
        // A file placed by hand under a name the API cannot address is skipped.
        let Ok(path) = saved_path(&name) else {
            continue;
        };
        // Vanished or unreadable between the listing and the read.
        let Ok(script) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some((bytes, mtime)) = stat_of(&path) else {
            continue;
        };
        out.push(SavedWorkflow {
            name,
            path: path.to_string_lossy().into_owned(),
            description: describe(&script),
            bytes,
            updated_at: mtime,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// One saved workflow, script included — the read an invocation makes.
///
/// A missing file is a 404 naming the name, not an empty script: invoking a
/// workflow that is not there must not start an empty run.
pub fn read_saved_workflow(name: &str) -> Result<SavedWorkflowDetail, BoughError> {
    let path = saved_path(name)?;
    let script = std::fs::read_to_string(&path).map_err(|_| {
        BoughError::not_found(format!(
            "no saved workflow named {} — GET /saved-workflows lists what is saved, and \
             POST /workflows/<id>/save {{name}} saves a run's script under one.",
            json_quote(&normalize_name(name)),
        ))
    })?;
    let stat = stat_of(&path);
    Ok(SavedWorkflowDetail {
        name: normalize_name(name),
        path: path.to_string_lossy().into_owned(),
        description: describe(&script),
        bytes: stat.map_or(script.encode_utf16().count() as u64, |(b, _)| b),
        updated_at: stat.map_or(0, |(_, m)| m),
        script,
    })
}

/// Remove a saved workflow. `false` when there was nothing under that name.
pub fn delete_saved_workflow(name: &str) -> Result<bool, BoughError> {
    let path = saved_path(name)?;
    // No `force`: a missing file must fail, so this reports `false`.
    Ok(std::fs::remove_file(&path).is_ok())
}

/// Create the saved directory at boot so `~/.bough/workflows/saved/` is a place
/// the user can drop a script into, not one that only appears after the first
/// API save. Returns how many workflows are there, for the boot line.
pub fn ensure_saved_dir() -> usize {
    let Ok(dir) = saved_dir() else { return 0 };
    if std::fs::create_dir_all(&dir).is_err() {
        // Read-only ~/.bough: saving will report its own error when tried.
        return 0;
    }
    list_saved_workflows().len()
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{Session, SessionKind, WorkflowRun, WorkflowStatus};
    use std::sync::{Arc, Mutex};

    const META: &str =
        "export const meta = { name: 'branch-review', description: 'review a branch' }\n";

    /// `BOUGH_HOME` is process-global and cargo runs tests in parallel
    /// threads, so every test that relocates it takes the CRATE-WIDE lock in
    /// `paths::test_env` — a module-local lock only serializes this file
    /// against itself and still races `paths`, `scratch` and every other module
    /// that moves the same variable.
    fn with_home(f: impl FnOnce()) {
        let home = std::env::temp_dir().join(format!("bough-saved-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], f);
        let _ = std::fs::remove_dir_all(&home);
    }

    fn mem_db() -> SharedDb {
        Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ))
    }

    fn seed_run(db: &SharedDb, script: &str) -> String {
        let guard = db.lock().unwrap();
        let sid = uuid::Uuid::new_v4().to_string();
        guard
            .create_session(Session {
                id: sid.clone(),
                title: "s".into(),
                kind: SessionKind::Root,
                created_at: 1,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some("/tmp/checkout".into()),
                origin_dir: Some("/tmp/checkout".into()),
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
                description: None,
            })
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        guard
            .create_workflow(WorkflowRun {
                id: id.clone(),
                session_id: sid,
                name: "branch-review".into(),
                description: "review a branch".into(),
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
            })
            .unwrap();
        id
    }

    /// Port of saved.test.ts "a name is normalized once".
    #[test]
    fn a_name_is_normalized_once_one_trailing_js_trimmed_never_doubled() {
        assert_eq!(normalize_name("  branch-review  "), "branch-review");
        assert_eq!(normalize_name("branch-review.js"), "branch-review");
        assert_eq!(normalize_name("branch-review.JS"), "branch-review");
        assert_eq!(normalize_name("a.js.js"), "a.js");
        assert_eq!(normalize_name(""), "");
    }

    /// The acceptance criterion the spec states as this module's invariant: a
    /// name can only ever address a file inside `saved/`. Every escape shape,
    /// through the ONE function that builds a path.
    #[test]
    fn a_name_can_only_ever_address_a_file_inside_the_saved_dir() {
        with_home(|| {
            let dir = saved_dir().unwrap();
            for escape in [
                "../../etc/crontab",
                "..",
                "/etc/crontab",
                "a/b",
                "a\\b",
                ".hidden",
                "-leading-dash",
                " ",
                "",
                "name with spaces",
                "nul\0byte",
            ] {
                let out = saved_path(escape);
                assert!(out.is_err(), "{escape:?} produced a path: {out:?}");
                assert_eq!(out.unwrap_err().status(), 400, "{escape:?}");
            }
            // A 65-character name is refused; 64 is not.
            assert!(saved_path(&"a".repeat(65)).is_err());
            assert!(saved_path(&"a".repeat(64)).unwrap().starts_with(&dir));
            // And the ordinary case lands exactly where it says it does.
            assert_eq!(
                saved_path("branch-review.js").unwrap(),
                dir.join("branch-review.js")
            );
        });
    }

    /// Port of saved.test.ts "saving a run saves the mirror the user edited".
    #[test]
    fn saving_a_run_saves_the_mirror_the_user_edited_not_the_stored_row() {
        with_home(|| {
            let db = mem_db();
            let run_id = seed_run(
                &db,
                &format!("{META}return await agent('review the row version')"),
            );
            std::fs::create_dir_all(workflows_dir()).unwrap();
            std::fs::write(
                workflow_script_path(&run_id),
                format!("{META}return await agent('review the EDITED version')"),
            )
            .unwrap();

            let saved = save_run_as(&db, &run_id, "branch-review", 5).unwrap();
            assert_eq!(saved.name, "branch-review");
            assert_eq!(saved.description, "review a branch");
            assert!(saved
                .path
                .starts_with(&format!("{}/", saved_dir().unwrap().display())));

            let read = read_saved_workflow("branch-review").unwrap();
            assert!(read.script.contains("EDITED version"), "{}", read.script);
            assert!(read.bytes > 0);

            // No mirror on disk: the row is the fallback, so a cleaned
            // ~/.bough still saves.
            let bare = seed_run(&db, &format!("{META}return await agent('only the row')"));
            let second = save_run_as(&db, &bare, "row-only", 5).unwrap();
            let back = read_saved_workflow(&second.name).unwrap();
            assert!(back.script.contains("only the row"), "{}", back.script);

            let missing = save_run_as(&db, "no-such-run", "x", 5).unwrap_err();
            assert_eq!(missing.status(), 404);
            assert_eq!(missing.to_string(), "workflow no-such-run not found");
        });
    }

    /// Port of saved.test.ts "saving is idempotent on the name, and listing
    /// carries meta.description".
    #[test]
    fn saving_is_idempotent_on_the_name_and_listing_carries_meta_description() {
        with_home(|| {
            assert_eq!(ensure_saved_dir(), 0);
            save_workflow("branch-review", &format!("{META}return 1"), 1).unwrap();
            save_workflow("branch-review", &format!("{META}return 2"), 1).unwrap();
            save_workflow("zzz-last", "return 3", 1).unwrap(); // no meta: listing still works

            let listed = list_saved_workflows();
            assert_eq!(
                listed.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                ["branch-review", "zzz-last"]
            );
            assert_eq!(
                listed
                    .iter()
                    .map(|s| s.description.as_str())
                    .collect::<Vec<_>>(),
                ["review a branch", ""]
            );
            assert!(read_saved_workflow("branch-review")
                .unwrap()
                .script
                .contains("return 2"));
            assert_eq!(ensure_saved_dir(), 2);

            assert!(delete_saved_workflow("zzz-last").unwrap());
            assert!(
                !delete_saved_workflow("zzz-last").unwrap(),
                "deleting twice is not an error"
            );
            let gone = read_saved_workflow("zzz-last").unwrap_err();
            assert_eq!(gone.status(), 404);
            assert!(
                gone.to_string()
                    .starts_with("no saved workflow named \"zzz-last\" — "),
                "{}",
                gone.to_string()
            );
        });
    }

    /// A blank script is refused before anything is written — `{script}` or
    /// `{runId}`, named in the message.
    #[test]
    fn a_blank_script_is_refused_and_nothing_is_written() {
        with_home(|| {
            let err = save_workflow("x", "   \n", 1).unwrap_err();
            assert_eq!(err.status(), 400);
            assert!(err.to_string().contains("{runId}"), "{}", err.to_string());
            assert!(list_saved_workflows().is_empty());
        });
    }

    /// `describe` is the swallowing wrapper around `meta::extract_meta`: a
    /// VALID meta gives its description, and everything else — a computed
    /// value, a missing field, a meta that fails validation, no meta at all —
    /// gives `""` rather than failing the listing it is part of.
    #[test]
    fn describe_answers_a_valid_meta_and_swallows_every_other_shape() {
        assert_eq!(describe(META), "review a branch");
        assert_eq!(
            describe("export const meta = {\n  name: \"a\",\n  description: \"b c\",\n}\n"),
            "b c"
        );
        // Not a valid meta: `name` is required, so the whole literal is refused.
        assert_eq!(
            describe("export const meta = { description: 'only a description' }"),
            ""
        );
        // Computed, and therefore not a literal at all.
        assert_eq!(
            describe("export const meta = { name: 'a', description: desc() }"),
            ""
        );
        // No declaration, and a script that is not a meta.
        assert_eq!(describe("const meta = { name: 'a', description: 'x' }"), "");
        assert_eq!(describe("return 3"), "");
    }
}
