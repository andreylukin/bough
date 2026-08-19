//! The file verbs — `view`, `patch`, `write` — and the per-session snapshot
//! store that is what lets an empty tag mean anything at all. Port of
//! `src/hostfn/files.ts` (spec: hostfn.md §files).
//!
//! THE INVARIANT THIS HOLDS: **`[path#]` — the empty tag — always means the
//! exact bytes this session last saw at that path, and a patch is refused
//! outright when there are no such bytes on record.** So:
//!
//!   - `view()` RECORDS the text it renders, keyed by the RESOLVED path — so
//!     `view("m.ts")` and a later `[./m.ts#]` are one record, not two.
//!   - `patch()` records what it just wrote, so the TAG it echoes is live: a
//!     second patch chains onto it without viewing again.
//!   - `write()` records too — writing a file is a way of seeing it.
//!   - A section naming a file this session never viewed is REFUSED, not
//!     applied against whatever the file happens to be now.
//!
//! Keeping the TEXT and not just its hash is what makes a stale patch
//! *recoverable* rather than merely *detectable*: when the other writer stayed
//! out of the patched lines, the engine rebases and both edits land.
//!
//! There is no `read()` and no `edit()`. One editing idiom. The workspace is
//! the ORIGIN for relative paths, never a boundary: an absolute path anywhere
//! the user can reach resolves unchanged, matching `bash`. Nothing here is a
//! security mechanism.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::errors::BoughError;
use crate::hostfn::patch::{
    apply_patch, group_by_file, parse_patch, render_numbered, tag_of, to_lines,
};

// ---------------------------------------------------------------------------
// The snapshot store
// ---------------------------------------------------------------------------

/// What one session last saw at each path, bounded: the oldest entry is
/// dropped rather than growing without limit. A dropped snapshot costs a
/// re-view, never a wrong edit.
pub const MAX_SNAPSHOTS_PER_SESSION: usize = 64;

/// …and a bound on sessions too. The server runs for weeks, so keying by
/// session without a cap is a slow leak.
pub const MAX_SESSIONS: usize = 32;

/// Per-session memory of viewed text, LRU-bounded on both axes.
///
/// Memory-only and deliberately so: a server restart loses it, and the correct
/// consequence is that the next patch is refused with "call view() first" —
/// a wasted round, not a wrong edit.
///
/// Scoping is per session, not per lineage: a subagent is its own session and
/// must view a file itself before patching it. Two agents sharing a checkout
/// must each anchor to what THEY read, or the hash anchoring stops
/// distinguishing "I saw this" from "someone told me".
pub struct SnapshotStore {
    max_sessions: usize,
    max_per_session: usize,
    /// session id → (absolute path → text). Both vecs are least-recently-used
    /// first; sizes are tiny (≤ 32 / ≤ 64) so linear scans are fine.
    by_session: Mutex<Vec<(String, Vec<(String, String)>)>>,
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::with_limits(MAX_SESSIONS, MAX_SNAPSHOTS_PER_SESSION)
    }

    pub fn with_limits(max_sessions: usize, max_per_session: usize) -> Self {
        SnapshotStore {
            max_sessions,
            max_per_session,
            by_session: Mutex::new(Vec::new()),
        }
    }

    /// Remember the text a session just saw. Call after view, patch and write.
    pub fn record(&self, session_id: &str, abs_path: &str, text: &str) {
        let mut sessions = self.by_session.lock().unwrap();
        // Re-insert the session on every touch so order tracks recency.
        let mut files = match sessions.iter().position(|(id, _)| id == session_id) {
            Some(i) => sessions.remove(i).1,
            None => Vec::new(),
        };
        if let Some(i) = files.iter().position(|(p, _)| p == abs_path) {
            files.remove(i); // re-insert so order tracks recency
        }
        files.push((abs_path.to_string(), text.to_string()));
        while files.len() > self.max_per_session {
            files.remove(0);
        }
        sessions.push((session_id.to_string(), files));
        while sessions.len() > self.max_sessions {
            sessions.remove(0);
        }
    }

    /// The text this session last saw at `abs_path`, if it is still held.
    pub fn get(&self, session_id: &str, abs_path: &str) -> Option<String> {
        let sessions = self.by_session.lock().unwrap();
        sessions
            .iter()
            .find(|(id, _)| id == session_id)
            .and_then(|(_, files)| files.iter().find(|(p, _)| p == abs_path))
            .map(|(_, text)| text.clone())
    }

    /// Live path count for a session — the eviction tests read it.
    pub fn size(&self, session_id: &str) -> usize {
        let sessions = self.by_session.lock().unwrap();
        sessions
            .iter()
            .find(|(id, _)| id == session_id)
            .map(|(_, f)| f.len())
            .unwrap_or(0)
    }

    /// Forget everything a session saw. Not called in the turn path.
    pub fn clear(&self, session_id: &str) {
        let mut sessions = self.by_session.lock().unwrap();
        sessions.retain(|(id, _)| id != session_id);
    }
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Cap per session: a report lists files, and a runaway loop must not grow
/// this forever.
pub const MAX_TRACKED_WRITES: usize = 200;

/// Which paths a session's own programs WROTE, keyed by session id.
///
/// Git cannot answer "which files did THIS agent change": subagents share
/// their spawner's checkout by design, so `git diff` at the end reports the
/// union of every concurrent sibling's work. The write verbs know exactly
/// what they wrote, so this records it at the source.
pub struct WriteLog {
    by_session: Mutex<HashMap<String, Vec<String>>>,
}

