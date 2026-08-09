//! The note memory: prose keyed on a tag, beside the command memory.
//!
//! WHY THIS EXISTS. `command_history` records what ran and whether it worked;
//! it cannot record WHY. That knowledge either lives in a head or dies with
//! the session — the observed failure being nine `session_state` keys for one
//! PR rollout, each written by a different lineage root and invisible to the
//! next session.
//!
//! THE INVARIANT THIS MODULE HOLDS: **a note contains no command strings and
//! no command output.** Every "how" is a citation into `bough tags show TAG`.
//! Break it and the two stores become two copies of the same facts that age
//! independently, which is the exact drift the split exists to prevent. The
//! test `a_note_never_stores_a_command` pins it.
//!
//! PROVENANCE POINTS ONE WAY. `command_history` is canonical; the note's
//! `## Log` is DERIVED from it and rebuildable ([`rebuild_log`] drops it so a
//! re-fold can reproduce it). The body above the Log is not derived — it is
//! original authorship and canonical in its own right. That is why the page is
//! zoned rather than free-form: one file, two authorities, and the machine
//! writes only in the derived zone.
//!
//! NOT A DATABASE TABLE. Markdown files under `~/.bough/notes`, so the notes
//! are hand-editable, git-syncable across installs, readable by llmwiki and
//! Obsidian, and survive a database reset — the same contract artifacts and
//! skills already have (spec §4). The table set stays closed.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::errors::BoughError;
use crate::history::tags::record::is_ref;

pub mod drift;
pub mod llmwiki;

// ---------------------------------------------------------------------------
// Caps
// ---------------------------------------------------------------------------

/// Log lines kept on one page. Past this an append is refused rather than
/// rotating: silently dropping the oldest line would make the Log a lossy copy
/// of a log that is not lossy, and the fix (fold it into prose) is a human's.
pub const MAX_LOG_LINES: usize = 40;

/// One Log line. A cheap model asked for "a line" will write a paragraph if
/// nothing stops it.
pub const MAX_LINE_CHARS: usize = 120;

/// Whole-page ceiling, matching `session_state`'s per-key limit. Notes, not
/// storage.
pub const MAX_NOTE_BYTES: usize = 16 * 1024;

/// A reference needs this many commands before automation will create a page
/// for it. On a real memory (1,971 tags, 143 references) this is the
/// difference between ~6 pages and 143 stubs.
pub const AUTO_CREATE_MIN_COMMANDS: usize = 20;

/// …across at least this many sessions, so one long afternoon does not mint a
/// page for a reference that never came back.
pub const AUTO_CREATE_MIN_SESSIONS: usize = 2;

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// Who wrote a Log line. The trust question a reader cannot otherwise answer:
/// a line the cheap model inferred and a line you typed arrive as the same
/// paragraph of confident text unless the payload says otherwise, and no
/// staleness query can detect a note that was WRONG WHEN WRITTEN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// You, at a terminal.
    Human,
    /// The session model, mid-turn, through `bough notes append`.
    Session,
    /// The cheap tier, automatically.
    Cheap,
}

impl Source {
    pub fn glyph(&self) -> char {
        match self {
            Source::Human => '*',
            Source::Session => '+',
            Source::Cheap => '~',
        }
    }

    pub fn from_glyph(c: char) -> Option<Source> {
        match c {
            '*' => Some(Source::Human),
            '+' => Some(Source::Session),
            '~' => Some(Source::Cheap),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Source::Human => "you",
            Source::Session => "session model",
            Source::Cheap => "cheap model",
        }
    }
}

/// One line of the derived zone.
#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    pub source: Source,
    /// `YYYY-MM-DD`, local time — the question is "what did I do on Tuesday".
    pub date: String,
    pub text: String,
}

impl LogLine {
    fn render(&self) -> String {
        format!("{} {}  {}", self.source.glyph(), self.date, self.text)
    }

