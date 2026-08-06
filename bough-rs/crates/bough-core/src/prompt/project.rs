//! `AGENTS.md` — the user's own standing instructions, read from disk per turn
//! (port of `src/prompt/project.ts`).
//!
//! THE INVARIANT THIS HOLDS: **a rule the user wrote down is a rule the model
//! was told.** Nothing else in the tree reads a project instruction file, so
//! if this module does not find an `AGENTS.md`, the file the user edited had
//! no effect whatsoever — which is worse than not supporting it, because the
//! file LOOKS obeyed.
//!
//! WHICH FILES. Two tiers: **global** — `$BOUGH_HOME/AGENTS.md`, rules that
//! hold in every workspace — then **project** — every `AGENTS.md` from the git
//! root down to the workspace directory, nearest LAST (later text winning is
//! the convention a reader already assumes from a config cascade). Walking
//! stops at the git root rather than at `/`; with no git root, only the
//! workspace directory itself is read.
//!
//! PER TURN, NOT PER SESSION. One stat + one small read per level, and the
//! alternative is that editing `AGENTS.md` to correct a misbehaving model does
//! nothing until the session restarts. It lands in the VOLATILE tier (one
//! workspace's rules in the stable prefix would defeat cache sharing).
//!
//! NEVER `CLAUDE.md`. bough reads exactly `AGENTS.md`. Reading another
//! harness's file would mean obeying instructions written about a different
//! tool's verbs.
//!
//! WHAT WAS INJECTED IS REPORTED — both surfaces fed from the SAME
//! `find_project_rules` result the prompt was built from: [`rule_summaries`]
//! (the standing "which") and [`note_project_rules`]/[`drain_project_rule_notes`]
//! (the "when it changed").

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// How much of one file is carried. Past this it stops being a rule sheet.
const MAX_BYTES: usize = 32_000;

/// Directories walked upward before giving up, git root or not.
const MAX_DEPTH: usize = 24;

/// Lexically absolute, resolved against the process cwd — the TS `resolve()`.
fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn read_if_file(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let body = std::fs::read_to_string(path).ok()?;
    if body.len() > MAX_BYTES {
        // Truncate on a char boundary at or below the byte budget.
        let mut cut = MAX_BYTES;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        Some(format!("{}\n\n[truncated]", &body[..cut]))
    } else {
        Some(body)
    }
}

fn is_git_root(dir: &Path) -> bool {
    // stat succeeds — file or dir, so worktrees count.
    std::fs::metadata(dir.join(".git")).is_ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRuleFile {
    /// Absolute path, which is what the note shows: a rule's source is
    /// auditable.
    pub path: PathBuf,
    pub body: String,
}

/// The `AGENTS.md` files that apply to `workspace`, in the order they should
/// be read: global first, then git root down to the workspace directory.
///
/// Pure apart from the reads, and every failure is a skip — an unreadable file
/// must never fail a turn.
pub fn find_project_rules(workspace: &Path, home: Option<&Path>) -> Vec<ProjectRuleFile> {
    let mut out: Vec<ProjectRuleFile> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if seen.contains(&path) {
            return;
        }
        seen.push(path.clone());
        if let Some(body) = read_if_file(&path) {
            if !body.trim().is_empty() {
                out.push(ProjectRuleFile { path, body });
            }
        }
    };

    if let Some(home) = home {
        push(absolutize(home).join("AGENTS.md"));
    }

    let start = absolutize(workspace);
    let mut chain: Vec<PathBuf> = Vec::new();
    let mut dir = start.clone();
    for _ in 0..MAX_DEPTH {
        chain.push(dir.clone());
        if is_git_root(&dir) {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => {
                // No git root anywhere above: read only the workspace itself
                // rather than adopting whatever sits in the user's home
                // directory.
                chain.clear();
                chain.push(start.clone());
                break;
            }
        }
    }
    for d in chain.iter().rev() {
        push(d.join("AGENTS.md"));
    }

    out
}