impl WriteLog {
    pub fn new() -> Self {
        WriteLog {
            by_session: Mutex::new(HashMap::new()),
        }
    }

    pub fn record(&self, session_id: &str, path: &str) {
        let mut map = self.by_session.lock().unwrap();
        let list = map.entry(session_id.to_string()).or_default();
        if list.len() < MAX_TRACKED_WRITES && !list.iter().any(|p| p == path) {
            list.push(path.to_string());
        }
    }

    /// The paths this session has written so far, WITHOUT forgetting them.
    ///
    /// A second reader exists now — the turn boundary tells hooks which files
    /// this turn wrote, and a hook that consumed the log would silently empty
    /// it under the report that clears it. Peeking is the only safe shape for
    /// a store with two consumers and one of them destructive.
    pub fn paths(&self, session_id: &str) -> Vec<String> {
        self.by_session
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// The paths this session wrote, in write order, and FORGET them.
    ///
    /// Read-and-clear because the only caller is a report built once, and a
    /// store that only ever grows in a process that runs for weeks is a leak
    /// with extra steps.
    pub fn take(&self, session_id: &str) -> Vec<String> {
        self.by_session
            .lock()
            .unwrap()
            .remove(session_id)
            .unwrap_or_default()
    }
}

impl Default for WriteLog {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

/// Everything the file verbs need from a turn. Deliberately narrower than
/// `TurnCtx` — these three functions touch no database, no bus and no LLM,
/// and a test should not have to fabricate one to edit a file in a temp
/// directory.
#[derive(Clone, Default)]
pub struct FileCtx {
    pub workspace: String,
    pub session_id: String,
    /// The read trail behind the directory-triggered tag hints. Appended by
    /// `view()`, never consulted here.
    pub reads: Option<Arc<Mutex<Vec<String>>>>,
    /// Largest file this turn's model can be shown. `None` keeps
    /// [`MAX_VIEW_BYTES`] — see [`crate::hostfn::budget`].
    pub view_bytes: Option<u64>,
}

/// A view is rendered into the model's context in full, so an unbounded one
/// is a context overflow. Refusing is strictly better than truncating: a
/// truncated listing would still carry line numbers, and the model would
/// write anchors against a version it never saw.
pub const MAX_VIEW_BYTES: u64 = 2 * 1024 * 1024;

/// The three bridged file functions, bound to one turn. String-in/string-out
/// because the bridge wire is, and for these three the text IS the payload.
pub struct FileHostFns {
    ctx: FileCtx,
    snapshots: Arc<SnapshotStore>,
    writes: Arc<WriteLog>,
}

/// Build the file verbs for one turn.
pub fn create_file_host_fns(
    ctx: FileCtx,
    snapshots: Arc<SnapshotStore>,
    writes: Arc<WriteLog>,
) -> FileHostFns {
    FileHostFns {
        ctx,
        snapshots,
        writes,
    }
}

impl FileHostFns {
    /// The workspace is the origin for relative paths, not a boundary.
    fn abs(&self, path: &str) -> String {
        resolve_lexical(&self.ctx.workspace, path)
    }

    /// `[path#TAG]` plus numbered `N:text` lines — and the record that makes
    /// a later `[path#]` resolvable. Rendering without recording would be a
    /// lie: the model would be handed a tag naming a version nothing can
    /// produce again.
    pub fn view(&self, path: &str) -> Result<String, BoughError> {
        let p = require_path(path, "view")?;
        let full = self.abs(p);

        let stat = std::fs::metadata(&full).map_err(|err| view_read_error(p, &full, &err))?;
        if stat.is_dir() {
            return Err(BoughError::bad_request(format!(
                "cannot view {p}: it is a directory, not a file. List it with \
                 bash(\"ls -la {}\") and view one of the files inside it.",
                shell_quote(p)
            )));
        }
        let limit = self.ctx.view_bytes.unwrap_or(MAX_VIEW_BYTES);
        if stat.len() > limit {
            return Err(BoughError::bad_request(format!(
                "cannot view {p}: it is {} bytes, over the {limit}-byte \
                 view limit for this model, and rendering it would overflow the context \
                 window. Read the \
                 part you need with bash (rg -n PATTERN {}, or \
                 sed -n '1,200p' {}); patch() needs a view() of the file to \
                 anchor to, so edit a smaller file or rewrite this one with write().",
                stat.len(),
                shell_quote(p),
                shell_quote(p),
            )));
        }

        let bytes = std::fs::read(&full).map_err(|err| view_read_error(p, &full, &err))?;
        // Decoding is lossy, so a binary file arrives as replacement
        // characters and writing it back would destroy it. Refuse before it
        // is on record.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if text.contains('\u{0}') {
            return Err(BoughError::bad_request(format!(
                "cannot view {p}: it contains NUL bytes, so it is not a text file — \
                 viewing it would decode it lossily and patching it would corrupt it. \
                 Inspect it with bash instead (file {}).",
                shell_quote(p)
            )));
        }

        // Keyed by the RESOLVED path: "m.ts" and "./m.ts" are one file and
        // must be one record.
        self.snapshots.record(&self.ctx.session_id, &full, &text);
        if let Some(reads) = &self.ctx.reads {
            reads.lock().unwrap().push(full.clone());
        }