    fn parse(line: &str) -> Option<LogLine> {
        let mut chars = line.chars();
        let source = Source::from_glyph(chars.next()?)?;
        let rest = chars.as_str().trim_start();
        let (date, text) = rest.split_once(char::is_whitespace)?;
        if date.len() != 10 || !date.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        Some(LogLine {
            source,
            date: date.to_string(),
            text: text.trim().to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// The heading that opens the derived zone. Verbatim product surface: the CLI
/// splits on it, `wiki` renders it, and you read it.
pub const LOG_HEADING: &str = "## Log";

/// One note. `body` is everything between the frontmatter and [`LOG_HEADING`]
/// — prose, wikilinks, and any `> [!WARNING]` callouts a consolidation raised.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// The tag this note is about. A reference (`linear.nme-1673`) or an
    /// ordinary vocabulary tag (`nased`).
    pub key: String,
    pub title: String,
    /// Per HOST frontier: the last `command_history.ts` folded into the Log on
    /// that machine. A map and not a scalar because the notes directory
    /// git-syncs between installs while the command memory does NOT — one
    /// scalar would let one host advance the frontier past another host's
    /// unfolded commands, and those rows would never be seen again.
    pub synced: BTreeMap<String, i64>,
    pub body: String,
    pub log: Vec<LogLine>,
}

impl Note {
    pub fn new(key: &str, title: &str) -> Note {
        Note {
            key: key.to_string(),
            title: title.to_string(),
            synced: BTreeMap::new(),
            body: String::new(),
            log: Vec::new(),
        }
    }

    /// This host's frontier, or 0 when this host has never folded anything.
    pub fn frontier(&self, host: &str) -> i64 {
        self.synced.get(host).copied().unwrap_or(0)
    }

    /// Advance this host's frontier. Never moves backwards: a re-fold over an
    /// older window must not un-sync rows already accounted for.
    pub fn mark_synced(&mut self, host: &str, ts: i64) {
        let entry = self.synced.entry(host.to_string()).or_insert(0);
        if ts > *entry {
            *entry = ts;
        }
    }

    /// Does the body carry an unresolved contradiction marker?
    pub fn has_warning(&self) -> bool {
        self.body
            .lines()
            .any(|l| l.trim_start().starts_with("> [!WARNING]"))
    }

    /// The page WITHOUT frontmatter — what llmwiki is handed.
    ///
    /// It must never be given [`render`](Note::render): `wiki write` writes
    /// its own frontmatter around whatever content it receives, so passing a
    /// rendered page nests one set of frontmatter inside another and the
    /// `key` and `synced` fields end up inside the body, where the next read
    /// takes them for prose. `a_rewrite_keeps_the_derived_log` is the test
    /// that found it.
    pub fn page_content(&self) -> String {
        let mut out = String::new();
        let body = self.body.trim();
        if !body.is_empty() {
            out.push_str(body);
            out.push_str("\n\n");
        }
        out.push_str(LOG_HEADING);
        out.push('\n');
        for line in &self.log {
            out.push_str(&line.render());
            out.push('\n');
        }
        out
    }

    pub fn render(&self) -> String {
        let mut out = String::from("---\n");
        let _ = writeln!(out, "title: {}", self.title);
        let _ = writeln!(out, "key: {}", self.key);
        if self.synced.is_empty() {
            out.push_str("synced: {}\n");
        } else {
            out.push_str("synced:\n");
            for (host, ts) in &self.synced {
                let _ = writeln!(out, "  {host}: {ts}");
            }
        }
        out.push_str("---\n\n");
        let body = self.body.trim();
        if !body.is_empty() {
            out.push_str(body);
            out.push_str("\n\n");
        }
        out.push_str(LOG_HEADING);
        out.push('\n');
        for line in &self.log {
            out.push_str(&line.render());
            out.push('\n');
        }
        out
    }

    pub fn parse(text: &str) -> Note {
        let (front, rest) = split_frontmatter(text);
        let mut note = Note::new("", "");
        let mut in_synced = false;
        for line in front.lines() {
            if let Some(v) = line.strip_prefix("title:") {
                note.title = v.trim().to_string();
                in_synced = false;
            } else if let Some(v) = line.strip_prefix("key:") {
                note.key = v.trim().to_string();
                in_synced = false;
            } else if let Some(v) = line.strip_prefix("synced:") {
                in_synced = v.trim().is_empty();
            } else if in_synced && line.starts_with(' ') {
                if let Some((host, ts)) = line.trim().split_once(':') {
                    if let Ok(ts) = ts.trim().parse::<i64>() {
                        note.synced.insert(host.trim().to_string(), ts);
                    }
                }
            } else if !line.trim().is_empty() {
                in_synced = false;
            }
        }
        // The Log is whatever follows the LAST `## Log` heading, so a body that
        // quotes the heading inside prose does not swallow the zone.
        match rest.rfind(LOG_HEADING) {
            Some(at) => {
                note.body = rest[..at].trim().to_string();
                let tail = &rest[at + LOG_HEADING.len()..];
                note.log = tail.lines().filter_map(LogLine::parse).collect();
            }
            None => note.body = rest.trim().to_string(),
        }
        note
    }
}

fn split_frontmatter(text: &str) -> (&str, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return ("", text);
    };
    match rest.find("\n---") {
        Some(end) => {
            let after = &rest[end + 4..];
            (&rest[..end], after.strip_prefix('\n').unwrap_or(after))
        }
        None => ("", text),
    }
}

// ---------------------------------------------------------------------------
// Where a note lives
// ---------------------------------------------------------------------------

/// The wiki subdirectory a key files under. References get their own, because
/// they have a lifecycle (a ticket closes) and ordinary vocabulary does not.
pub fn dir_for(key: &str) -> &'static str {
    if is_ref(key) {
        "refs"
    } else {
        "tags"
    }
}

/// The page path for a key, under a notes root.
///
/// A reference keeps slashes (`branch.claude/tags-history` is one id), so they
/// are folded to `-` for the filename — half a branch name is a path, not a
/// reference, and a nested directory would hide the page from `wiki list`.
pub fn path_for(root: &Path, key: &str) -> PathBuf {
    root.join("wiki")
        .join(dir_for(key))
        .join(format!("{}.md", key.replace('/', "-")))
}

/// Every note under a root, in no particular order. Unreadable files are
/// skipped, not raised: one corrupt page must not blind the whole memory.
pub fn list(root: &Path) -> Vec<Note> {
    let mut out = Vec::new();
    for dir in ["refs", "tags"] {
        let Ok(entries) = std::fs::read_dir(root.join("wiki").join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                let note = Note::parse(&text);
                if !note.key.is_empty() {
                    out.push(note);
                }
            }
        }
    }
    out
}