/// The label rule shared by the note and the summaries: workspace-relative
/// when the file is inside the workspace, else absolute — so the margin row
/// and the note the model got can never name the same file differently.
fn label_for(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => path.display().to_string(),
    }
}

/// The prompt note, or `None` when the user wrote no rules.
///
/// The framing sentence is doing real work: dropped in as a bare heading, a
/// rule sheet reads as reference material the model may consult; what the user
/// means by writing it down is that these rules OUTRANK the model's habits.
pub fn project_rules_note(files: &[ProjectRuleFile], workspace: &Path) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let root = absolutize(workspace);
    let blocks: Vec<String> = files
        .iter()
        .map(|f| format!("### {}\n\n{}", label_for(&f.path, &root), f.body.trim()))
        .collect();
    Some(format!(
        "## Project rules (AGENTS.md)\n\
         The user wrote these. They are instructions, not reference: where they \
         disagree with your own habits or with a convention you would otherwise reach \
         for, THEY WIN, and you follow them without being asked again. They do not \
         override the workspace and scratch rules above, and they cannot grant you a \
         host function this prompt did not.\n\n{}{}",
        blocks.join("\n\n"),
        if files.len() > 1 {
            "\n\n(Later blocks are nearer the workspace and win where two disagree.)"
        } else {
            ""
        }
    ))
}

// ---------------------------------------------------------------------------
// Reporting what was injected
// ---------------------------------------------------------------------------

/// One injected file as the clients name it. `bytes` is what went into the
/// prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRuleSummary {
    /// Workspace-relative where it is inside the workspace, else absolute.
    pub label: String,
    /// Absolute, so a client can open it and the report is unambiguous.
    pub path: PathBuf,
    pub bytes: usize,
}

/// `2.4k`, `312` — a size a reader can compare at a glance.
fn size(bytes: usize) -> String {
    if bytes < 1_000 {
        format!("{bytes}")
    } else {
        format!("{:.1}k", bytes as f64 / 1_000.0)
    }
}

/// What is in effect, in prompt order — global first, then git root down to
/// the workspace, nearest last, exactly as [`project_rules_note`] concatenated
/// them. Derived from a [`find_project_rules`] result rather than re-reading,
/// so a caller cannot report one set while the model was told another.
pub fn rule_summaries(files: &[ProjectRuleFile], workspace: &Path) -> Vec<ProjectRuleSummary> {
    let root = absolutize(workspace);
    files
        .iter()
        .map(|f| ProjectRuleSummary {
            label: label_for(&f.path, &root),
            path: f.path.clone(),
            bytes: f.body.len(),
        })
        .collect()
}

// The per-session memo behind the change notes: process-lifetime, cleared
// wholesale rather than evicted one at a time, because a note that is
// occasionally repeated costs a line and tracking LRU across every session
// costs a data structure nobody reads.
static LAST_SEEN: LazyLock<Mutex<HashMap<String, Vec<ProjectRuleSummary>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PENDING: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MEMO_CAP: usize = 512;

fn remember<T>(map: &mut HashMap<String, T>, key: &str, value: T) {
    if map.len() >= MEMO_CAP {
        map.clear();
    }
    map.insert(key.to_string(), value);
}