        let rendered = render_numbered(p, &text);
        if text.is_empty() {
            return Ok(format!(
                "{}\n(this file is empty — use INS.HEAD: to put the \
                 first lines in, or write() to replace it wholesale)",
                rendered.trim_end()
            ));
        }
        Ok(rendered)
    }

    /// Apply hash-anchored edits and echo each file's NEW tag.
    ///
    /// Order is load-bearing for the all-or-none rule: parse, read every
    /// file, let the engine validate/rebase/assemble every file, and only
    /// then write. A patch that fails on its third file has written nothing.
    pub fn patch(&self, input: &str) -> Result<String, BoughError> {
        let ops = parse_patch(input)?;
        let groups = group_by_file(&ops)?;

        let mut full: HashMap<String, String> = HashMap::new();
        let mut current: HashMap<String, String> = HashMap::new();
        let mut base: HashMap<String, String> = HashMap::new();
        /* absolute path → the section path that claimed it, for aliasing. */
        let mut claimed: HashMap<String, String> = HashMap::new();

        for g in &groups {
            let p = self.abs(&g.path);
            // `group_by_file` merges by the literal string, so "a.ts" and
            // "./a.ts" would be two groups over one file — the second write
            // computed from the pre-patch text would silently discard the
            // first. Refuse instead.
            if let Some(other) = claimed.get(&p) {
                return Err(BoughError::patch(format!(
                    "\"{other}\" and \"{}\" name the same file ({p}) in one patch, so \
                     the second set of operations would be written against the version from \
                     before the first — silently discarding it. Nothing was written. Put all \
                     of that file's operations under a single \"[{other}#]\" section.",
                    g.path
                )));
            }
            claimed.insert(p.clone(), g.path.clone());
            full.insert(g.path.clone(), p.clone());

            let bytes = std::fs::read(&p).map_err(|err| patch_read_error(&g.path, &p, &err))?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            current.insert(g.path.clone(), text);

            // Absent = never viewed. The engine refuses that section by name;
            // leaving the entry out is how it is told.
            if let Some(snapshot) = self.snapshots.get(&self.ctx.session_id, &p) {
                base.insert(g.path.clone(), snapshot);
            }
        }

        // Errors — stale tag, conflict, bad anchor — before returning
        // anything, so nothing below runs on a half-decided patch.
        let next = apply_patch(&current, &ops, Some(&base))?;

        let mut written: Vec<String> = Vec::new();
        let mut out: Vec<String> = Vec::new();
        for g in &groups {
            let p = &full[&g.path];
            let text = &next[&g.path];
            if let Err(err) = std::fs::write(p, text) {
                // Every file was decided before any was written, so this is a
                // filesystem failure, not a patch decision. Say exactly how
                // far it got — the alternative is the model re-applying edits
                // that landed.
                let landed = if written.is_empty() {
                    "Nothing was written.".to_string()
                } else {
                    format!(
                        "Already written and NOT rolled back: {} — \
                         re-view those before editing them again.",
                        written.join(", ")
                    )
                };
                return Err(BoughError::patch(format!(
                    "cannot write {}: {err}. {landed} The remaining files in this patch \
                     were not written.",
                    g.path
                )));
            }
            // What this session last saw at the path is now what it just
            // wrote, so the echoed tag is live: a follow-up patch may anchor
            // to it, or use "[path#]", without viewing again.
            self.snapshots.record(&self.ctx.session_id, p, text);
            self.writes.record(&self.ctx.session_id, &g.path);
            written.push(g.path.clone());
            out.push(format!(
                "[{}#{}] patched — {}, now {}",
                g.path,
                tag_of(text),
                plural(g.ops.len(), "operation"),
                plural(to_lines(text).len(), "line"),
            ));
        }
        Ok(out.join("\n"))
    }

    /// New files and wholesale rewrites. Parent directories are created,
    /// because the alternative is a program that has to `bash("mkdir -p")`
    /// before every new file. Recording is what lets a freshly written file
    /// be patched with `[path#]` in the same round.
    pub fn write(&self, path: &str, content: &str) -> Result<String, BoughError> {
        let p = require_path(path, "write")?;
        let full = self.abs(p);
        let attempt = || -> std::io::Result<()> {
            if let Some(dir) = Path::new(&full).parent() {
                if !dir.as_os_str().is_empty() && dir != Path::new(&full) {
                    std::fs::create_dir_all(dir)?;
                }
            }
            std::fs::write(&full, content)
        };
        attempt().map_err(|err| BoughError::bad_request(format!("cannot write {p}: {err}")))?;
        self.snapshots.record(&self.ctx.session_id, &full, content);
        self.writes.record(&self.ctx.session_id, p);
        let bytes = content.len();
        Ok(format!(
            "[{p}#{}] wrote {} ({})",
            tag_of(content),
            plural(to_lines(content).len(), "line"),
            plural(bytes, "byte"),
        ))
    }
}

// ---------------------------------------------------------------------------
// Path resolution — Node's `resolve(workspace, path)`, purely lexical
// ---------------------------------------------------------------------------

fn resolve_lexical(workspace: &str, path: &str) -> String {
    let candidate = Path::new(path);
    let mut out = PathBuf::new();
    let push_all = |out: &mut PathBuf, p: &Path| {
        for c in p.components() {
            match c {
                Component::Prefix(pre) => *out = PathBuf::from(pre.as_os_str()),
                Component::RootDir => *out = PathBuf::from(std::path::MAIN_SEPARATOR.to_string()),
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                    if out.as_os_str().is_empty() {
                        out.push(std::path::MAIN_SEPARATOR.to_string());
                    }
                }
                Component::Normal(seg) => out.push(seg),
            }
        }
    };
    if !candidate.is_absolute() {
        push_all(&mut out, Path::new(workspace));
    }
    push_all(&mut out, candidate);
    if out.as_os_str().is_empty() {
        out.push(std::path::MAIN_SEPARATOR.to_string());
    }
    out.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Error text — a product surface: what failed, the state, the move
// ---------------------------------------------------------------------------

fn require_path<'a>(path: &'a str, verb: &str) -> Result<&'a str, BoughError> {
    if path.trim().is_empty() {
        return Err(BoughError::bad_request(format!(
            "{verb}() needs a path — it was called with {}. Pass a \
             path relative to the workspace, or an absolute one.",
            serde_json::to_string(path).unwrap_or_else(|_| format!("{path:?}")),
        )));
    }
    Ok(path)
}

