//! Tag normalization + command recording (port of `src/history/record.ts`):
//! normalize/reference grammar, attribution dir-scan caps, one-transaction
//! insert via `Db::record_command`.
//!
//! The design bet: the model labels its own INTENT at generation time
//! (`bash(cmd, "psql:migrate")`), which is nearly free and far more accurate
//! than post-hoc clustering of command strings — the tag is the stable join
//! key across sessions, the exit code is the ground truth that weights it.
//!
//! Everything here is best-effort and MUST NEVER surface a failure into a
//! turn: a broken git checkout, a locked database, or a weird command string
//! loses one memory row, not the round.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::types::{
    system_clock, Clock, CommandRecord, CommandRecorder, RecordedCommand, SharedDb,
};

use super::hygiene::clean_tags;

/// How much printed output one history row keeps inline.
pub const OUTPUT_HEAD_CHARS: usize = 2_000;

/// The spill file a bounded output points at, parsed back out of the marker
/// (`hostfn/spill.rs`'s `spill_marker`). The marker travels INSIDE the text
/// the program saw, so parsing it here spares the spill module a second
/// return channel it would only ever grow for this one consumer.
pub fn spill_path_from(output: &str) -> Option<String> {
    let re = spill_path_re();
    re.captures(output).map(|c| c[1].to_string())
}

fn spill_path_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"FULL OUTPUT SAVED[^\n]*\n\s+(\S+)\n").unwrap())
}

/// Tags the model may write: short lowercase slugs, colon-separated.
const MAX_TAGS: usize = 8;

/// How far back "already a word here" reaches. Matches the priming note's
/// lookback (`history/tags/stats.rs`) on purpose: hygiene should judge a tag
/// against the same vocabulary the model is being primed with, or it would
/// drop words the note is still recommending.
const VOCAB_LOOKBACK_MS: i64 = 150 * 24 * 60 * 60 * 1000;

/// A REFERENCE: `namespace.id`, pointing at something with an identity outside
/// bough — `linear.eng-1234`, `pr.456`, `commit.3c1c78e`. The dot is the whole
/// rule; dashes and slashes survive INSIDE a reference and nowhere else.
/// `ENG-1234` written bare still becomes `eng:1234`, because without a
/// namespace there is nothing to tell an identifier from a hyphenated phrase,
/// and `repo-inspect` must keep splitting.
fn is_ref_piece(piece: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[a-z][a-z0-9]*\.[a-z0-9][a-z0-9._/-]*$").unwrap())
        .is_match(piece)
}

/// Is this normalized tag a reference rather than a coined word?
pub fn is_ref(tag: &str) -> bool {
    tag.contains('.')
}

/// Normalize a model-written tag string: lowercase, split into tags, slugify
/// each part, drop empties, cap the count. Returns `""` when nothing survives
/// — which the caller treats as "no tags given".
///
/// Normalization is what makes a folksonomy converge: `PSQL:Migrate` and
/// `psql:migrate` must be the same tag or the popularity stats fragment.
/// Dashes and whitespace are SEPARATORS, not tag characters. A reference
/// (`namespace.id`) is the one exception and passes through whole.
pub fn normalize_tags(raw: Option<&str>) -> String {
    let raw = match raw {
        Some(r) if !r.is_empty() => r,
        _ => return String::new(),
    };
    let mut out: Vec<String> = Vec::new();
    let lower = raw.to_lowercase();
    // Split on colons and whitespace FIRST, so a reference is still whole when
    // it is tested — splitting on dashes up front would have shredded it.
    for piece in lower.split(|c: char| c == ':' || c.is_whitespace()) {
        if piece.is_empty() {
            continue;
        }
        if is_ref_piece(piece) {
            out.push(piece.to_string());
            continue;
        }
        for part in piece.split('-') {
            let tag: String = part
                .chars()
                .filter(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '.'))
                .collect();
            // At least one letter or digit: `...` survives the character
            // filter (dots are legal in a tag) and would then read as a
            // reference, which it is not.
            //
            // NO BARE-NUMBER RULE HERE: `ENG-1234` written without a
            // namespace normalizes to `eng:1234`, and the number is the half
            // that identifies the ticket — `bough tags show 1234` is exactly
            // how a bare-written reference is found again.
            if tag.chars().any(|c| c.is_ascii_alphanumeric()) {
                out.push(tag);
            }
        }
    }
    out.truncate(MAX_TAGS);
    out.join(":")
}