/// The note for a key, or `None`.
pub fn load(root: &Path, key: &str) -> Option<Note> {
    let text = std::fs::read_to_string(path_for(root, key)).ok()?;
    let mut note = Note::parse(&text);
    if note.key.is_empty() {
        note.key = key.to_string();
    }
    Some(note)
}

/// Write a note, creating the directory. Refuses a page over the byte cap
/// rather than truncating it — a truncated note is a note that lies.
pub fn save(root: &Path, note: &Note) -> Result<PathBuf, BoughError> {
    let text = note.render();
    if text.len() > MAX_NOTE_BYTES {
        return Err(BoughError::bad_request(format!(
            "note {} is {} bytes, over the {MAX_NOTE_BYTES} limit — notes are notes, not storage: \
             move the detail into a file and link it",
            note.key,
            text.len()
        )));
    }
    let path = path_for(root, &note.key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            BoughError::bad_request(format!("cannot create {}: {e}", parent.display()))
        })?;
    }
    std::fs::write(&path, text)
        .map_err(|e| BoughError::bad_request(format!("cannot write {}: {e}", path.display())))?;
    Ok(path)
}

/// Delete the derived zone so a re-fold can reproduce it from
/// `command_history`. The body is untouched, and every host frontier is reset
/// so the next fold walks the whole window again.
///
/// This is the separation test, executable: if a rebuild cannot reproduce the
/// Log, something canonical was living in the derived zone.
pub fn rebuild_log(note: &mut Note) {
    note.log.clear();
    note.synced.clear();
}

// ---------------------------------------------------------------------------
// Appending
// ---------------------------------------------------------------------------

/// What an append did. `Refused` is a normal outcome, not an error — the cheap
/// tier's contract is that it can only ever ADD, so a cap it cannot satisfy
/// must be a quiet no-op rather than a failed turn.
#[derive(Clone, Debug, PartialEq)]
pub enum Appended {
    Wrote,
    /// The Log is full; a human has to fold it into prose.
    LogFull,
    /// Nothing worth writing (empty after trimming, or a duplicate of the last
    /// line).
    Empty,
}