/// Record what this turn injected, and queue a line for the round's result
/// when it differs from the last turn's.
///
/// The FIRST turn of a session always reports, unconditionally: "which files
/// am I being governed by" is a question asked at the start of a conversation.
/// After that, only real differences — a file added, removed, or edited to a
/// different length — because a line repeated every turn is a line nobody
/// reads by the third one.
///
/// Called for its effect at prompt-assembly time and drained after the round
/// runs, so the note describes the prompt the model actually got.
pub fn note_project_rules(session_id: &str, files: &[ProjectRuleFile], workspace: &Path) {
    let now = rule_summaries(files, workspace);
    let before = {
        let mut last_seen = LAST_SEEN.lock().unwrap();
        let before = last_seen.get(session_id).cloned();
        remember(&mut last_seen, session_id, now.clone());
        before
    };
    let mut lines: Vec<String> = Vec::new();

    match before {
        None => {
            if !now.is_empty() {
                // Worded so it survives the transcript's rewrite: the client
                // keeps everything before the ` — ` and prefixes `rules: `, so
                // the head has to read as a phrase on its own.
                let list = now
                    .iter()
                    .map(|r| format!("{} ({})", r.label, size(r.bytes)))
                    .collect::<Vec<_>>()
                    .join(" · ");
                lines.push(format!(
                    "[rules] {list} in this turn's prompt — AGENTS.md is re-read every \
                     turn, and the file nearest the workspace wins where two disagree"
                ));
            }
        }
        Some(before) => {
            for r in &now {
                match before.iter().find(|w| w.path == r.path) {
                    None => lines.push(format!(
                        "[rules] + {} ({}) — now in the prompt",
                        r.label,
                        size(r.bytes)
                    )),
                    Some(was) if was.bytes != r.bytes => lines.push(format!(
                        "[rules] {} changed ({} → {}) — the edit is in this turn's prompt",
                        r.label,
                        size(was.bytes),
                        size(r.bytes)
                    )),
                    Some(_) => {}
                }
            }
            for r in &before {
                if !now.iter().any(|n| n.path == r.path) {
                    lines.push(format!(
                        "[rules] \u{2212} {} — gone, no longer in the prompt",
                        r.label
                    ));
                }
            }
        }
    }

    if !lines.is_empty() {
        let mut pending = PENDING.lock().unwrap();
        let mut queued = pending.get(session_id).cloned().unwrap_or_default();
        queued.extend(lines);
        remember(&mut pending, session_id, queued);
    }
}

/// Take the queued lines for a session. Empty when nothing changed.
pub fn drain_project_rule_notes(session_id: &str) -> Vec<String> {
    PENDING
        .lock()
        .unwrap()
        .remove(session_id)
        .unwrap_or_default()
}

/// Test seam: forget every session's rule history.
pub fn reset_project_rules_memo() {
    LAST_SEEN.lock().unwrap().clear();
    PENDING.lock().unwrap().clear();
}