/// The individual tags of a normalized string, deduped; `[]` for `""`.
pub fn split_tags(tags: &str) -> Vec<String> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for t in tags.split(':') {
        if seen.insert(t) {
            out.push(t.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Repo identity
// ---------------------------------------------------------------------------

fn repo_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `git config --get remote.origin.url` with a 2s wall clock, or None. A
/// bounded poll rather than a plain `output()` so a hostile checkout (a hung
/// credential helper, a fuse mount) cannot wedge the turn path.
fn git_origin_url(workspace: &str) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .args(["-C", workspace, "config", "--get", "remote.origin.url"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2_000);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                let url = out.trim().to_string();
                return if url.is_empty() { None } else { Some(url) };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

/// The scope key for a workspace's command history: the git remote origin URL
/// when there is one, else the workspace root path.
///
/// The URL rather than the path because paths are fragile — the same project
/// moved, re-cloned, or checked out on another machine should keep its tag
/// profile. Cached per workspace for the process lifetime; a mid-session
/// `git remote set-url` is not a case worth a subprocess per command.
pub fn repo_identity(workspace: &str) -> String {
    if let Some(hit) = repo_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(workspace).cloned())
    {
        return hit;
    }
    let repo = git_origin_url(workspace).unwrap_or_else(|| workspace.to_string());
    if let Ok(mut c) = repo_cache().lock() {
        c.insert(workspace.to_string(), repo.clone());
    }
    repo
}

// ---------------------------------------------------------------------------
// Directory + repo attribution
// ---------------------------------------------------------------------------

const MAX_TOKENS_CHECKED: usize = 24;
const MAX_DIRS: usize = 4;

/// Lexical resolve: absolute stays; relative joins onto `base`; `.`/`..`
/// components normalize away (the TS `resolve` behavior, minus symlinks —
/// attribution is lexical by design, like `paths::confine`).
fn lexical_resolve(base: &str, tok: &str) -> String {
    let raw = if Path::new(tok).is_absolute() {
        PathBuf::from(tok)
    } else {
        Path::new(base).join(tok)
    };
    let mut out = PathBuf::new();
    for c in raw.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

/// The ABSOLUTE directories a command was about.
///
/// Not the cwd: a bough program runs at the workspace root and never cds, so
/// cwd carries no per-directory signal. Instead, tokens that resolve to real
/// paths attribute the command to their directories — `bun test
/// src/tui/x.test.ts` → `<ws>/src/tui`. Absolute tokens OUTSIDE the
/// workspace count too: a session rooted at `~` that runs `cd ~/repos/bough
/// && …` is working on that repo, and dropping the path was exactly how such
/// commands got mis-scoped to `~`.
fn extract_abs_dirs(command: &str, workspace: &str) -> Vec<String> {
    static LINE_REF: OnceLock<regex::Regex> = OnceLock::new();
    static NAME_EXT: OnceLock<regex::Regex> = OnceLock::new();
    let line_ref = LINE_REF.get_or_init(|| regex::Regex::new(r":\d+(:\d+)?$").unwrap());
    let name_ext = NAME_EXT.get_or_init(|| regex::Regex::new(r"^[^./]+\.[^./]+$").unwrap());

    let mut dirs: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let tokens: Vec<&str> = command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')'))
        .filter(|t| !t.is_empty())
        .take(200)
        .collect();
    for raw_token in tokens {
        if dirs.len() >= MAX_DIRS || checked >= MAX_TOKENS_CHECKED {
            break;
        }
        let mut tok = raw_token
            .trim_start_matches(['\'', '"', '`'])
            .trim_end_matches(['\'', '"', '`', ','])
            .to_string();
        // `--output=path/x` and `FOO=path/x` both carry the path after `=`.
        if let Some(eq) = tok.find('=') {
            tok = tok[eq + 1..].to_string();
        }
        // Line refs (`src/a.ts:12`) resolve after stripping the suffix.
        tok = line_ref.replace(&tok, "").into_owned();
        // Tokens that are clearly not paths, cheaply: too short, flag-shaped,
        // or all digits.
        if tok.chars().count() < 2
            || tok.starts_with('-')
            || tok.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        // Only path-looking tokens are worth a stat: containing a separator,
        // or a dotted filename. Bare words (`git`, `push`) are commands.
        if !tok.contains('/') && !name_ext.is_match(&tok) {
            continue;
        }
        if tok.contains("://") {
            continue;
        }
        let full = lexical_resolve(workspace, &tok);
        if full.contains("/node_modules") || full.contains("/.git") {
            continue;
        }
        checked += 1;
        let Ok(st) = std::fs::metadata(&full) else {
            continue;
        };
        let dir = if st.is_dir() {
            full
        } else {
            Path::new(&full)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(full)
        };
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

fn git_root_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The enclosing git checkout's root, or None. Walks up ≤32 levels stat-ing
/// `.git` (a directory in a normal clone, a file in a worktree — either
/// counts); cached per starting directory for the process lifetime.
pub fn find_git_root(dir: &str) -> Option<String> {
    if let Some(hit) = git_root_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(dir).cloned())
    {
        return hit;
    }
    let mut cur = dir.to_string();
    let mut found: Option<String> = None;
    for _ in 0..32 {
        if std::fs::metadata(Path::new(&cur).join(".git")).is_ok() {
            found = Some(cur);
            break;
        }
        let parent = Path::new(&cur)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| cur.clone());
        if parent == cur {
            break;
        }
        cur = parent;
    }
    if let Ok(mut c) = git_root_cache().lock() {
        c.insert(dir.to_string(), found.clone());
    }
    found
}

/// What one command resolves to: its memory scope and the dirs inside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Attribution {
    /// The repo identity the history row is scoped to.
    pub repo: String,
    /// Directories relative to that repo's root (or the workspace, without one).
    pub rel_dirs: Vec<String>,
    /// The absolute dirs the command touched — the hint trigger's input.
    pub abs_dirs: Vec<String>,
}

/// Resolve a command's memory scope from the paths it TOUCHES, not from where
/// the session sits. Each touched directory is mapped to its enclosing git
/// checkout; the checkout containing the most touched dirs wins and the
/// command is scoped to ITS identity, with dirs relative to its root. A
/// session rooted at `~` inspecting `~/repos/bough` therefore writes rows
/// other sessions rooted IN that repo can recall — the miss that motivated
/// this function.
///
/// A command touching nothing (or nothing inside any checkout) falls back to
/// the workspace's own scope, which is the common case and the cheap path.
pub fn attribute_command(command: &str, workspace: &str) -> Attribution {
    let abs_dirs = extract_abs_dirs(command, workspace);
    let ws_root = find_git_root(workspace).unwrap_or_else(|| workspace.to_string());
    let mut order: Vec<String> = Vec::new();
    let mut by_root: HashMap<String, Vec<String>> = HashMap::new();
    for d in &abs_dirs {
        let root = find_git_root(d).unwrap_or_else(|| ws_root.clone());
        if !by_root.contains_key(&root) {
            order.push(root.clone());
        }
        by_root.entry(root).or_default().push(d.clone());
    }
    let mut root = ws_root.clone();
    let mut best = by_root.get(&ws_root).map_or(0, Vec::len);
    for r in &order {
        let n = by_root[r].len();
        if n > best {
            root = r.clone();
            best = n;
        }
    }
    let mut rel_dirs: Vec<String> = Vec::new();
    for d in by_root.get(&root).map(Vec::as_slice).unwrap_or_default() {
        // Outside the root (`..`-escaping in TS terms) or the root itself
        // never becomes a rel dir.
        let Ok(rel) = Path::new(d).strip_prefix(&root) else {
            continue;
        };
        let rel = rel.to_string_lossy().into_owned();
        if rel.is_empty() || rel == "." || Path::new(&rel).is_absolute() {
            continue;
        }
        if !rel_dirs.contains(&rel) {
            rel_dirs.push(rel);
        }
    }
    Attribution {
        repo: repo_identity(&root),
        rel_dirs,
        abs_dirs,
    }
}

// ---------------------------------------------------------------------------
// The recorder
// ---------------------------------------------------------------------------

/// What the recorder needs from the turn. Structural subset of `TurnCtx`.
pub struct RecorderCtx {
    pub db: SharedDb,
    pub session_id: String,
    pub workspace: String,
    /// The turn's supervisor message — the one whose `run_steps` program is
    /// running this command. Stamped on every row so recall reaches the
    /// program, not just the incantation. Optional because a caller without a
    /// turn (a test) has no message, and a row without the link is still a
    /// memory.
    pub message_id: Option<String>,
    pub now: Option<Clock>,
    /// Where the absolute dirs each command touched are appended — the
    /// trigger input for the round's directory hints (`turn/runner.rs`), so
    /// hints fire on shell exploration too, not only on `view()` reads.
    pub touched: Option<Arc<Mutex<Vec<String>>>>,
}

/// Build the per-turn recorder the shell verbs call — see `ShellCtx::record`.
/// Every failure is swallowed: memory is a side channel, never a turn hazard.
/// Does this command only READ OR MAINTAIN the memory?
///
/// THE LOOP THIS CLOSES, measured on a real install: of 748 commands recorded
/// in a few days, 271 were bough talking to itself — and per tag it was worse.
/// 53 of the 54 commands tagged `notion` were `bough notes show notion`; 39 of
/// 40 for `slack`; 69 of 72 for `history`. The "this repo has worked on that
/// before" hint had become a tour of bough's own CLI.
///
/// It is self-reinforcing, which is why filtering at read time is not enough:
/// a recall is recorded as work, which lifts the tag's weight, which raises it
/// in the priming note and the hints, which prompts another recall. Every turn
/// of that loop carries zero information about the project.
///
/// WRITES TOO, not only reads. `bough notes write` was originally recorded on
/// the argument that "the memory records its own maintenance" — a nicer
/// sentence than it was a rule. Bookkeeping about the work is not the work.
///
/// Deliberately narrow: `bough patterns` reads a real log and `bough mcp`
/// changes real configuration, so both stay recorded. Only the two verbs whose
/// entire subject is the memory are skipped.
pub fn is_memory_command(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    // Leading `VAR=value` assignments, as in the observed
    // `PATH=…/target/release:$PATH bough notes show nimbus`.
    // An assignment is `NAME=…` where NAME is a shell identifier. Testing for
    // "contains = but no /" fails on the very form this was written for:
    // `PATH=/Users/…/target/release:$PATH`, whose value is nothing but slashes.
    fn is_assignment(token: &str) -> bool {
        let Some((name, _)) = token.split_once('=') else {
            return false;
        };
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    }
    let binary = loop {
        match tokens.next() {
            None => return false,
            Some(t) if is_assignment(t) => continue,
            Some(t) => break t,
        }
    };
    // Any path prefix: `bough`, `./scripts/bough`, `~/.local/bin/bough`.
    let name = binary.rsplit('/').next().unwrap_or(binary);
    if name != "bough" {
        return false;
    }
    matches!(tokens.next(), Some("tags") | Some("notes"))
}

pub fn create_command_recorder(ctx: RecorderCtx) -> CommandRecorder {
    let now: Clock = ctx.now.clone().unwrap_or_else(system_clock);
    // One vocabulary read per repo per turn. The set a turn is judged against
    // must also be STABLE across that turn — a word coined by the first
    // command would otherwise be established vocabulary by the third, and the
    // same tag would be dropped or kept depending on where in the program it
    // happened to run.
    let vocab_by_repo: Arc<Mutex<HashMap<String, HashMap<String, i64>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    Arc::new(move |e: RecordedCommand| {
        // A lost memory row is strictly better than a broken round: every
        // early return below is the TS `catch {}`.
        //
        // FIRST, before anything is attributed or counted: a command that only
        // reads the memory is not project work, and recording it is what let
        // recall colonise the vocabulary it recalls from.
        if is_memory_command(&e.command) {
            return;
        }
        let att = attribute_command(&e.command, &ctx.workspace);
        if let Some(touched) = &ctx.touched {
            if let Ok(mut t) = touched.lock() {
                t.extend(att.abs_dirs.iter().cloned());
            }
        }
        let ts = now();
        let vocab: HashMap<String, i64> = {
            let Ok(mut map) = vocab_by_repo.lock() else {
                return;
            };
            match map.get(&att.repo) {
                Some(v) => v.clone(),
                None => {
                    let Ok(db) = ctx.db.lock() else { return };
                    let Ok(v) = db.repo_tag_counts(&att.repo, ts - VOCAB_LOOKBACK_MS) else {
                        return;
                    };
                    map.insert(att.repo.clone(), v.clone());
                    v
                }
            }
        };
        let tag_list = clean_tags(&split_tags(&e.tags), &e.command, &vocab);
        let record = CommandRecord {
            session_id: ctx.session_id.clone(),
            ts,
            repo: att.repo,
            cmd: e.command,
            tags: tag_list.join(":"),
            tag_list,
            dirs: att.rel_dirs,
            exit_code: e.exit_code,
            duration_ms: e.duration_ms,
            output_head: take_chars(&e.output_head, OUTPUT_HEAD_CHARS),
            spill_path: e.spill_path,
            source: "live".to_string(),
            message_id: ctx.message_id.clone(),
        };
        let Ok(db) = ctx.db.lock() else { return };
        let _ = db.record_command(&record);
    })
}

/// The first `n` chars (not bytes) of a string — the TS `.slice(0, n)`.
fn take_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/history/record.test.ts. `extract_abs_dirs` runs
// against a real temp directory: the whole heuristic is "does this token
// resolve to something on disk", and faking the disk would test the fake.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {

    #[test]
    fn a_command_that_only_reads_the_memory_is_not_project_work() {
        // Measured: 53 of 54 commands tagged `notion` on a real install were
        // `bough notes show notion`. Recall was being recorded as work, which
        // lifted the tag, which prompted more recall.
        for cmd in [
            "bough tags",
            "bough tags show atlas",
            "bough tags sql \"SELECT 1\"",
            "bough notes",
            "bough notes show nimbus",
            "bough notes write atlas",
            "bough notes append pr.1 x",
            // Every spelling seen in the wild.
            "PATH=$HOME/repos/bough/target/release:$PATH bough notes show nimbus",
            "./scripts/bough notes show x",
            "~/.local/bin/bough tags show git",
            "/usr/local/bin/bough notes stale",
            "FOO=1 BAR=2 bough tags",
        ] {
            assert!(is_memory_command(cmd), "should be skipped: {cmd}");
        }
    }

    #[test]
    fn real_work_is_still_recorded_including_boughs_other_verbs() {
        for cmd in [
            "git status",
            "kubectl -n atlas rollout status deploy/executor",
            // Narrow on purpose: `patterns` reads a real log, `mcp` changes
            // real configuration. Only the two verbs whose whole subject is
            // the memory are skipped.
            "bough patterns server.log",
            "bough mcp doctor",
            "bough exec \"do a thing\"",
            // A command that merely MENTIONS the memory is work.
            "rg 'bough notes' src/",
            "echo bough tags",
            // Not our binary.
            "notbough tags",
            "",
        ] {
            assert!(!is_memory_command(cmd), "should be recorded: {cmd}");
        }
    }
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{Session, SessionKind};
    use crate::types::CommandTagOpts;

    #[test]
    fn normalize_lowercases_trims_slugifies_and_rejoins() {
        assert_eq!(normalize_tags(Some("PSQL:Migrate")), "psql:migrate");
        assert_eq!(normalize_tags(Some(" Git : PUSH ")), "git:push");
        assert_eq!(normalize_tags(Some("a!!:b c:d.e")), "a:b:c:d.e");
    }

    #[test]
    fn dashes_and_spaces_are_separators_no_tag_ever_contains_a_dash() {
        assert_eq!(normalize_tags(Some("repo-inspect")), "repo:inspect");
        assert_eq!(normalize_tags(Some("git push")), "git:push");
        assert_eq!(normalize_tags(Some("bun--test")), "bun:test");
        assert_eq!(normalize_tags(Some("pre-commit-hook")), "pre:commit:hook");
    }

    #[test]
    fn a_dot_makes_a_reference_and_only_there_do_dashes_survive() {
        // The one exception to "dashes are separators", and the rule is the
        // dot: a namespace says the thing has an identity outside bough, so
        // bough does not get to reformat the id.
        assert_eq!(
            normalize_tags(Some("git:push:linear.ENG-1234")),
            "git:push:linear.eng-1234"
        );
        assert_eq!(normalize_tags(Some("pr.456")), "pr.456");
        assert_eq!(normalize_tags(Some("commit.3c1c78e")), "commit.3c1c78e");
        // A branch name keeps its slashes, because half of one points at nothing.
        assert_eq!(
            normalize_tags(Some("branch.claude/tags-history-db-docs")),
            "branch.claude/tags-history-db-docs"
        );

        // WITHOUT a namespace it is still a hyphenated phrase.
        assert_eq!(normalize_tags(Some("ENG-1234")), "eng:1234");
        assert_eq!(normalize_tags(Some("repo-inspect")), "repo:inspect");

        // `is_ref` is what the ranking and the stats split on.
        assert!(is_ref("linear.eng-1234"));
        assert!(!is_ref("composer"));

        // Punctuation alone is not a reference. Dots are legal tag
        // characters, so `...` used to survive the filter and would now read
        // as one.
        assert_eq!(normalize_tags(Some("...")), "");
        assert_eq!(normalize_tags(Some("--")), "");
    }

    #[test]
    fn normalize_returns_empty_when_nothing_survives_the_callers_no_tags_signal() {
        assert_eq!(normalize_tags(None), "");
        assert_eq!(normalize_tags(Some("")), "");
        assert_eq!(normalize_tags(Some("  ")), "");
        assert_eq!(normalize_tags(Some(":::")), "");
        assert_eq!(normalize_tags(Some("!!!:???")), "");
    }

    #[test]
    fn normalize_caps_the_count() {
        assert_eq!(
            normalize_tags(Some("a:b:c:d:e:f:g:h:i:j")),
            "a:b:c:d:e:f:g:h"
        );
    }

    #[test]
    fn split_tags_dedupes_and_treats_empty_as_no_tags() {
        assert_eq!(split_tags(""), Vec::<String>::new());
        assert_eq!(split_tags("a:b:a"), ["a", "b"]);
    }

    #[test]
    fn spill_path_is_parsed_back_out_of_the_marker() {
        let marker = "head\n[… 1 chars omitted from the middle. FULL OUTPUT SAVED — 30,000 chars:\n   /tmp/s/bash-001.log\n   rg -n 'error|fail' '/tmp/s/bash-001.log'\n…]\ntail";
        assert_eq!(
            spill_path_from(marker).as_deref(),
            Some("/tmp/s/bash-001.log")
        );
        assert_eq!(spill_path_from("plain output"), None);
    }

    // ---- attribution (real temp dirs) --------------------------------------

    fn with_workspace(f: impl FnOnce(&str)) {
        let ws = std::env::temp_dir().join(format!("bough-hist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(ws.join("src/tui")).unwrap();
        std::fs::create_dir_all(ws.join("migrations")).unwrap();
        std::fs::create_dir_all(ws.join("node_modules/pkg")).unwrap();
        std::fs::write(ws.join("src/tui/composer.ts"), "x").unwrap();
        std::fs::write(ws.join("migrations/004.sql"), "x").unwrap();
        f(&ws.to_string_lossy());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn attribute_command_maps_a_command_to_the_dirs_of_the_paths_it_names() {
        with_workspace(|ws| {
            assert_eq!(
                attribute_command("bun test src/tui/composer.ts", ws).rel_dirs,
                ["src/tui"]
            );
            assert_eq!(
                attribute_command("psql -f migrations/004.sql", ws).rel_dirs,
                ["migrations"]
            );
            // A directory token attributes to itself; a file to its dirname.
            assert_eq!(
                attribute_command("ls -la src/tui", ws).rel_dirs,
                ["src/tui"]
            );
            // `--flag=path` and line refs both resolve.
            assert_eq!(
                attribute_command("tool --input=src/tui/composer.ts", ws).rel_dirs,
                ["src/tui"]
            );
            assert_eq!(
                attribute_command("rg -n foo src/tui/composer.ts:12", ws).rel_dirs,
                ["src/tui"]
            );
            // A non-git workspace's scope is its own path.
            assert_eq!(attribute_command("ls -la src/tui", ws).repo, ws);
        });
    }

    #[test]
    fn attribute_command_ignores_non_paths_and_the_trees_nobody_means() {
        with_workspace(|ws| {
            assert_eq!(
                attribute_command("git push origin main", ws).rel_dirs,
                Vec::<String>::new()
            );
            assert_eq!(
                attribute_command("curl https://example.com/a/b", ws).rel_dirs,
                Vec::<String>::new()
            );
            // Outside every checkout and outside the workspace: touch-tracked,
            // never a rel dir.
            assert_eq!(
                attribute_command("cat /etc/hosts", ws).rel_dirs,
                Vec::<String>::new()
            );
            assert_eq!(
                attribute_command("ls node_modules/pkg", ws).rel_dirs,
                Vec::<String>::new()
            );
            // A path that does not exist attributes nothing — the heuristic
            // never guesses.
            assert_eq!(
                attribute_command("bun test src/gone/nope.ts", ws).rel_dirs,
                Vec::<String>::new()
            );
        });
    }

    #[test]
    fn attribute_command_dedupes_and_caps() {
        with_workspace(|ws| {
            assert_eq!(
                attribute_command(
                    "diff src/tui/composer.ts src/tui/composer.ts migrations/004.sql",
                    ws
                )
                .rel_dirs,
                ["src/tui", "migrations"]
            );
        });
    }

    #[test]
    fn a_command_about_another_checkout_is_scoped_to_that_checkout_not_the_workspace() {
        with_workspace(|ws| {
            // `ws` plays the home dir; a separate checkout lives at
            // ws/repos/proj.
            let proj = Path::new(ws).join("repos/proj");
            std::fs::create_dir_all(proj.join(".git")).unwrap();
            std::fs::create_dir_all(proj.join("src")).unwrap();
            std::fs::write(proj.join("src/a.ts"), "x").unwrap();
            let proj = proj.to_string_lossy().into_owned();
            // Touching the checkout's root scopes the row to it, with no dir
            // rows.
            let at_root = attribute_command(&format!("cd {proj} && ls -la"), ws);
            assert_eq!(at_root.repo, proj);
            assert_eq!(at_root.rel_dirs, Vec::<String>::new());
            assert_eq!(at_root.abs_dirs, std::slice::from_ref(&proj));
            // Touching a file inside it attributes REPO-ROOT-relative dirs,
            // so sessions rooted anywhere agree on what "src" means.
            let inside = attribute_command(&format!("sed -n 1p {proj}/src/a.ts"), ws);
            assert_eq!(inside.repo, proj);
            assert_eq!(inside.rel_dirs, ["src"]);
        });
    }

    // ---- repo identity ------------------------------------------------------

    #[test]
    fn repo_identity_is_the_origin_url_in_a_git_checkout_else_the_path() {
        let ws = std::env::temp_dir().join(format!("bough-repoid-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        let ws_s = ws.to_string_lossy().into_owned();
        // Non-git: the path is the identity.
        assert_eq!(repo_identity(&ws_s), ws_s);
        // The cache is per-workspace, so a second dir sees fresh state.
        let git = std::env::temp_dir().join(format!("bough-repoid-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&git).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&git)
                .output()
                .expect("git runs");
        };
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", "https://example.com/me/repo.git"]);
        assert_eq!(
            repo_identity(&git.to_string_lossy()),
            "https://example.com/me/repo.git"
        );
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&git);
    }

    // ---- the recorder -------------------------------------------------------

    fn mem_db() -> SharedDb {
        Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ))
    }

    fn make_session(db: &SharedDb, id: &str) {
        db.lock()
            .unwrap()
            .create_session(Session {
                id: id.to_string(),
                title: id.to_string(),
                kind: SessionKind::Root,
                created_at: 1,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: None,
                origin_dir: None,
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
    }

    fn finished(command: &str, tags: &str) -> RecordedCommand {
        RecordedCommand {
            command: command.to_string(),
            tags: tags.to_string(),
            exit_code: Some(0),
            duration_ms: Some(7),
            output_head: "ok".to_string(),
            spill_path: None,
        }
    }

    #[test]
    fn the_recorder_writes_a_full_row_and_never_throws_for_a_broken_db() {
        let ws = std::env::temp_dir().join(format!("bough-rec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/a.ts"), "x").unwrap();
        let ws_s = ws.to_string_lossy().into_owned();

        let db = mem_db();
        make_session(&db, "s1");
        let record = create_command_recorder(RecorderCtx {
            db: db.clone(),
            session_id: "s1".to_string(),
            workspace: ws_s.clone(),
            message_id: None,
            now: Some(Arc::new(|| 42)),
            touched: None,
        });
        record(finished("bun test src/a.ts", "bun:test"));
        let rows = db
            .lock()
            .unwrap()
            .command_tag_rows(&ws_s, CommandTagOpts::default())
            .unwrap();
        assert_eq!(
            rows,
            vec![
                crate::types::CommandTagRow {
                    tag: "bun".into(),
                    ts: 42,
                    exit_code: Some(0)
                },
                crate::types::CommandTagRow {
                    tag: "test".into(),
                    ts: 42,
                    exit_code: Some(0)
                },
            ]
        );
        assert_eq!(
            db.lock()
                .unwrap()
                .command_tag_rows(
                    &ws_s,
                    CommandTagOpts {
                        dir: Some("src".into()),
                        since_ts: None
                    }
                )
                .unwrap()
                .len(),
            2
        );

        // A recorder over a session the db has never seen (FK violation)
        // swallows it. Not panicking IS the assertion.
        let db2 = mem_db();
        let broken = create_command_recorder(RecorderCtx {
            db: db2.clone(),
            session_id: "ghost".to_string(),
            workspace: ws_s.clone(),
            message_id: None,
            now: None,
            touched: None,
        });
        broken(finished("true", "t"));
        assert!(db2
            .lock()
            .unwrap()
            .command_tag_rows(&ws_s, CommandTagOpts::default())
            .unwrap()
            .is_empty());

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn the_recorder_pushes_touched_dirs_the_dir_hint_trigger_input() {
        let ws = std::env::temp_dir().join(format!("bough-rec-touch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(ws.join("migrations")).unwrap();
        let ws_s = ws.to_string_lossy().into_owned();
        let db = mem_db();
        make_session(&db, "s1");
        let touched = Arc::new(Mutex::new(Vec::new()));
        let record = create_command_recorder(RecorderCtx {
            db,
            session_id: "s1".to_string(),
            workspace: ws_s.clone(),
            message_id: None,
            now: Some(Arc::new(|| 42)),
            touched: Some(touched.clone()),
        });
        // A trailing slash is what makes a bare dir name path-shaped — the
        // token heuristic skips slashless, dotless words as command words.
        record(finished("ls migrations/", "ls"));
        assert_eq!(
            touched.lock().unwrap().clone(),
            vec![Path::new(&ws_s)
                .join("migrations")
                .to_string_lossy()
                .into_owned()]
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