fn view_read_error(path: &str, full: &str, err: &std::io::Error) -> BoughError {
    if err.kind() == std::io::ErrorKind::NotFound {
        return BoughError::not_found(format!(
            "cannot view {path}: no such file (looked at {full}). Relative paths \
             resolve against the workspace — check the path with \
             bash(\"ls {}\"), or create it with \
             write(\"{path}\", …).",
            shell_quote(&dirname(path)),
        ));
    }
    BoughError::bad_request(format!("cannot view {path}: {err}"))
}

fn patch_read_error(path: &str, full: &str, err: &std::io::Error) -> BoughError {
    if err.kind() == std::io::ErrorKind::NotFound {
        return BoughError::patch(format!(
            "cannot patch {path}: no such file (looked at {full}). patch() edits a file \
             that exists — create it with write(\"{path}\", …) instead. Nothing was \
             written; a patch applies to all its files or none.",
        ));
    }
    BoughError::patch(format!(
        "cannot patch {path}: {err}. Nothing was written; a patch applies to \
         all its files or none.",
    ))
}

/// Node's `dirname` over the as-written path: the leading directory part, or
/// `"."` when there is none.
fn dirname(path: &str) -> String {
    match Path::new(path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    }
}

/// Single-quote a path for the shell hints above, so a space or `$` is inert.
fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