/// Append one line to the derived zone, enforcing every cap.
///
/// A line identical to the last one is dropped: the cheap tier fires per
/// round, and a rollout that runs the same check ten times must not produce
/// ten identical lines. That is deduplication AT WRITE TIME, which is what
/// makes a later consolidation pass unnecessary.
pub fn append_log(note: &mut Note, source: Source, date: &str, text: &str) -> Appended {
    let text = text.trim();
    if text.is_empty() {
        return Appended::Empty;
    }
    if note.log.last().map(|l| l.text.as_str()) == Some(text) {
        return Appended::Empty;
    }
    if note.log.len() >= MAX_LOG_LINES {
        return Appended::LogFull;
    }
    let text: String = text.chars().take(MAX_LINE_CHARS).collect();
    note.log.push(LogLine {
        source,
        date: date.to_string(),
        text,
    });
    Appended::Wrote
}

/// `YYYY-MM-DD` in LOCAL time. A log line answers "what did I do on Tuesday",
/// and a UTC boundary answers a different question — the same rule
/// `bough tags stats` groups days by.
pub fn local_date(now_ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(now_ms)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

// ---------------------------------------------------------------------------
// This host
// ---------------------------------------------------------------------------

/// The machine's name, for the per-host sync frontier. `$BOUGH_HOST` overrides
/// it so a test is not at the mercy of the developer's hostname.
pub fn host_name() -> String {
    if let Ok(v) = std::env::var("BOUGH_HOST") {
        if !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    let mut buf = [0i8; 256];
    // SAFETY: `gethostname` writes at most `len` bytes into `buf`.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) } == 0;
    if !ok {
        return "unknown-host".to_string();
    }
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    let name = String::from_utf8_lossy(&bytes).trim().to_string();
    if name.is_empty() {
        "unknown-host".to_string()
    } else {
        name
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bough-notes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_page_round_trips_through_render_and_parse() {
        let mut note = Note::new("linear.nme-1673", "NASED executor removal");
        note.body = "DAG removal lands first.\n\nRelated: [[pr.7134]]".into();
        note.mark_synced("host-a", 1786286717159);
        append_log(&mut note, Source::Cheap, "2026-08-09", "otel rollout green");
        append_log(&mut note, Source::Human, "2026-08-10", "cutover merged");

        let back = Note::parse(&note.render());
        assert_eq!(back, note);
        assert_eq!(back.log[0].source, Source::Cheap);
        assert_eq!(back.log[1].source, Source::Human);
        assert_eq!(back.frontier("host-a"), 1786286717159);
    }

    #[test]
    fn a_page_with_no_frontmatter_is_still_readable_as_a_body() {
        let note = Note::parse("just some prose someone pasted");
        assert_eq!(note.body, "just some prose someone pasted");
        assert!(note.log.is_empty());
    }

    #[test]
    fn the_log_is_whatever_follows_the_last_heading() {
        // A body that QUOTES the heading must not swallow the derived zone.
        let text = "---\ntitle: t\nkey: k\nsynced: {}\n---\n\nwe keep a `## Log` section.\n\n## Log\n~ 2026-08-09  a line\n";
        let note = Note::parse(text);
        assert!(note.body.contains("`## Log`"));
        assert_eq!(note.log.len(), 1);
        assert_eq!(note.log[0].text, "a line");
    }

    #[test]
    fn a_frontier_never_moves_backwards() {
        let mut note = Note::new("k", "t");
        note.mark_synced("h", 100);
        note.mark_synced("h", 50);
        assert_eq!(note.frontier("h"), 100);
    }

    #[test]
    fn each_host_keeps_its_own_frontier() {
        // The bug this shape exists to prevent: the notes dir git-syncs, the
        // command memory does not, so a shared scalar would let one machine
        // mark another machine's unfolded rows as accounted for.
        let mut note = Note::new("k", "t");
        note.mark_synced("laptop", 500);
        note.mark_synced("server", 100);
        assert_eq!(note.frontier("laptop"), 500);
        assert_eq!(note.frontier("server"), 100);
        assert_eq!(note.frontier("third-machine"), 0);
        assert_eq!(Note::parse(&note.render()).synced, note.synced);
    }

    #[test]
    fn the_log_stops_at_its_cap_instead_of_rotating() {
        let mut note = Note::new("k", "t");
        for i in 0..MAX_LOG_LINES {
            assert_eq!(
                append_log(&mut note, Source::Cheap, "2026-08-09", &format!("line {i}")),
                Appended::Wrote
            );
        }
        assert_eq!(
            append_log(&mut note, Source::Cheap, "2026-08-09", "one more"),
            Appended::LogFull
        );
        assert_eq!(note.log.len(), MAX_LOG_LINES);
        assert_eq!(note.log[0].text, "line 0", "the oldest line is kept");
    }

    #[test]
    fn a_long_line_is_capped_and_a_repeat_is_dropped() {
        let mut note = Note::new("k", "t");
        append_log(&mut note, Source::Cheap, "2026-08-09", &"x".repeat(400));
        assert_eq!(note.log[0].text.chars().count(), MAX_LINE_CHARS);
        append_log(&mut note, Source::Cheap, "2026-08-09", "same");
        assert_eq!(
            append_log(&mut note, Source::Cheap, "2026-08-10", "same"),
            Appended::Empty,
            "write-time dedup is what makes a consolidation rewrite unnecessary"
        );
        assert_eq!(
            append_log(&mut note, Source::Cheap, "2026-08-09", "   "),
            Appended::Empty
        );
    }

    #[test]
    fn rebuilding_drops_the_derived_zone_and_keeps_the_body() {
        let mut note = Note::new("k", "t");
        note.body = "the curated claim".into();
        note.mark_synced("h", 900);
        append_log(&mut note, Source::Cheap, "2026-08-09", "derived");
        rebuild_log(&mut note);
        assert!(note.log.is_empty());
        assert_eq!(note.frontier("h"), 0, "the window reopens");
        assert_eq!(note.body, "the curated claim", "canonical prose survives");
    }

    #[test]
    fn a_warning_is_visible_to_the_stale_queue() {
        let mut note = Note::new("k", "t");
        assert!(!note.has_warning());
        note.body = "claim\n\n> [!WARNING] the cutover merged; check this".into();
        assert!(note.has_warning());
    }

    #[test]
    fn references_and_vocabulary_file_apart() {
        assert_eq!(dir_for("linear.nme-1673"), "refs");
        assert_eq!(dir_for("nased"), "tags");
        let root = Path::new("/n");
        assert!(path_for(root, "linear.nme-1673").ends_with("wiki/refs/linear.nme-1673.md"));
        assert!(path_for(root, "nased").ends_with("wiki/tags/nased.md"));
        assert!(
            path_for(root, "branch.claude/tags-history")
                .ends_with("wiki/refs/branch.claude-tags-history.md"),
            "a slash in a reference is folded, never a nested directory"
        );
    }

    #[test]
    fn saving_and_loading_a_note() {
        let root = tmp();
        let mut note = Note::new("nased", "NASED");
        note.body = "the scheduler's evaluator".into();
        append_log(
            &mut note,
            Source::Session,
            "2026-08-09",
            "dag removal first",
        );
        save(&root, &note).unwrap();

        let back = load(&root, "nased").unwrap();
        assert_eq!(back, note);
        assert_eq!(list(&root).len(), 1);
        assert!(load(&root, "missing").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_oversized_page_is_refused_not_truncated() {
        let root = tmp();
        let mut note = Note::new("k", "t");
        note.body = "x".repeat(MAX_NOTE_BYTES + 1);
        let error = save(&root, &note).unwrap_err();
        assert!(error.to_string().contains("over the"), "{error}");
        assert!(!path_for(&root, "k").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_note_never_stores_a_command() {
        // The invariant, pinned where it is cheapest to check: the render path
        // has no field a command string could ride in. If a future column
        // appears here, this test is the one that should stop it.
        let mut note = Note::new("k", "t");
        note.body = "why, not how".into();
        append_log(&mut note, Source::Cheap, "2026-08-09", "an outcome");
        let text = note.render();
        for forbidden in ["exit_code", "output_head", "$ ", "cmd:"] {
            assert!(!text.contains(forbidden), "{forbidden} leaked into a note");
        }
    }

    #[test]
    fn the_host_name_is_overridable() {
        std::env::set_var("BOUGH_HOST", "test-host");
        assert_eq!(host_name(), "test-host");
        std::env::remove_var("BOUGH_HOST");
        assert!(!host_name().is_empty());
    }
}
