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
//! `CLAUDE.md` IS A FALLBACK, NEVER A SECOND FILE. Per directory: `AGENTS.md`
//! if it exists, else `CLAUDE.md`. A repo that has both is read exactly as it
//! was before this fallback existed — the bough-native file wins and the CC
//! one is not read at all — so nothing that was tuned against `AGENTS.md`
//! changes behaviour, while a CC-only repo stops being silently ignored. The
//! two are never concatenated: they are the same document written for two
//! harnesses, and injecting both means the model reads one project's rules
//! twice, in two dialects, with no way to tell which it should follow.
//!
//! The cost of the fallback is real and accepted: a `CLAUDE.md` is written
//! about a different tool's verbs (its tool names, its slash commands, its
//! permission model). [`project_rules_note`] says so in the framing sentence
//! when any of the files it carries is a `CLAUDE.md`, because the alternative
//! is the model trying to invoke a tool bough does not have.
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectRuleFile {
    /// Absolute path, which is what the note shows: a rule's source is
    /// auditable.
    pub path: PathBuf,
    pub body: String,
    /// The paths this block was merged from, when it is the union of two
    /// near-identical files. Empty for the ordinary case of one file.
    ///
    /// A MERGE THE USER CANNOT SEE IS A RULE THEY CANNOT AUDIT, which is the
    /// one thing this module refuses to allow. Every surface that names a
    /// rule's source reads this, so a merged block says it is one.
    pub merged_from: Vec<PathBuf>,
}

/// The instruction file names one directory may contribute, in precedence
/// order. The FIRST one that exists and has content is taken and the rest of
/// the list is not consulted for that directory.
const RULE_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Is this one of the foreign files, read only because the native one was
/// absent? Drives the extra sentence in [`project_rules_note`].
pub fn is_foreign_rule_file(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "CLAUDE.md")
}

/// The project instruction files that apply to `workspace`, in the order they
/// should be read: global first, then git root down to the workspace
/// directory.
///
/// One file per directory: `AGENTS.md`, or `CLAUDE.md` when that directory has
/// no `AGENTS.md`. A directory holding both contributes only the `AGENTS.md`,
/// so adding an `AGENTS.md` beside an existing `CLAUDE.md` is how a project
/// takes the bough-native path without deleting anything.
///
/// Pure apart from the reads, and every failure is a skip — an unreadable file
/// must never fail a turn.
pub fn find_project_rules(workspace: &Path, home: Option<&Path>) -> Vec<ProjectRuleFile> {
    let mut out: Vec<ProjectRuleFile> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    // One directory contributes at most one file. The blank-file case falls
    // through deliberately: a `AGENTS.md` that is whitespace has the same
    // effect as an absent one everywhere else in this module, so it must not
    // shadow a `CLAUDE.md` that actually says something.
    let mut push = |dir: PathBuf| {
        for name in RULE_FILES {
            let path = dir.join(name);
            if seen.contains(&path) {
                return;
            }
            seen.push(path.clone());
            if let Some(body) = read_if_file(&path) {
                if !body.trim().is_empty() {
                    out.push(ProjectRuleFile {
                        path,
                        body,
                        merged_from: Vec::new(),
                    });
                    return;
                }
            }
        }
    };

    if let Some(home) = home {
        push(absolutize(home));
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
        push(d.clone());
    }

    out
}

/// Directories a session has actually run a command in, beyond its own
/// workspace. Keyed by session, capped like the other memos here.
static WORKED_IN: LazyLock<Mutex<HashMap<String, Vec<PathBuf>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// How many subdirectories one session's rules can be drawn from.
///
/// A session that roams is real, and every directory it visits adds an
/// `AGENTS.md` to every subsequent prompt — permanently, since nothing here
/// forgets. The cap is the backstop on that: past it, the oldest directory
/// drops out, so a long session's prompt stops growing instead of quietly
/// eating the context window.
const MAX_WORKED_IN: usize = 8;

/// Record that this session ran something in `dir`, so the next prompt reads
/// that directory's rules too.
///
/// Called from the shell boundary, where a command's real directory is known.
/// A session opened in `$HOME` that works inside one repo has to pick up that
/// repo's `AGENTS.md`, and `find_project_rules` cannot find it on its own: it
/// walks UP from the workspace to the git root, so nothing below the workspace
/// is ever reachable. This is how a directory gets below-the-workspace rules
/// into the prompt at all.
///
/// ONE TURN LATE, BY CONSTRUCTION. The prompt is assembled before the turn's
/// commands run, so the repo you just stepped into governs the NEXT turn. The
/// alternative — guessing the directory from paths in the user's message —
/// injects rules for a directory the turn may never touch, and a rule block
/// that should not be there is worse than one that arrives a turn later.
pub fn note_worked_in(session_id: &str, dir: &Path) {
    let dir = absolutize(dir);
    let mut map = WORKED_IN.lock().unwrap();
    let mut dirs = map.get(session_id).cloned().unwrap_or_default();
    if dirs.contains(&dir) {
        return;
    }
    dirs.push(dir);
    if dirs.len() > MAX_WORKED_IN {
        dirs.remove(0);
    }
    remember(&mut map, session_id, dirs);
}