// ---------------------------------------------------------------------------
// Tests — ported from src/hostfn/files.test.ts. The engine's own conflict
// math is exhausted in patch.rs tests; here it is only spot-checked where it
// crosses the filesystem.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hostfn::budget::budget_for;

    /// File text from lines, with the trailing newline a real file has.
    fn doc(lines: &[&str]) -> String {
        format!("{}\n", lines.join("\n"))
    }

    struct Workspace {
        dir: PathBuf,
        snapshots: Arc<SnapshotStore>,
        writes: Arc<WriteLog>,
    }

    impl Workspace {
        fn new(files: &[(&str, &str)], store: SnapshotStore) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bough-files-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let ws = Workspace {
                dir,
                snapshots: Arc::new(store),
                writes: Arc::new(WriteLog::new()),
            };
            for (path, text) in files {
                ws.put(path, text);
            }
            ws
        }

        fn fns(&self) -> FileHostFns {
            self.session("s1")
        }

        fn session(&self, id: &str) -> FileHostFns {
            create_file_host_fns(
                FileCtx {
                    view_bytes: None,
                    workspace: self.dir.to_string_lossy().into_owned(),
                    session_id: id.to_string(),
                    reads: None,
                },
                self.snapshots.clone(),
                self.writes.clone(),
            )
        }

        fn read(&self, path: &str) -> String {
            std::fs::read_to_string(self.dir.join(path)).unwrap()
        }

        fn put(&self, path: &str, text: &str) {
            let full = self.dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, text).unwrap();
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn ws(files: &[(&str, &str)]) -> Workspace {
        Workspace::new(files, SnapshotStore::new())
    }

    /// The `[path#TAG]` a verb echoed. Fails loudly rather than returning None.
    fn echoed_tag(output: &str, path: &str) -> String {
        let needle = format!("[{path}#");
        let start = output
            .find(&needle)
            .unwrap_or_else(|| panic!("expected a [{path}#TAG] in:\n{output}"));
        let rest = &output[start + needle.len()..];
        let tag: String = rest.chars().take(4).collect();
        assert!(
            tag.len() == 4 && tag.chars().all(|c| c.is_ascii_hexdigit()),
            "expected a 4-hex tag in:\n{output}"
        );
        tag
    }

    fn err_of(r: Result<String, BoughError>) -> BoughError {
        match r {
            Ok(out) => panic!("expected a rejection, but the call resolved:\n{out}"),
            Err(e) => e,
        }
    }

    // -- the acceptance criterion -------------------------------------------

    #[test]
    fn ac_view_then_empty_tag_patch_then_echoed_tag_chains_without_re_view() {
        let w = ws(&[("a.ts", &doc(&["one", "two", "three", "four"]))]);
        let fns = w.fns();

        // 1. view records the version the ops will be written against.
        let listing = fns.view("a.ts").unwrap();
        let viewed_tag = echoed_tag(&listing, "a.ts");

        // 2. an EMPTY tag means "the version I just viewed" — the normal case.
        let first = fns.patch("[a.ts#]\nSWAP 2:\n+TWO\n").unwrap();
        assert_eq!(w.read("a.ts"), doc(&["one", "TWO", "three", "four"]));

        // …and it echoes the file's NEW tag, which is a real tag of the text.
        let first_tag = echoed_tag(&first, "a.ts");
        assert_eq!(first_tag, tag_of(&w.read("a.ts")));
        assert_ne!(first_tag, viewed_tag);
        assert!(
            first.contains("patched — 1 operation, now 4 lines"),
            "{first}"
        );

        // 3. a SECOND patch chains onto the echoed tag with no view() between.
        let second = fns
            .patch(&format!("[a.ts#{first_tag}]\nINS.POST 4:\n+five\nDEL 1\n"))
            .unwrap();
        assert_eq!(w.read("a.ts"), doc(&["TWO", "three", "four", "five"]));

        // …and that echo chains again, indefinitely.
        let second_tag = echoed_tag(&second, "a.ts");
        assert_eq!(second_tag, tag_of(&w.read("a.ts")));
        assert_ne!(second_tag, first_tag);
        fns.patch(&format!("[a.ts#{second_tag}]\nSWAP 1:\n+2\n"))
            .unwrap();
        assert_eq!(w.read("a.ts"), doc(&["2", "three", "four", "five"]));

        // …as does an empty tag, which now names what the last patch wrote.
        fns.patch("[a.ts#]\nINS.HEAD:\n+// header\n").unwrap();
        assert_eq!(
            w.read("a.ts"),
            doc(&["// header", "2", "three", "four", "five"])
        );
    }

    // -- view ----------------------------------------------------------------

    #[test]
    fn view_renders_header_then_numbered_lines_padded_to_common_width() {
        let lines: Vec<String> = (1..=12).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let text = doc(&refs);
        let w = ws(&[("a.ts", &text)]);
        let out = w.fns().view("a.ts").unwrap();
        let rows: Vec<&str> = out.split('\n').collect();
        assert_eq!(rows[0], format!("[a.ts#{}]", tag_of(&text)));
        assert_eq!(rows[1], " 1:line 1");
        assert_eq!(rows[9], " 9:line 9");
        assert_eq!(rows[10], "10:line 10");
        assert_eq!(rows[12], "12:line 12");
        assert_eq!(rows.len(), 13); // header + 12 lines, no trailing blank
    }

    #[test]
    fn view_echoes_the_path_as_written_but_records_it_resolved() {
        let w = ws(&[("sub/a.ts", &doc(&["x"]))]);
        let fns = w.fns();
        let out = fns.view("./sub/a.ts").unwrap();
        assert!(out.starts_with("[./sub/a.ts#"), "{out}");
        // "./sub/a.ts" and "sub/a.ts" are one file, so one record.
        fns.patch("[sub/a.ts#]\nSWAP 1:\n+y\n").unwrap();
        assert_eq!(w.read("sub/a.ts"), doc(&["y"]));
        assert_eq!(w.snapshots.size("s1"), 1);
    }

    #[test]
    fn view_of_an_empty_file_says_so_instead_of_rendering_nothing() {
        let w = ws(&[("empty.ts", "")]);
        let fns = w.fns();
        let out = fns.view("empty.ts").unwrap();
        assert_eq!(
            out.split('\n').next().unwrap(),
            format!("[empty.ts#{}]", tag_of(""))
        );
        assert!(out.contains("this file is empty"), "{out}");
        assert!(out.contains("INS.HEAD:"), "{out}");
        // …and it is on record, so INS.HEAD against it works.
        fns.patch("[empty.ts#]\nINS.HEAD:\n+first\n").unwrap();
        assert_eq!(w.read("empty.ts"), doc(&["first"]));
    }

    #[test]
    fn the_view_limit_is_the_models_not_a_constant_every_model_shares() {
        // 2 MiB is ~500k tokens: on a 131k-window model the old limit said
        // yes to a read four times larger than everything it can hold. The
        // budget is the model's, so the same file is fine for one and refused
        // for another — and the refusal still names the way through.
        let body = "x".repeat(120_000);
        let w = ws(&[("big.ts", &body)]);

        let generous = create_file_host_fns(
            FileCtx {
                view_bytes: None,
                workspace: w.dir.to_string_lossy().into_owned(),
                session_id: "s1".into(),
                reads: None,
            },
            w.snapshots.clone(),
            w.writes.clone(),
        );
        assert!(
            generous.view("big.ts").is_ok(),
            "under the absolute ceiling"
        );

        let tight = create_file_host_fns(
            FileCtx {
                view_bytes: Some(budget_for(Some(131_072)).view_bytes),
                workspace: w.dir.to_string_lossy().into_owned(),
                session_id: "s2".into(),
                reads: None,
            },
            w.snapshots.clone(),
            w.writes.clone(),
        );
        let err = err_of(tight.view("big.ts")).to_string();
        assert!(err.contains("view limit for this model"), "{err}");
        assert!(err.contains("rg -n"), "the way through survives: {err}");
    }

    #[test]
    fn view_of_a_missing_file_names_the_path_and_how_to_create_it() {
        let w = ws(&[]);
        let err = err_of(w.fns().view("nope/a.ts"));
        assert_eq!(err.name(), "NotFoundError");
        let msg = err.to_string();
        assert!(msg.contains("no such file"), "{msg}");
        assert!(msg.contains("nope/a.ts"), "{msg}");
        assert!(msg.contains("write(\"nope/a.ts\""), "{msg}");
    }

    #[test]
    fn view_of_a_directory_names_it_as_one() {
        let w = ws(&[("sub/a.ts", &doc(&["x"]))]);
        let msg = err_of(w.fns().view("sub")).to_string();
        assert!(msg.contains("it is a directory"), "{msg}");
        assert!(msg.contains("bash(\"ls -la sub\")"), "{msg}");
    }

    #[test]
    fn view_refuses_a_binary_file_before_it_can_be_lossily_rewritten() {
        let w = ws(&[]);
        std::fs::write(w.dir.join("b.bin"), [0x89u8, 0x00, 0x01, 0x02]).unwrap();
        let msg = err_of(w.fns().view("b.bin")).to_string();
        assert!(msg.contains("NUL bytes"), "{msg}");
        // Nothing on record, so a patch against it is refused too.
        assert_eq!(w.snapshots.size("s1"), 0);
    }

    #[test]
    fn view_refuses_an_oversized_file_with_a_way_to_read_part_of_it() {
        let w = ws(&[]);
        std::fs::write(w.dir.join("big.txt"), "x".repeat(2 * 1024 * 1024 + 1)).unwrap();
        let msg = err_of(w.fns().view("big.txt")).to_string();
        assert!(msg.contains("over the 2097152-byte view limit"), "{msg}");
        assert!(msg.contains("rg -n PATTERN big.txt"), "{msg}");
    }

    #[test]
    fn view_refuses_an_empty_path_by_name() {
        let w = ws(&[]);
        let msg = err_of(w.fns().view("   ")).to_string();
        assert!(msg.contains("view() needs a path"), "{msg}");
    }

    // -- write ---------------------------------------------------------------

    #[test]
    fn write_creates_parent_directories_echoes_the_tag_and_records_it() {
        let w = ws(&[]);
        let fns = w.fns();
        let content = doc(&["a", "b", "c"]);
        let out = fns.write("deep/er/new.ts", &content).unwrap();
        assert_eq!(w.read("deep/er/new.ts"), content);
        assert_eq!(echoed_tag(&out, "deep/er/new.ts"), tag_of(&content));
        assert!(out.contains("wrote 3 lines (6 bytes)"), "{out}");

        // A file this session wrote is a file it has seen: patch without view.
        fns.patch("[deep/er/new.ts#]\nSWAP 2:\n+B\n").unwrap();
        assert_eq!(w.read("deep/er/new.ts"), doc(&["a", "B", "c"]));
    }

    #[test]
    fn write_replaces_an_existing_file_wholesale_and_re_anchors_it() {
        let w = ws(&[("a.ts", &doc(&["old", "old", "old"]))]);
        let fns = w.fns();
        fns.view("a.ts").unwrap();
        let out = fns.write("a.ts", &doc(&["new"])).unwrap();
        assert_eq!(w.read("a.ts"), doc(&["new"]));
        // The snapshot is the WRITE, not the earlier view.
        assert_eq!(echoed_tag(&out, "a.ts"), tag_of(&doc(&["new"])));
        fns.patch("[a.ts#]\nINS.TAIL:\n+tail\n").unwrap();
        assert_eq!(w.read("a.ts"), doc(&["new", "tail"]));
    }

    #[test]
    fn write_of_an_empty_file_is_0_lines_not_one_blank_one() {
        let w = ws(&[]);
        let out = w.fns().write("e.ts", "").unwrap();
        assert_eq!(w.read("e.ts"), "");
        assert!(out.contains("wrote 0 lines (0 bytes)"), "{out}");
    }

    // -- patch: what the snapshot store is for -------------------------------

    #[test]
    fn patch_of_a_never_viewed_file_is_refused_and_told_to_view_it() {
        let w = ws(&[("a.ts", &doc(&["one", "two"]))]);
        let err = err_of(w.fns().patch("[a.ts#]\nSWAP 1:\n+ONE\n"));
        assert_eq!(err.name(), "PatchError");
        let msg = err.to_string();
        assert!(
            msg.contains("no viewed version of a.ts is on record"),
            "{msg}"
        );
        assert!(msg.contains("call view(\"a.ts\")"), "{msg}");
        assert_eq!(w.read("a.ts"), doc(&["one", "two"])); // untouched
    }

    #[test]
    fn snapshots_are_per_session_a_siblings_view_is_not_mine() {
        let w = ws(&[("a.ts", &doc(&["one", "two"]))]);
        w.session("spawner").view("a.ts").unwrap();
        // A subagent is its own session and must anchor to what IT read.
        let msg = err_of(w.session("subagent").patch("[a.ts#]\nSWAP 1:\n+ONE\n")).to_string();
        assert!(
            msg.contains("no viewed version of a.ts is on record"),
            "{msg}"
        );
        assert_eq!(w.read("a.ts"), doc(&["one", "two"]));

        // The session that did view it is unaffected.
        w.session("spawner")
            .patch("[a.ts#]\nSWAP 1:\n+ONE\n")
            .unwrap();
        assert_eq!(w.read("a.ts"), doc(&["ONE", "two"]));
    }

    #[test]
    fn patch_of_a_missing_file_says_to_write_it_and_nothing_else_lands() {
        let w = ws(&[("a.ts", &doc(&["one"]))]);
        let fns = w.fns();
        fns.view("a.ts").unwrap();
        let msg =
            err_of(fns.patch("[a.ts#]\nSWAP 1:\n+ONE\n\n[gone.ts#]\nSWAP 1:\n+x\n")).to_string();
        assert!(msg.contains("cannot patch gone.ts: no such file"), "{msg}");
        assert!(msg.contains("write(\"gone.ts\""), "{msg}");
        assert!(msg.contains("all its files or none"), "{msg}");
        assert_eq!(w.read("a.ts"), doc(&["one"])); // the readable file untouched
    }

    #[test]
    fn a_stale_explicit_tag_names_the_current_tag_and_the_empty_tag_escape() {
        let w = ws(&[("a.ts", &doc(&["one", "two"]))]);
        let fns = w.fns();
        fns.view("a.ts").unwrap();
        let msg = err_of(fns.patch("[a.ts#0000]\nSWAP 1:\n+ONE\n")).to_string();
        assert!(msg.contains("stale tag"), "{msg}");
        assert!(
            msg.contains(&format!("now #{}", tag_of(&doc(&["one", "two"])))),
            "{msg}"
        );
        assert!(msg.contains("empty tag \"[a.ts#]\""), "{msg}");
        assert_eq!(w.read("a.ts"), doc(&["one", "two"]));
    }

    // -- patch: concurrency, the reason any of this exists --------------------

    #[test]
    fn a_concurrent_edit_outside_the_patched_lines_rebases_and_both_land() {
        let w = ws(&[("a.ts", &doc(&["l1", "l2", "l3", "l4"]))]);
        let fns = w.fns();
        fns.view("a.ts").unwrap();
        // Someone else (another subagent, the user's editor) prepends a line.
        w.put("a.ts", &doc(&["added", "l1", "l2", "l3", "l4"]));

        // Written in the VIEWED coordinates: line 3 is "l3".
        let out = fns.patch("[a.ts#]\nSWAP 3:\n+L3\n").unwrap();
        assert_eq!(w.read("a.ts"), doc(&["added", "l1", "l2", "L3", "l4"]));
        assert_eq!(echoed_tag(&out, "a.ts"), tag_of(&w.read("a.ts")));
    }

    #[test]
    fn a_concurrent_edit_inside_the_patched_lines_is_a_named_conflict() {
        let w = ws(&[("a.ts", &doc(&["l1", "l2", "l3", "l4"]))]);
        let fns = w.fns();
        fns.view("a.ts").unwrap();
        let theirs = doc(&["l1", "l2", "THEIRS", "l4"]);
        w.put("a.ts", &theirs);

        let err = err_of(fns.patch("[a.ts#]\nSWAP 3:\n+MINE\n"));
        assert_eq!(err.name(), "PatchError");
        let msg = err.to_string();
        assert!(msg.contains("patch conflict in a.ts"), "{msg}");
        assert!(msg.contains("lines 3.=3 were rewritten"), "{msg}");
        assert!(msg.contains("Someone else changed a.ts"), "{msg}");
        assert!(msg.contains("Re-view a.ts"), "{msg}");
        // Their edit survives untouched — this is the whole point.
        assert_eq!(w.read("a.ts"), theirs);
    }

    #[test]
    fn multi_file_all_of_them_or_none() {
        let a = doc(&["a1", "a2"]);
        let b = doc(&["b1", "b2"]);
        let w = ws(&[("a.ts", &a), ("b.ts", &b)]);
        let fns = w.fns();
        fns.view("a.ts").unwrap();
        fns.view("b.ts").unwrap();

        // b's anchor is out of range, so NEITHER file may be written.
        let msg = err_of(fns.patch("[a.ts#]\nSWAP 1:\n+A1\n\n[b.ts#]\nSWAP 99:\n+B\n")).to_string();
        assert!(msg.contains("b.ts: line 99 is out of range"), "{msg}");
        assert_eq!(w.read("a.ts"), a);
        assert_eq!(w.read("b.ts"), b);

        // Corrected, both land in one call, and both tags are echoed.
        let out = fns
            .patch("[a.ts#]\nSWAP 1:\n+A1\n\n[b.ts#]\nSWAP 2:\n+B2\n")
            .unwrap();
        assert_eq!(w.read("a.ts"), doc(&["A1", "a2"]));
        assert_eq!(w.read("b.ts"), doc(&["b1", "B2"]));
        assert_eq!(echoed_tag(&out, "a.ts"), tag_of(&w.read("a.ts")));
        assert_eq!(echoed_tag(&out, "b.ts"), tag_of(&w.read("b.ts")));
        assert_eq!(out.split('\n').count(), 2);
    }

    #[test]
    fn two_spellings_of_one_path_in_one_patch_are_refused_not_merged() {
        let w = ws(&[("a.ts", &doc(&["one", "two"]))]);
        let fns = w.fns();
        fns.view("a.ts").unwrap();
        // Both sections would be computed against the pre-patch text, so the
        // second write would silently discard the first.
        let err = err_of(fns.patch("[a.ts#]\nSWAP 1:\n+ONE\n\n[./a.ts#]\nSWAP 2:\n+TWO\n"));
        assert_eq!(err.name(), "PatchError");
        let msg = err.to_string();
        assert!(msg.contains("name the same file"), "{msg}");
        assert!(msg.contains("single \"[a.ts#]\" section"), "{msg}");
        assert_eq!(w.read("a.ts"), doc(&["one", "two"]));
    }

    #[test]
    fn an_absolute_path_outside_the_workspace_is_an_ordinary_target() {
        let outside =
            std::env::temp_dir().join(format!("bough-files-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        let target = outside.join("cfg.txt");
        std::fs::write(&target, doc(&["k=1"])).unwrap();
        let target_s = target.to_string_lossy().into_owned();

        let w = ws(&[]);
        let fns = w.fns();
        // The workspace is the ORIGIN for relative paths, never a boundary.
        fns.view(&target_s).unwrap();
        fns.patch(&format!("[{target_s}#]\nSWAP 1:\n+k=2\n"))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), doc(&["k=2"]));
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn crlf_and_a_missing_trailing_newline_survive_the_round_trip() {
        let w = ws(&[]);
        std::fs::write(w.dir.join("crlf.ts"), "one\r\ntwo\r\nthree").unwrap();
        let fns = w.fns();
        fns.view("crlf.ts").unwrap();
        fns.patch("[crlf.ts#]\nSWAP 2:\n+TWO\n").unwrap();
        assert_eq!(w.read("crlf.ts"), "one\r\nTWO\r\nthree");
    }

    // -- the store's bounds ---------------------------------------------------

    #[test]
    fn the_oldest_path_is_evicted_and_a_dropped_one_costs_a_re_view() {
        let w = Workspace::new(
            &[
                ("a.ts", &doc(&["a"])),
                ("b.ts", &doc(&["b"])),
                ("c.ts", &doc(&["c"])),
            ],
            SnapshotStore::with_limits(MAX_SESSIONS, 2),
        );
        let fns = w.fns();
        fns.view("a.ts").unwrap();
        fns.view("b.ts").unwrap();
        fns.view("c.ts").unwrap(); // evicts a.ts
        assert_eq!(w.snapshots.size("s1"), 2);

        let msg = err_of(fns.patch("[a.ts#]\nSWAP 1:\n+A\n")).to_string();
        assert!(
            msg.contains("no viewed version of a.ts is on record"),
            "{msg}"
        );
        assert_eq!(w.read("a.ts"), doc(&["a"]));

        // Re-viewing puts it back — the eviction costs a round, never an edit.
        fns.view("a.ts").unwrap();
        fns.patch("[a.ts#]\nSWAP 1:\n+A\n").unwrap();
        assert_eq!(w.read("a.ts"), doc(&["A"]));
    }

    #[test]
    fn the_least_recently_active_session_is_evicted_whole() {
        let w = Workspace::new(
            &[("a.ts", &doc(&["a"]))],
            SnapshotStore::with_limits(2, MAX_SNAPSHOTS_PER_SESSION),
        );
        w.session("s-old").view("a.ts").unwrap();
        w.session("s-mid").view("a.ts").unwrap();
        w.session("s-new").view("a.ts").unwrap(); // evicts s-old
        assert_eq!(w.snapshots.size("s-old"), 0);
        assert_eq!(w.snapshots.size("s-mid"), 1);
        assert_eq!(w.snapshots.size("s-new"), 1);

        let msg = err_of(w.session("s-old").patch("[a.ts#]\nSWAP 1:\n+A\n")).to_string();
        assert!(
            msg.contains("no viewed version of a.ts is on record"),
            "{msg}"
        );
    }

    #[test]
    fn recording_is_keyed_by_session_so_two_sessions_hold_two_versions() {
        let w = ws(&[("a.ts", &doc(&["one"]))]);
        let one = w.session("one");
        let two = w.session("two");
        one.view("a.ts").unwrap();
        two.view("a.ts").unwrap();
        // `one` patches first; `two` is now anchored to a version that has
        // moved on, but its lines were not touched, so it rebases.
        one.patch("[a.ts#]\nINS.HEAD:\n+header\n").unwrap();
        two.patch("[a.ts#]\nINS.TAIL:\n+footer\n").unwrap();
        assert_eq!(w.read("a.ts"), doc(&["header", "one", "footer"]));
    }

    #[test]
    fn the_write_verbs_record_what_a_session_wrote_and_the_record_is_read_once() {
        let w = ws(&[]);
        let fns = w.fns();
        fns.write("lib/alpha.py", &doc(&["def a(): pass"])).unwrap();
        fns.write("lib/beta.py", &doc(&["def b(): pass"])).unwrap();
        // A patch counts too: it is the other way a file changes.
        let shown = fns.view("lib/alpha.py").unwrap();
        let tag = echoed_tag(&shown, "lib/alpha.py");
        fns.patch(&format!(
            "[lib/alpha.py#{tag}]\nSWAP 1.=1:\n+def a(): return 1"
        ))
        .unwrap();

        let mut wrote = w.writes.take("s1");
        wrote.sort();
        assert_eq!(wrote.join(","), "lib/alpha.py,lib/beta.py");
        // READ ONCE: a store that only grows in a process running for weeks
        // is a leak with extra steps.
        assert_eq!(w.writes.take("s1").len(), 0);
        // Another session's writes are its own.
        assert_eq!(w.writes.take("s2").len(), 0);
    }
}