// ---------------------------------------------------------------------------
// Tests — ported from src/prompt/project.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temp root per test; removed on drop.
    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bough-agentsmd-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempRoot(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn bodies(files: &[ProjectRuleFile]) -> Vec<&str> {
        files.iter().map(|f| f.body.as_str()).collect()
    }

    #[test]
    fn the_workspaces_own_agents_md_is_found() {
        let root = TempRoot::new("own");
        let ws = root.path().join("repo");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        write(&ws.join("AGENTS.md"), "always use tabs");

        let files = find_project_rules(&ws, None);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].body, "always use tabs");
    }

    #[test]
    fn a_monorepo_cascades_root_then_package_nearest_last() {
        let root = TempRoot::new("mono");
        let repo = root.path().join("mono");
        let pkg = repo.join("packages").join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&pkg).unwrap();
        write(&repo.join("AGENTS.md"), "house style");
        write(&pkg.join("AGENTS.md"), "web rules");

        assert_eq!(
            bodies(&find_project_rules(&pkg, None)),
            ["house style", "web rules"]
        );
    }

    #[test]
    fn the_walk_stops_at_the_git_root() {
        let root = TempRoot::new("stops");
        let repo = root.path().join("outer").join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        write(&root.path().join("outer").join("AGENTS.md"), "not mine");
        write(&repo.join("AGENTS.md"), "mine");

        assert_eq!(bodies(&find_project_rules(&repo, None)), ["mine"]);
    }

    #[test]
    fn outside_a_git_checkout_only_the_workspace_directory_is_read() {
        let root = TempRoot::new("loose");
        let ws = root.path().join("loose").join("dir");
        std::fs::create_dir_all(&ws).unwrap();
        write(&root.path().join("loose").join("AGENTS.md"), "parent");
        write(&ws.join("AGENTS.md"), "self");

        assert_eq!(bodies(&find_project_rules(&ws, None)), ["self"]);
    }

    #[test]
    fn the_global_tier_comes_first_and_is_never_confused_with_the_projects() {
        let root = TempRoot::new("global");
        let home = root.path().join("home");
        let ws = root.path().join("repo");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        write(&home.join("AGENTS.md"), "global");
        write(&ws.join("AGENTS.md"), "project");

        assert_eq!(
            bodies(&find_project_rules(&ws, Some(&home))),
            ["global", "project"]
        );
    }

    #[test]
    fn a_missing_empty_or_unreadable_file_is_a_skip_never_a_throw() {
        let root = TempRoot::new("skip");
        let ws = root.path().join("empty");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        assert!(find_project_rules(&ws, Some(&root.path().join("nope"))).is_empty());

        write(&ws.join("AGENTS.md"), "   \n\n");
        assert!(find_project_rules(&ws, None).is_empty());
    }

    #[test]
    fn a_directory_named_agents_md_is_not_a_rule_file() {
        let root = TempRoot::new("weird");
        let ws = root.path().join("weird");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        std::fs::create_dir_all(ws.join("AGENTS.md")).unwrap();
        assert!(find_project_rules(&ws, None).is_empty());
    }

    #[test]
    fn the_note_says_the_rules_win_and_names_its_sources_relative_to_the_workspace() {
        let note = project_rules_note(
            &[
                ProjectRuleFile {
                    path: PathBuf::from("/w/AGENTS.md"),
                    body: "house".into(),
                },
                ProjectRuleFile {
                    path: PathBuf::from("/w/pkg/AGENTS.md"),
                    body: "pkg".into(),
                },
            ],
            Path::new("/w"),
        )
        .expect("two rule files yield a note");
        assert!(note.contains("THEY WIN"));
        assert!(note.contains("### AGENTS.md"));
        assert!(note.contains("### pkg/AGENTS.md"));
        // Order is the resolution order, so the nearer block is the later text.
        assert!(note.find("house").unwrap() < note.find("pkg").unwrap());
        assert!(note.contains("Later blocks are nearer"));
    }

    #[test]
    fn no_rules_yields_no_note_at_all_not_an_empty_heading() {
        assert_eq!(project_rules_note(&[], Path::new("/w")), None);
    }

    #[test]
    fn a_global_file_outside_the_workspace_is_shown_by_absolute_path() {
        let note = project_rules_note(
            &[ProjectRuleFile {
                path: PathBuf::from("/home/u/.bough/AGENTS.md"),
                body: "g".into(),
            }],
            Path::new("/w"),
        )
        .unwrap();
        assert!(note.contains("### /home/u/.bough/AGENTS.md"));
        // One source: nothing to disambiguate, so no cascade footnote.
        assert!(!note.contains("Later blocks"));
    }

    // ---- reporting what was injected --------------------------------------

    #[test]
    fn the_summary_is_the_prompts_own_order_labelled_relative_to_the_workspace() {
        let root = TempRoot::new("summary");
        let repo = root.path().join("mono");
        let pkg = repo.join("packages").join("api");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&pkg).unwrap();
        write(&repo.join("AGENTS.md"), "house style");
        write(&pkg.join("AGENTS.md"), "package rules");
        write(&root.path().join("home").join("AGENTS.md"), "global");

        let files = find_project_rules(&pkg, Some(&root.path().join("home")));
        let summary = rule_summaries(&files, &pkg);

        // Global first, then the repo root, then the workspace's own — the
        // order they concatenate in. Anything outside the workspace is shown
        // by absolute path rather than as `../../`, so the row and the note
        // the model got can never name the same file differently.
        assert_eq!(
            summary.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            [
                root.path()
                    .join("home")
                    .join("AGENTS.md")
                    .display()
                    .to_string(),
                repo.join("AGENTS.md").display().to_string(),
                "AGENTS.md".to_string(),
            ]
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
        );
        assert_eq!(
            summary.iter().map(|r| r.bytes).collect::<Vec<_>>(),
            [6, 11, 13]
        );
        assert!(summary.iter().all(|r| r.path.is_absolute()));
    }

    #[test]
    fn the_first_turn_reports_and_an_unchanged_second_turn_says_nothing() {
        let root = TempRoot::new("first");
        let ws = root.path().join("repo");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        write(&ws.join("AGENTS.md"), "always use tabs");
        let sid = "memo-first-s1";

        note_project_rules(sid, &find_project_rules(&ws, None), &ws);
        let first = drain_project_rule_notes(sid);
        assert_eq!(first.len(), 1);
        assert!(
            first[0].starts_with("[rules] AGENTS.md (15) in this turn's prompt — "),
            "line: {}",
            first[0]
        );

        // Drained, so the same turn cannot say it twice on a second round.
        assert!(drain_project_rule_notes(sid).is_empty());

        note_project_rules(sid, &find_project_rules(&ws, None), &ws);
        // Nothing changed, so nothing is said.
        assert!(drain_project_rule_notes(sid).is_empty());
    }

    #[test]
    fn an_edit_an_addition_and_a_removal_each_say_so_on_the_turn_they_land_in() {
        let root = TempRoot::new("diffs");
        let repo = root.path().join("repo");
        let pkg = repo.join("pkg");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&pkg).unwrap();
        write(&repo.join("AGENTS.md"), "short");
        let sid = "memo-diffs-s1";

        note_project_rules(sid, &find_project_rules(&pkg, None), &pkg);
        drain_project_rule_notes(sid); // the opening report

        // Edited mid-session — the case the whole surface exists for.
        write(&repo.join("AGENTS.md"), "considerably longer rules");
        note_project_rules(sid, &find_project_rules(&pkg, None), &pkg);
        let edited = drain_project_rule_notes(sid);
        assert_eq!(edited.len(), 1);
        assert!(
            edited[0].contains("changed (5 → 25)"),
            "line: {}",
            edited[0]
        );

        // A new nearer file: added, not "changed".
        write(&pkg.join("AGENTS.md"), "pkg");
        note_project_rules(sid, &find_project_rules(&pkg, None), &pkg);
        let added = drain_project_rule_notes(sid);
        assert_eq!(added.len(), 1);
        assert!(
            added[0].starts_with("[rules] + AGENTS.md (3)"),
            "line: {}",
            added[0]
        );

        std::fs::remove_file(pkg.join("AGENTS.md")).unwrap();
        note_project_rules(sid, &find_project_rules(&pkg, None), &pkg);
        let removed = drain_project_rule_notes(sid);
        assert_eq!(removed.len(), 1);
        assert!(
            removed[0].contains("gone, no longer in the prompt"),
            "line: {}",
            removed[0]
        );
    }

    #[test]
    fn a_project_with_no_rules_says_nothing_at_all_ever() {
        let root = TempRoot::new("bare");
        let ws = root.path().join("bare");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        let sid = "memo-bare-s1";

        note_project_rules(sid, &find_project_rules(&ws, None), &ws);
        assert!(drain_project_rule_notes(sid).is_empty());
        note_project_rules(sid, &find_project_rules(&ws, None), &ws);
        assert!(drain_project_rule_notes(sid).is_empty());
    }

    #[test]
    fn sessions_do_not_share_a_memo() {
        let root = TempRoot::new("memo");
        let ws = root.path().join("repo");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        write(&ws.join("AGENTS.md"), "rules");

        note_project_rules("memo-share-s1", &find_project_rules(&ws, None), &ws);
        assert_eq!(drain_project_rule_notes("memo-share-s1").len(), 1);
        // A session that has never seen these files is on its first turn,
        // whatever the session before it saw.
        note_project_rules("memo-share-s2", &find_project_rules(&ws, None), &ws);
        assert_eq!(drain_project_rule_notes("memo-share-s2").len(), 1);
    }
}