/// The directories [`note_worked_in`] recorded, oldest first.
pub fn worked_in(session_id: &str) -> Vec<PathBuf> {
    WORKED_IN
        .lock()
        .unwrap()
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

/// [`find_project_rules`] for the workspace, then for every directory this
/// session has worked in — one merged list, no file twice.
///
/// ORDER IS THE PRECEDENCE. The existing rule is "later text wins", and the
/// note says so in as many words, so the workspace's own chain leads and each
/// visited directory's rules follow in the order they were first visited. The
/// practical effect is the one you want: a `~/.bough/AGENTS.md` global still
/// applies, and the repo you are actually working in overrides it.
///
/// `extra` directories contribute only files the merged list does not already
/// carry — a visited subdirectory shares most of its ancestry with the
/// workspace, and re-reading the same `AGENTS.md` under a second heading would
/// double it in the prompt and say nothing new. [`keep_first`] decides what
/// "already carry" means, on two keys rather than one.
pub fn find_project_rules_across(
    workspace: &Path,
    extra: &[PathBuf],
    home: Option<&Path>,
    claude_home: Option<&Path>,
) -> Vec<ProjectRuleFile> {
    let mut out = with_user_tier(find_project_rules(workspace, home), claude_home);
    for dir in extra {
        // `home` is deliberately not passed again: the global tier is already
        // in `out` and must not be re-inserted mid-list, where "later wins"
        // would let it override the project rules it is supposed to sit under.
        out.extend(find_project_rules(dir, None));
    }
    // Identity and content first, then the near-copies that survive both.
    merge_near_duplicates(keep_first(out))
}

/// Drop every file the list already carries — by IDENTITY, then by CONTENT.
///
/// **Identity, canonicalized.** Path equality alone was not enough, and the
/// asymmetry that broke it is real: the shell boundary canonicalizes the
/// directory it records (`command_workspace`), while the workspace itself is
/// only made lexically absolute, because resolving symlinks there would change
/// which path the model is told it is working in. So one repo reached as
/// `~/repos/thing` and as its symlink target produced two entries for one
/// file, and "later wins" then read the second as a more specific tier
/// restating the first. `canonicalize` is what makes the two spellings one
/// key; a path that cannot be resolved falls back to itself, since a file that
/// was just read almost always can be.
///
/// **Content, because identity is not enough.** Two genuinely different paths
/// hold the same bytes more often than they should: a monorepo whose packages
/// each carry the same boilerplate, or a `~/.claude/CLAUDE.md` kept in sync
/// with `$BOUGH_HOME/AGENTS.md` by copying rather than linking. No path check
/// can see those. The second copy adds nothing a reader could act on and costs
/// its own length in a window the user pays for.
///
/// FIRST WINS, which is the earlier tier — the later copy is byte-identical,
/// so nothing about "later wins" is lost by dropping it.
fn keep_first(files: Vec<ProjectRuleFile>) -> Vec<ProjectRuleFile> {
    let mut out: Vec<ProjectRuleFile> = Vec::with_capacity(files.len());
    let mut ids: Vec<PathBuf> = Vec::with_capacity(files.len());
    for file in files {
        let id = file
            .path
            .canonicalize()
            .unwrap_or_else(|_| file.path.clone());
        if ids.contains(&id) || out.iter().any(|f| f.body == file.body) {
            continue;
        }
        ids.push(id);
        out.push(file);
    }
    out
}

/// [`find_project_rules`] with the user-level Claude Code tier appended to the
/// global one: `$BOUGH_HOME/AGENTS.md` (or `CLAUDE.md`) first, then
/// `~/.claude/CLAUDE.md`.
///
/// A SEPARATE FUNCTION, not a widened `home` argument, because the two globals
/// are not interchangeable — `$BOUGH_HOME` is redirected by every test and by
/// `bough --home`, and `~/.claude` is a real user directory that must not move
/// with it. Both are read: they are different documents (one the user wrote
/// for bough, one for Claude Code), unlike the per-directory pair.
pub fn find_project_rules_with_user_tier(
    workspace: &Path,
    home: Option<&Path>,
    claude_home: Option<&Path>,
) -> Vec<ProjectRuleFile> {
    with_user_tier(find_project_rules(workspace, home), claude_home)
}

/// Put `~/.claude/CLAUDE.md` at the front of an already-resolved list.
///
/// Extracted so the workspace-only and the across-directories entry points
/// cannot disagree about whether the user tier is read — for most of this
/// module's life the runner called the variant that skipped it, so the file
/// was supported, tested, and never once in a prompt.
fn with_user_tier(
    mut out: Vec<ProjectRuleFile>,
    claude_home: Option<&Path>,
) -> Vec<ProjectRuleFile> {
    let Some(claude_home) = claude_home else {
        return out;
    };
    let path = absolutize(claude_home).join("CLAUDE.md");
    if out.iter().any(|f| f.path == path) {
        return out;
    }
    if let Some(body) = read_if_file(&path) {
        if !body.trim().is_empty() {
            // Global tier, so it goes ahead of everything the project said —
            // nearest still wins.
            out.insert(
                0,
                ProjectRuleFile {
                    path,
                    body,
                    merged_from: Vec::new(),
                },
            );
        }
    }
    out
}

/// Where Claude Code keeps its user-level rules. Not `paths.rs`'s business:
/// nothing about it moves with `$BOUGH_HOME`, which is the whole reason it is
/// a second argument everywhere rather than a widened `home`.
pub fn claude_user_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// Every rule file one session's next turn will inject, in order.
///
/// THE ONE RESOLUTION. Two surfaces answer "which files govern this session" —
/// the prompt itself and the session listing the panel reads — and the module
/// contract is that both come from the SAME read. They drifted anyway, twice:
/// the listing never learned about directories the session had worked in, and
/// neither of them read the Claude Code user tier. Both callers now come
/// through here, so the next thing added to the cascade cannot reach one and
/// miss the other.
///
/// Resolves both homes itself, unlike everything above it — that is the point,
/// and it is why the pure functions it calls keep taking them as arguments.
pub fn session_rule_files(workspace: &Path, session_id: &str) -> Vec<ProjectRuleFile> {
    find_project_rules_across(
        workspace,
        &worked_in(session_id),
        Some(&crate::paths::bough_home()),
        claude_user_dir().as_deref(),
    )
}

/// Two rule files that are nearly the same document, as a percentage.
///
/// WHAT `keep_first` CANNOT SEE. Byte equality collapses a copy; it does
/// nothing for a copy that has since been edited, which is the state every
/// duplicated rules file eventually reaches. The real instance that prompted
/// this: a `~/.claude/CLAUDE.md` and a `$BOUGH_HOME/AGENTS.md` of 8,239 and
/// 8,261 bytes, 92% identical — four thousand tokens per turn to say almost
/// the same thing twice, and the 8% where they DIFFER is two contradictory
/// answers to the same question with nothing to say which one governs.
///
/// Reported, never acted on. Dropping one is not this module's call: they are
/// not the same file and the harness cannot know which one the user meant.
/// Naming the pair is the whole contribution — it is a thing you fix once, in
/// a text editor, and never think about again.
///
/// Compared as SETS OF NON-BLANK LINES, which is what survives the edits
/// people actually make to these files: reordering sections, and rewrapping a
/// paragraph. A character-level ratio calls a reordered file different, and a
/// reordered file is exactly the duplicate worth catching.
pub fn similarity(a: &str, b: &str) -> u8 {
    let lines = |t: &str| -> Vec<String> {
        t.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    };
    let (mut left, right) = (lines(a), lines(b));
    if left.is_empty() || right.is_empty() {
        return 0;
    }
    let total = left.len().max(right.len());
    let mut shared = 0usize;
    // Each line on the right consumes at most one on the left, so a file that
    // repeats one line a hundred times cannot claim a hundred matches.
    for line in right {
        if let Some(at) = left.iter().position(|l| *l == line) {
            left.remove(at);
            shared += 1;
        }
    }
    ((shared * 100) / total) as u8
}

/// How much two rule files must share before they are treated as one
/// document. Deliberately high: two files that share a preamble are ordinary;
/// two that share nine tenths of their lines are one document that got forked.
pub const NEAR_DUPLICATE_PCT: u8 = 80;

/// The union of two near-identical documents: shared lines once, divergent
/// lines from BOTH, in order.
///
/// WHY UNION AND NOT A CHOICE. This module's invariant is that a rule the user
/// wrote down is a rule the model was told. Picking one file discards lines
/// the user wrote; emitting conflict markers hands the model a merge to
/// resolve at inference time, which makes the governing rules nondeterministic
/// between two turns of the same conversation. The union is the only
/// resolution that keeps every line and still produces one stable document.
///
/// It is exactly `git merge-file --union` against a base of the two files'
/// longest common subsequence — the same synthetic-base trick you would use by
/// hand, because two rule files have no common ancestor to merge against.
///
/// EARLIER FILE FIRST inside a divergent hunk, so the later file's variant is
/// the one a reader ends on. That matches the cascade's own "later wins"
/// convention for the case where the two hunks really are alternatives.
pub fn union_merge(a: &str, b: &str) -> String {
    let left: Vec<&str> = a.lines().collect();
    let right: Vec<&str> = b.lines().collect();
    let common = lcs(&left, &right);

    let mut out: Vec<&str> = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0usize, 0usize);
    for anchor in common {
        // Everything either side added before the next shared line, both kept.
        while i < left.len() && left[i] != anchor {
            out.push(left[i]);
            i += 1;
        }
        while j < right.len() && right[j] != anchor {
            // A line the left side already contributed in this same hunk is
            // not added twice — the two files share most of their text, and a
            // duplicated line is the defect this whole path exists to remove.
            if !out.ends_with(&[right[j]]) {
                out.push(right[j]);
            }
            j += 1;
        }
        out.push(anchor);
        i += 1;
        j += 1;
    }
    while i < left.len() {
        out.push(left[i]);
        i += 1;
    }
    while j < right.len() {
        if !out.ends_with(&[right[j]]) {
            out.push(right[j]);
        }
        j += 1;
    }

    // A hunk seam can leave two blank lines where each side ended with one.
    // Left alone deliberately: collapsing runs of blanks would also rewrite
    // the spacing INSIDE regions neither file disagreed about, and this
    // function's contract is that it reorganises nothing it did not have to.
    // Measured against `git merge-file --union` on a real pair, the whole
    // difference is one blank line at one seam.
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// The longest common subsequence of two line slices.
///
/// Quadratic, and bounded by the fact that it only ever runs on two files this
/// module has already decided are ≥[`NEAR_DUPLICATE_PCT`] the same — at most a
/// few hundred lines each, once per turn.
fn lcs<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<&'a str> {
    let (n, m) = (a.len(), b.len());
    // A BLANK LINE IS NOT AN ANCHOR. It matches everywhere, so allowing it to
    // pair splits one side's divergent region into pieces around it, and the
    // pieces then interleave with the other side's. Measured against
    // `git merge-file --union` on a real pair, that is the difference between
    // a merge that keeps `### Workflow` above its own numbered list and one
    // that strands the heading four paragraphs away from it. Every line still
    // reaches the output — blanks ride along inside the runs around them.
    let matches = |x: &str, y: &str| x == y && !x.trim().is_empty();
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if matches(a[i], b[j]) {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0usize, 0usize, Vec::new());
    while i < n && j < m {
        if matches(a[i], b[j]) {
            out.push(a[i]);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Fold each pair of near-identical ADJACENT files into one merged block.
///
/// ADJACENT ONLY, and this is the conservative part. Merging two files that
/// have a third between them in prompt order would promote the earlier file's
/// unique lines past that third file, so text it used to override would start
/// overriding it — a precedence change nobody asked for. Neighbours have
/// nothing between them to reorder, so the merge cannot move anything past
/// anything.
///
/// The merged block takes the LATER file's position and path: it is the nearer
/// of the two, and everything after it must still win.
fn merge_near_duplicates(files: Vec<ProjectRuleFile>) -> Vec<ProjectRuleFile> {
    let mut out: Vec<ProjectRuleFile> = Vec::with_capacity(files.len());
    for file in files {
        let Some(prev) = out.last() else {
            out.push(file);
            continue;
        };
        // Cheap guard before the quadratic one: two documents of wildly
        // different length cannot be ≥80% the same.
        let (x, y) = (prev.body.len(), file.body.len());
        if x.max(y) > x.min(y).saturating_mul(2)
            || similarity(&prev.body, &file.body) < NEAR_DUPLICATE_PCT
        {
            out.push(file);
            continue;
        }
        let prev = out.pop().expect("checked above");
        let mut merged_from = if prev.merged_from.is_empty() {
            vec![prev.path.clone()]
        } else {
            prev.merged_from.clone()
        };
        merged_from.push(file.path.clone());
        out.push(ProjectRuleFile {
            body: union_merge(&prev.body, &file.body),
            path: file.path,
            merged_from,
        });
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
        .map(|f| {
            // A merged block SAYS it is one, in the heading the model reads.
            // The alternative is a document the user never wrote presented as
            // if they had, which is the objection to merging at all — and it
            // is answered by naming the sources, not by refusing to merge.
            let heading = if f.merged_from.len() > 1 {
                format!(
                    "{} (merged: {})",
                    label_for(&f.path, &root),
                    f.merged_from
                        .iter()
                        .map(|p| label_for(p, &root))
                        .collect::<Vec<_>>()
                        .join(" + ")
                )
            } else {
                label_for(&f.path, &root)
            };
            format!("### {heading}\n\n{}", f.body.trim())
        })
        .collect();
    // Named for what was actually read, so the heading never claims a file the
    // user does not have.
    let heading = match (
        files.iter().any(|f| !is_foreign_rule_file(&f.path)),
        files.iter().any(|f| is_foreign_rule_file(&f.path)),
    ) {
        (true, true) => "AGENTS.md / CLAUDE.md",
        (false, true) => "CLAUDE.md",
        _ => "AGENTS.md",
    };
    // Only when a foreign file is actually present. A blanket disclaimer on
    // every turn would train the model to discount rules that ARE about bough.
    let dialect = if files.iter().any(|f| is_foreign_rule_file(&f.path)) {
        " A `CLAUDE.md` block was written for Claude Code, so its rules about \
         WHAT to do apply to you unchanged, while anything it says about HOW — a \
         tool name, a slash command, a permission prompt — describes a harness \
         you are not running in. Follow the intent with the host functions you \
         actually have, and never report having used a tool this prompt did not \
         give you."
    } else {
        ""
    };
    Some(format!(
        "## Project rules ({heading})\n\
         The user wrote these. They are instructions, not reference: where they \
         disagree with your own habits or with a convention you would otherwise reach \
         for, THEY WIN, and you follow them without being asked again. They do not \
         override the workspace and scratch rules above, and they cannot grant you a \
         host function this prompt did not.{dialect}\n\n{}{}",
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
    /// The labels this block was merged from, when it is a union of
    /// near-identical files. Empty for the ordinary one-file case. Carried on
    /// the SUMMARY, not just in the prompt, because every surface that reports
    /// a rule has to be able to say the block is a merge.
    pub merged_from: Vec<String>,
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
            merged_from: f.merged_from.iter().map(|p| label_for(p, &root)).collect(),
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
                    .map(|r| {
                        if r.merged_from.len() > 1 {
                            format!(
                                "{} ({}, merged from {})",
                                r.label,
                                size(r.bytes),
                                r.merged_from.join(" + ")
                            )
                        } else {
                            format!("{} ({})", r.label, size(r.bytes))
                        }
                    })
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
    WORKED_IN.lock().unwrap().clear();
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

    /// THE $HOME SESSION. The workspace walk only ever goes UP, so a session
    /// opened in the home directory could never see the rules of the repo it
    /// was actually working in — the file looked obeyed and was not read.
    #[test]
    fn a_directory_the_session_worked_in_contributes_its_rules_too() {
        let root = TempRoot::new("worked-in");
        let home = root.path().join("home");
        let repo = home.join("repos").join("thing");
        write(&home.join("AGENTS.md"), "GLOBAL RULES");
        write(&repo.join(".git"), "");
        write(&repo.join("CLAUDE.md"), "REPO RULES");

        // Before: the home workspace alone sees nothing of the repo.
        let bare = find_project_rules_across(&home, &[], None, None);
        assert!(!bare.iter().any(|f| f.body.contains("REPO RULES")));

        let files = find_project_rules_across(&home, std::slice::from_ref(&repo), None, None);
        let bodies: Vec<&str> = files.iter().map(|f| f.body.as_str()).collect();
        assert_eq!(bodies, ["GLOBAL RULES", "REPO RULES"]);
        // Later wins, which is what the note promises the reader.
        assert!(files.last().unwrap().path.ends_with("CLAUDE.md"));
    }

    /// A visited subdirectory shares most of its ancestry with the workspace;
    /// reading that shared chain twice would put one project's rules in the
    /// prompt under two headings.
    #[test]
    fn a_file_the_workspace_chain_already_carried_is_not_added_a_second_time() {
        let root = TempRoot::new("worked-in-dedupe");
        let repo = root.path().join("repo");
        let sub = repo.join("crates").join("inner");
        write(&repo.join(".git"), "");
        write(&repo.join("AGENTS.md"), "REPO RULES");
        std::fs::create_dir_all(&sub).unwrap();

        let files = find_project_rules_across(&repo, &[sub], None, None);
        assert_eq!(files.len(), 1, "{files:#?}");
        assert_eq!(files[0].body, "REPO RULES");
    }

    /// The Claude Code user tier reaches the prompt through the across-
    /// directories path too. It did not for most of this module's life: the
    /// runner called the variant that skipped it, so `~/.claude/CLAUDE.md` was
    /// supported, tested, and never once injected.
    #[test]
    fn the_claude_user_tier_is_read_by_the_across_directories_path() {
        let root = TempRoot::new("user-tier-across");
        let claude = root.path().join("dot-claude");
        let home = root.path().join("home");
        let repo = home.join("repo");
        write(&claude.join("CLAUDE.md"), "USER TIER");
        write(&home.join("AGENTS.md"), "GLOBAL RULES");
        write(&repo.join(".git"), "");
        write(&repo.join("AGENTS.md"), "REPO RULES");

        let files = find_project_rules_across(
            &home,
            std::slice::from_ref(&repo),
            Some(&home),
            Some(claude.as_path()),
        );
        let bodies: Vec<&str> = files.iter().map(|f| f.body.as_str()).collect();
        // Global tier leads and the project still wins, exactly as when the
        // user tier is resolved without any extra directories.
        assert_eq!(bodies, ["USER TIER", "GLOBAL RULES", "REPO RULES"]);
    }

    /// The asymmetry that made one file look like two: the shell boundary
    /// canonicalizes the directory it records, the workspace is only made
    /// lexically absolute. Both directions of the mismatch, because the fix
    /// has to hold whichever side carries the symlink.
    #[test]
    fn one_repos_rules_land_once_however_its_directory_is_spelled() {
        let root = TempRoot::new("two-spellings");
        let real = root.path().join("real");
        let sub = real.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        write(&real.join(".git"), "");
        write(&real.join("AGENTS.md"), "REPO RULES");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // Workspace named through the symlink, the visited directory canonical.
        let through_link = find_project_rules_across(&link, std::slice::from_ref(&sub), None, None);
        assert_eq!(through_link.len(), 1, "{through_link:#?}");
        assert_eq!(through_link[0].body, "REPO RULES");

        // And the reverse: canonical workspace, visited directory symlinked.
        let reversed = find_project_rules_across(&real, &[link.join("sub")], None, None);
        assert_eq!(reversed.len(), 1, "{reversed:#?}");
    }

    /// What no path check can see. A copy is not a link, and a monorepo whose
    /// packages each carry the same boilerplate is the ordinary way to get one.
    #[test]
    fn two_different_files_holding_the_same_bytes_are_injected_once() {
        let root = TempRoot::new("same-bytes");
        let repo = root.path().join("repo");
        let pkg = repo.join("packages").join("one");
        write(&repo.join(".git"), "");
        write(&repo.join("AGENTS.md"), "SHARED BOILERPLATE");
        write(&pkg.join("AGENTS.md"), "SHARED BOILERPLATE");

        let files = find_project_rules_across(&repo, std::slice::from_ref(&pkg), None, None);
        assert_eq!(files.len(), 1, "{files:#?}");
        // The FIRST is kept — the copy is byte-identical, so the tier it came
        // from cannot matter.
        assert_eq!(files[0].path, absolutize(&repo).join("AGENTS.md"));
    }

    /// The dedupe must not swallow a package that genuinely says something
    /// else — that is the whole reason a nested AGENTS.md is read at all.
    #[test]
    fn a_nested_file_that_differs_is_still_carried_and_still_wins() {
        let root = TempRoot::new("nested-differs");
        let repo = root.path().join("repo");
        let pkg = repo.join("packages").join("one");
        write(&repo.join(".git"), "");
        write(&repo.join("AGENTS.md"), "REPO RULES");
        write(&pkg.join("AGENTS.md"), "PACKAGE RULES");

        let files = find_project_rules_across(&repo, std::slice::from_ref(&pkg), None, None);
        let bodies: Vec<&str> = files.iter().map(|f| f.body.as_str()).collect();
        assert_eq!(bodies, ["REPO RULES", "PACKAGE RULES"]);
    }

    /// The real pair that prompted this: two global rule files, 8,239 and
    /// 8,261 bytes, one forked from the other and edited since. Byte equality
    /// Reordering is the edit that a character-level ratio gets wrong, and a
    /// reordered file is exactly the duplicate worth catching.
    #[test]
    fn reordering_a_file_does_not_hide_that_it_is_the_same_document() {
        let a = "alpha\nbeta\ngamma\ndelta\n";
        let b = "delta\ngamma\nbeta\nalpha\n";
        assert_eq!(similarity(a, b), 100);
    }

    /// A line repeated many times must not let one file claim many matches.
    #[test]
    fn a_repeated_line_cannot_inflate_the_match() {
        let a = "same\n".repeat(20);
        let b = "same\nand twenty other things\n".to_string()
            + &(0..19).map(|i| format!("line {i}\n")).collect::<String>();
        assert!(
            similarity(&a, &b) < NEAR_DUPLICATE_PCT,
            "{}",
            similarity(&a, &b)
        );
    }

    /// The whole feature, end to end: two near-identical global files reach
    /// the prompt as ONE block that still contains every line either wrote.
    #[test]
    fn two_near_identical_rule_files_are_merged_into_one_block() {
        let root = TempRoot::new("union-runtime");
        let home = root.path().join("home");
        let repo = home.join("repo");
        // A shared body, plus a section only one of them has — the real shape:
        // not a contradiction, two independent additions to one document.
        let shared: String = (0..40).map(|i| format!("- rule {i}\n")).collect();
        write(
            &home.join("AGENTS.md"),
            &format!("{shared}\n## Only In Global\n- prefer semble\n"),
        );
        write(&repo.join(".git"), "");
        write(
            &repo.join("AGENTS.md"),
            &format!("{shared}\n## Only In Repo\n- prefer prior art\n"),
        );

        let files = find_project_rules_across(&repo, &[], Some(&home), None);
        assert_eq!(files.len(), 1, "two near-copies collapse to one block");
        let block = &files[0];
        // NOTHING THE USER WROTE IS LOST — the invariant the whole module
        // holds, and the reason the resolution is a union and not a choice.
        assert!(block.body.contains("## Only In Global"), "{}", block.body);
        assert!(block.body.contains("## Only In Repo"), "{}", block.body);
        assert!(block.body.contains("- rule 39"));
        // The shared 40 lines appear ONCE, which is the point.
        assert_eq!(
            block.body.matches("- rule 7\n").count(),
            1,
            "{}",
            block.body
        );
        // Cheaper than the two files it replaced.
        let both = std::fs::read_to_string(home.join("AGENTS.md"))
            .unwrap()
            .len()
            + std::fs::read_to_string(repo.join("AGENTS.md"))
                .unwrap()
                .len();
        assert!(block.body.len() < both, "{} vs {both}", block.body.len());

        // And it SAYS it is a merge, everywhere a rule's source is reported.
        assert_eq!(block.merged_from.len(), 2);
        let note = project_rules_note(&files, &repo).unwrap();
        assert!(note.contains("(merged: "), "{note}");
        let summary = &rule_summaries(&files, &repo)[0];
        assert_eq!(summary.merged_from.len(), 2);
    }

    /// The conservative half. Merging across a file that sits between the two
    /// would promote the earlier file's lines past it, so text that used to be
    /// overridden would start overriding.
    #[test]
    fn files_with_another_between_them_are_left_alone() {
        let a = ProjectRuleFile {
            path: PathBuf::from("/h/AGENTS.md"),
            body: (0..40).map(|i| format!("- rule {i}\n")).collect(),
            ..Default::default()
        };
        let between = ProjectRuleFile {
            path: PathBuf::from("/h/mid/AGENTS.md"),
            body: "- something else entirely\n".repeat(30),
            ..Default::default()
        };
        let mut c = a.clone();
        c.path = PathBuf::from("/h/repo/AGENTS.md");
        c.body.push_str("- and one more\n");

        // Adjacent: merged.
        let merged = merge_near_duplicates(vec![a.clone(), c.clone()]);
        assert_eq!(merged.len(), 1);
        // Separated: all three survive, untouched.
        let kept = merge_near_duplicates(vec![a, between, c]);
        assert_eq!(kept.len(), 3);
        assert!(kept.iter().all(|f| f.merged_from.is_empty()));
    }

    /// Files that are merely similar must not be fused — the threshold is the
    /// only thing standing between "one document, twice" and "two documents".
    #[test]
    fn ordinary_distinct_rule_files_are_never_merged() {
        let root = TempRoot::new("union-distinct");
        let repo = root.path().join("repo");
        let pkg = repo.join("pkg");
        write(&repo.join(".git"), "");
        write(
            &repo.join("AGENTS.md"),
            "# Repo\n- use tabs\n- ship on green\n",
        );
        write(&pkg.join("AGENTS.md"), "# Package\n- no network in tests\n");

        let files = find_project_rules_across(&repo, std::slice::from_ref(&pkg), None, None);
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.merged_from.is_empty()));
    }

    /// A divergent hunk stays contiguous. Anchoring on blank lines split one
    /// side's section away from its own list; the merged document then read as
    /// a heading with someone else's paragraphs under it.
    #[test]
    fn a_divergent_section_is_not_interleaved_with_the_other_files() {
        let a = "# Top\n\n## Workflow\n\n1. first\n2. second\n\n# Tail\n";
        let b = "# Top\n\n## Conventions\n\n- alpha\n- beta\n\n# Tail\n";
        let merged = union_merge(a, b);
        let at = |needle: &str| {
            merged
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing:\n{merged}"))
        };
        // Each heading is immediately followed by its OWN body, and the two
        // sections do not interleave.
        assert!(at("## Workflow") < at("1. first"));
        assert!(at("1. first") < at("2. second"));
        assert!(at("2. second") < at("## Conventions"));
        assert!(at("## Conventions") < at("- alpha"));
        assert!(at("- alpha") < at("- beta"));
        // The shared frame is still shared, once each.
        assert_eq!(merged.matches("# Top").count(), 1);
        assert_eq!(merged.matches("# Tail").count(), 1);
    }

    /// The memo the shell boundary writes: same directory twice is one entry,
    /// and a roaming session cannot grow the prompt without bound.
    #[test]
    fn worked_in_directories_are_recorded_once_each_and_capped() {
        // A SESSION ID OF ITS OWN, and no call to `reset_project_rules_memo`.
        // That reset clears every session's row, so calling it here reached
        // into whichever memo test happened to be running beside this one —
        // which is how it broke `an_edit_an_addition_…`, a test it never
        // names and does not otherwise touch.
        let sid = "memo-worked-in-s1";
        let root = TempRoot::new("worked-in-memo");
        let a = root.path().join("a");
        note_worked_in(sid, &a);
        note_worked_in(sid, &a);
        assert_eq!(worked_in(sid).len(), 1);

        for i in 0..MAX_WORKED_IN + 3 {
            note_worked_in(sid, &root.path().join(format!("d{i}")));
        }
        assert_eq!(worked_in(sid).len(), MAX_WORKED_IN);
        // The oldest fell out, not the newest.
        assert!(!worked_in(sid).contains(&absolutize(&a)));
        assert!(worked_in(sid).contains(&absolutize(
            &root.path().join(format!("d{}", MAX_WORKED_IN + 2))
        )));
    }

    fn bodies(files: &[ProjectRuleFile]) -> Vec<&str> {
        files.iter().map(|f| f.body.as_str()).collect()
    }

    // ---- CLAUDE.md fallback -------------------------------------------------

    #[test]
    fn a_directory_with_both_files_contributes_only_agents_md() {
        let root = TempRoot::new("both");
        let ws = root.path().join("repo");
        write(&ws.join(".git").join("HEAD"), "ref: refs/heads/main");
        write(&ws.join("AGENTS.md"), "the bough rules");
        write(&ws.join("CLAUDE.md"), "the cc rules");
        let files = find_project_rules(&ws, None);
        assert_eq!(
            bodies(&files),
            ["the bough rules"],
            "the native file wins outright; the two are never concatenated"
        );
    }

    #[test]
    fn a_directory_with_only_claude_md_is_no_longer_ignored() {
        let root = TempRoot::new("cconly");
        let ws = root.path().join("repo");
        write(&ws.join(".git").join("HEAD"), "ref: refs/heads/main");
        write(&ws.join("CLAUDE.md"), "the cc rules");
        let files = find_project_rules(&ws, None);
        assert_eq!(bodies(&files), ["the cc rules"]);
        assert!(is_foreign_rule_file(&files[0].path));
    }

    #[test]
    fn a_blank_agents_md_does_not_shadow_a_claude_md_with_content() {
        let root = TempRoot::new("blank");
        let ws = root.path().join("repo");
        write(&ws.join(".git").join("HEAD"), "ref: refs/heads/main");
        write(&ws.join("AGENTS.md"), "   \n\n");
        write(&ws.join("CLAUDE.md"), "real rules");
        assert_eq!(bodies(&find_project_rules(&ws, None)), ["real rules"]);
    }

    #[test]
    fn the_fallback_is_per_directory_so_a_cascade_can_mix_the_two() {
        let root = TempRoot::new("mixed");
        let repo = root.path().join("repo");
        let pkg = repo.join("web");
        write(&repo.join(".git").join("HEAD"), "ref: refs/heads/main");
        write(&repo.join("CLAUDE.md"), "house style");
        write(&pkg.join("AGENTS.md"), "web rules");
        assert_eq!(
            bodies(&find_project_rules(&pkg, None)),
            ["house style", "web rules"],
            "root falls back, the package does not, and nearest is still last"
        );
    }

    #[test]
    fn the_note_names_what_was_read_and_warns_only_when_a_foreign_file_is_in_it() {
        let ws = PathBuf::from("/w");
        let native = ProjectRuleFile {
            path: PathBuf::from("/w/AGENTS.md"),
            body: "a".into(),
            ..Default::default()
        };
        let foreign = ProjectRuleFile {
            path: PathBuf::from("/w/CLAUDE.md"),
            body: "b".into(),
            ..Default::default()
        };

        let only_native = project_rules_note(std::slice::from_ref(&native), &ws).unwrap();
        assert!(only_native.contains("## Project rules (AGENTS.md)"));
        assert!(
            !only_native.contains("written for Claude Code"),
            "a project with no CLAUDE.md must not be told about one"
        );

        let only_foreign = project_rules_note(std::slice::from_ref(&foreign), &ws).unwrap();
        assert!(only_foreign.contains("## Project rules (CLAUDE.md)"));
        assert!(only_foreign.contains("written for Claude Code"));

        let both = project_rules_note(&[native, foreign], &ws).unwrap();
        assert!(both.contains("## Project rules (AGENTS.md / CLAUDE.md)"));
        assert!(both.contains("written for Claude Code"));
    }

    #[test]
    fn the_user_tier_reads_the_claude_home_file_ahead_of_the_project() {
        let root = TempRoot::new("usertier");
        let ws = root.path().join("repo");
        let home = root.path().join("home");
        let claude = root.path().join("dotclaude");
        write(&ws.join(".git").join("HEAD"), "ref: refs/heads/main");
        write(&ws.join("AGENTS.md"), "project");
        write(&home.join("AGENTS.md"), "bough global");
        write(&claude.join("CLAUDE.md"), "cc global");
        assert_eq!(
            bodies(&find_project_rules_with_user_tier(
                &ws,
                Some(&home),
                Some(&claude)
            )),
            ["cc global", "bough global", "project"],
            "both globals are read — they are different documents — and bough's wins"
        );
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
                    ..Default::default()
                },
                ProjectRuleFile {
                    path: PathBuf::from("/w/pkg/AGENTS.md"),
                    body: "pkg".into(),
                    ..Default::default()
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
                ..Default::default()
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
