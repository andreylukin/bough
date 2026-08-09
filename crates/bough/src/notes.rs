//! `bough notes` — the note memory's one door, for the model and the human
//! alike.
//!
//! WHY A COMMAND AND NOT A HOST FUNCTION. Exactly `bough tags`'s argument: one
//! surface, and no bridge to keep in step with it. The model writes a note by
//! running `bough notes append`, which means the write itself lands in
//! `command_history` under its own tag — the memory records its own
//! maintenance, which a host function would have hidden.
//!
//! WHAT IS READ AND WHAT IS WRITTEN. Reads never need the CLI's optional
//! llmwiki bridge; writes use it only to keep `index.md` current, and say so
//! when it is missing rather than failing. The store itself is
//! `bough_core::notes`.
//!
//! Conventions are `tags.rs`'s: parsing is pure and total, effects are
//! injected, `run_notes` returns an exit code and never touches a real
//! process.
//!
//! Exit codes:
//!
//!   0  answered
//!   1  nothing to answer with (no such note, no memory yet)
//!   2  usage problem

use std::path::PathBuf;
use std::sync::Arc;

use bough_core::db::sqlite_db::{open_db, DbOptions};
use bough_core::history::tags::stats::workspace_repo;
use bough_core::notes::drift::{drift_for, Drift};
use bough_core::notes::{self, llmwiki, Note, Source};
use bough_core::paths::{db_path, notes_dir};
use bough_core::types::Db;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesVerb {
    List,
    Show,
    Write,
    Append,
    Search,
    Stale,
    Rebuild,
    Lint,
    Check,
    Path,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotesArgs {
    pub verb: NotesVerb,
    /// The tag a note is about; also the search query.
    pub key: Option<String>,
    /// `append`: the line. Absent = read it from stdin.
    pub text: Option<String>,
    pub title: Option<String>,
    pub repo: Option<String>,
    pub all_repos: bool,
    pub limit: usize,
    pub json: bool,
}

impl Default for NotesArgs {
    fn default() -> Self {
        NotesArgs {
            verb: NotesVerb::List,
            key: None,
            text: None,
            title: None,
            repo: None,
            all_repos: false,
            limit: 20,
            json: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Args(NotesArgs),
    UsageError(String),
}

pub const USAGE: &str = "usage: bough notes [VERB] [OPTIONS]

  (none)          every note, most out of date first
  show TAG        one note — the prose, then its log
  write TAG       replace a note's prose; the body comes in on stdin
  append TAG TEXT add one line to the log (stdin when TEXT is absent)
  search WORDS    notes whose prose or log mention every word
  stale           how far behind each note is, warnings first
  rebuild TAG     drop the derived log so it can be re-folded from commands
  check           ask the cheap model whether any log line contradicts a claim
  lint            llmwiki's structural check (broken links, orphans, index)
  path [TAG]      where a note's file is, or the wiki directory itself

  --title T       write: the note's title (default: the tag)
  --repo R        scope drift to a repo identity; default: this checkout's
  --all           measure drift across every repo
  --limit N       rows (default 20)
  --json          machine-readable output
  -h, --help      this

A note holds WHY, never a command: `bough tags show TAG` is the how.

exit: 0 answered · 1 nothing there · 2 usage";

/// Pure and total: arguments in, arguments or a usage error out. Never panics.
pub fn parse_notes_args(argv: &[String]) -> Parsed {
    let mut args = NotesArgs::default();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "-h" || a == "--help" {
            return Parsed::UsageError(USAGE.to_string());
        }
        match a {
            "--json" => {
                args.json = true;
                i += 1;
                continue;
            }
            "--all" => {
                args.all_repos = true;
                i += 1;
                continue;
            }
            "--title" | "--repo" => {
                let Some(v) = argv.get(i + 1) else {
                    return Parsed::UsageError(format!("{a} needs a value\n{USAGE}"));
                };
                if a == "--title" {
                    args.title = Some(v.clone());
                } else {
                    args.repo = Some(v.clone());
                }
                i += 2;
                continue;
            }
            "--limit" => {
                let n = argv.get(i + 1).and_then(|v| v.parse::<f64>().ok());
                match n {
                    Some(n) if n.is_finite() && n > 0.0 => args.limit = n.trunc() as usize,
                    _ => {
                        return Parsed::UsageError(format!(
                            "--limit needs a positive number\n{USAGE}"
                        ))
                    }
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        if a.starts_with('-') {
            return Parsed::UsageError(format!("unknown option {a}\n{USAGE}"));
        }
        positional.push(a.to_string());
        i += 1;
    }

    let first = positional.first().cloned();
    let rest = positional.len().saturating_sub(1);
    let needs_one = |verb: &str, v: NotesVerb, args: &mut NotesArgs| -> Option<Parsed> {
        if rest != 1 {
            return Some(Parsed::UsageError(format!(
                "{verb} needs exactly one TAG\n{USAGE}"
            )));
        }
        args.verb = v;
        args.key = Some(positional[1].clone());
        None
    };

    match first.as_deref() {
        Some("show") => {
            if let Some(e) = needs_one("show", NotesVerb::Show, &mut args) {
                return e;
            }
        }
        Some("write") => {
            if let Some(e) = needs_one("write", NotesVerb::Write, &mut args) {
                return e;
            }
        }
        Some("rebuild") => {
            if let Some(e) = needs_one("rebuild", NotesVerb::Rebuild, &mut args) {
                return e;
            }
        }
        Some("path") => {
            // The one verb whose argument is optional: with no TAG it answers
            // "where does this all live", which is the question someone asks
            // before they know a tag to ask about.
            if rest > 1 {
                return Parsed::UsageError(format!("path takes at most one TAG\n{USAGE}"));
            }
            args.verb = NotesVerb::Path;
            args.key = positional.get(1).cloned();
        }
        Some("append") => {
            if !(1..=2).contains(&rest) {
                return Parsed::UsageError(format!(
                    "append needs a TAG and a line (or a TAG, with the line on stdin)\n{USAGE}"
                ));
            }
            args.verb = NotesVerb::Append;
            args.key = Some(positional[1].clone());
            args.text = positional.get(2).cloned();
        }
        Some("search") => {
            if rest < 1 {
                return Parsed::UsageError(format!("search needs something to look for\n{USAGE}"));
            }
            args.verb = NotesVerb::Search;
            args.key = Some(positional[1..].join(" "));
        }
        Some("stale") => {
            if rest > 0 {
                return Parsed::UsageError(format!("stale takes no arguments\n{USAGE}"));
            }
            args.verb = NotesVerb::Stale;
        }
        Some("lint") => {
            if rest > 0 {
                return Parsed::UsageError(format!("lint takes no arguments\n{USAGE}"));
            }
            args.verb = NotesVerb::Lint;
        }
        Some("check") => {
            if rest > 1 {
                return Parsed::UsageError(format!("check takes at most one TAG\n{USAGE}"));
            }
            args.verb = NotesVerb::Check;
            args.key = positional.get(1).cloned();
        }
        // A bare word is a tag, and `show` is what someone typing one wants —
        // the same guess `bough tags` makes, for the same reason.
        Some(word) => {
            if rest > 0 {
                return Parsed::UsageError(format!("unknown verb {word}\n{USAGE}"));
            }
            args.verb = NotesVerb::Show;
            args.key = Some(word.to_string());
        }
        None => {}
    }
    if args.all_repos {
        args.repo = None;
    }
    Parsed::Args(args)
}

// ---------------------------------------------------------------------------
// Deps
// ---------------------------------------------------------------------------

pub struct NotesDeps<'a> {
    pub db: Option<&'a dyn Db>,
    /// Absent = `paths::notes_dir()`.
    pub root: Option<PathBuf>,
    pub cwd: Option<String>,
    pub now: Option<i64>,
    /// Absent = this machine's name. The per-host sync frontier's key.
    pub host: Option<String>,
    /// `$BOUGH_SESSION` — set in every shell bough runs, absent in yours. It
    /// is what tells an appended line apart from one you typed.
    pub session: Option<String>,
    /// Reads stdin for `write` and a bodyless `append`.
    pub stdin: Arc<dyn Fn() -> String>,
    pub out: Arc<dyn Fn(&str)>,
    pub err: Arc<dyn Fn(&str)>,
}

impl<'a> NotesDeps<'a> {
    pub fn real() -> NotesDeps<'a> {
        NotesDeps {
            db: None,
            root: None,
            cwd: None,
            now: None,
            host: None,
            session: std::env::var("BOUGH_SESSION")
                .ok()
                .filter(|v| !v.is_empty()),
            stdin: Arc::new(|| {
                use std::io::Read;
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                buf
            }),
            out: Arc::new(|l: &str| println!("{l}")),
            err: Arc::new(|l: &str| eprintln!("{l}")),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub use bough_core::notes::local_date;

/// How long ago, in the compact form the tag views already use.
fn ago(now: i64, ts: i64) -> String {
    let secs = ((now - ts).max(0)) / 1000;
    if secs < 90 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 90 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 48 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_note(note: &Note, out: &dyn Fn(&str)) {
    out(&format!("{}  ({})", note.title, note.key));
    out("");
    if !note.body.trim().is_empty() {
        for line in note.body.trim().lines() {
            out(&format!("  {line}"));
        }
        out("");
    }
    if note.log.is_empty() {
        out("  (no log yet)");
    } else {
        out("  log");
        for line in &note.log {
            out(&format!(
                "    {} {}  {}",
                line.source.glyph(),
                line.date,
                line.text
            ));
        }
    }
    out("");
    out(&format!(
        "  * you · + session model · ~ cheap model — commands: bough tags show {}",
        note.key
    ));
}

fn note_json(note: &Note, drift: Option<&Drift>, root: &std::path::Path) -> serde_json::Value {
    json!({
        "key": note.key,
        "path": notes::path_for(root, &note.key).to_string_lossy(),
        "title": note.title,
        "body": note.body,
        "synced": note.synced,
        "warned": note.has_warning(),
        "log": note.log.iter().map(|l| json!({
            "source": l.source.label(),
            "date": l.date,
            "text": l.text,
        })).collect::<Vec<_>>(),
        "unfolded": drift.map(|d| d.unfolded),
    })
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

pub fn run_notes(argv: &[String], deps: &NotesDeps<'_>) -> i32 {
    let parsed = match parse_notes_args(argv) {
        Parsed::UsageError(message) => {
            (deps.err)(&message);
            return if message == USAGE { 0 } else { 2 };
        }
        Parsed::Args(a) => a,
    };
    let now = deps.now.unwrap_or_else(now_ms);
    let root = deps.root.clone().unwrap_or_else(notes_dir);
    let host = deps.host.clone().unwrap_or_else(notes::host_name);

    match parsed.verb {
        NotesVerb::Path => {
            let Some(key) = parsed.key.clone() else {
                (deps.out)(&root.join("wiki").to_string_lossy());
                return 0;
            };
            let path = notes::path_for(&root, &key);
            // stdout stays the path either way, so `$EDITOR $(bough notes path
            // new-thing)` opens a new note instead of failing. The fact that
            // nothing is there yet goes to stderr, where a pipeline ignores it
            // and a human does not.
            (deps.out)(&path.to_string_lossy());
            if !path.exists() {
                (deps.err)(&format!(
                    "(no note on {key} yet — that is where one would go)"
                ));
            }
            0
        }

        NotesVerb::Lint => match llmwiki::lint(&root) {
            Ok(report) => {
                (deps.out)(if report.is_empty() { "clean" } else { &report });
                // SCHEMA.md is bough's own conventions page, written at wiki
                // creation and linked from nothing by design. Reporting it as
                // an orphan on every run teaches the reader to skim past a
                // command whose whole job is to be read.
                if report.contains("SCHEMA.md") {
                    (deps.out)(
                        "\n(SCHEMA.md is this wiki's conventions page — it is meant to have no \
                         inbound links.)",
                    );
                }
                0
            }
            Err(why) => {
                (deps.err)(&why);
                1
            }
        },

        NotesVerb::Show => {
            let key = parsed.key.clone().unwrap_or_default();
            let Some(note) = notes::load(&root, &key) else {
                (deps.err)(&format!(
                    "no note on {key} yet — write one with: bough notes write {key}\n\
                     (what has already been RUN under it: bough tags show {key})"
                ));
                return 1;
            };
            if parsed.json {
                (deps.out)(&note_json(&note, None, &root).to_string());
            } else {
                render_note(&note, &*deps.out);
            }
            0
        }

        NotesVerb::Write => {
            let key = parsed.key.clone().unwrap_or_default();
            let body = (deps.stdin)();
            if body.trim().is_empty() {
                (deps.err)("nothing on stdin — the note's prose is the body of this command");
                return 2;
            }
            // The log survives a rewrite: the body is yours, the log is
            // derived, and replacing one must not silently discard the other.
            let mut note = notes::load(&root, &key)
                .unwrap_or_else(|| Note::new(&key, parsed.title.as_deref().unwrap_or(&key)));
            if let Some(title) = &parsed.title {
                note.title = title.clone();
            }
            note.body = body.trim().to_string();
            // ORDER MATTERS, and this is the only place it does: `wiki write`
            // writes its OWN frontmatter around the content it is handed, so
            // it must go first and bough's authoritative render must land on
            // top of it. Reversed, llmwiki's copy wins and the per-host sync
            // frontier is silently swallowed into the body.
            let indexed = llmwiki::ensure_wiki(&root).and_then(|_| {
                llmwiki::index_page(
                    &root,
                    &llmwiki::rel_path(&key),
                    &note.title,
                    &note.page_content(),
                )
            });
            let path = match notes::save(&root, &note) {
                Ok(p) => p,
                Err(error) => {
                    (deps.err)(&error.to_string());
                    return 2;
                }
            };
            (deps.out)(&format!("wrote {}", path.display()));
            if let Err(why) = indexed {
                (deps.err)(&format!("index not updated: {why}"));
            }
            0
        }

        NotesVerb::Append => {
            let key = parsed.key.clone().unwrap_or_default();
            let text = parsed.text.clone().unwrap_or_else(|| (deps.stdin)());
            // Inside a turn `$BOUGH_SESSION` is set, so a line the model wrote
            // is told apart from a line you typed without either having to say
            // so — which is the whole point of the provenance glyph.
            let source = if deps.session.is_some() {
                Source::Session
            } else {
                Source::Human
            };
            let mut note = notes::load(&root, &key)
                .unwrap_or_else(|| Note::new(&key, parsed.title.as_deref().unwrap_or(&key)));
            match notes::append_log(&mut note, source, &local_date(now), &text) {
                notes::Appended::Empty => {
                    (deps.err)("nothing to append");
                    return 2;
                }
                notes::Appended::LogFull => {
                    (deps.err)(&format!(
                        "the log on {key} is full ({} lines) — fold it into the note's prose \
                         (bough notes write {key}), which is a judgment only you make",
                        notes::MAX_LOG_LINES
                    ));
                    return 1;
                }
                notes::Appended::Wrote => {}
            }
            match notes::save(&root, &note) {
                Ok(_) => {
                    (deps.out)(&format!("{} {}", source.glyph(), key));
                    0
                }
                Err(error) => {
                    (deps.err)(&error.to_string());
                    2
                }
            }
        }

        NotesVerb::Rebuild => {
            let key = parsed.key.clone().unwrap_or_default();
            let Some(mut note) = notes::load(&root, &key) else {
                (deps.err)(&format!("no note on {key}"));
                return 1;
            };
            let had = note.log.len();
            notes::rebuild_log(&mut note);
            if let Err(error) = notes::save(&root, &note) {
                (deps.err)(&error.to_string());
                return 2;
            }
            (deps.out)(&format!(
                "dropped {had} derived line(s) from {key}; the prose is untouched and every \
                 host frontier is reopened"
            ));
            0
        }

        NotesVerb::Search => {
            let query = parsed.key.clone().unwrap_or_default();
            let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
            let mut hits: Vec<Note> = notes::list(&root)
                .into_iter()
                .filter(|n| {
                    let hay = format!(
                        "{} {} {} {}",
                        n.key,
                        n.title,
                        n.body,
                        n.log
                            .iter()
                            .map(|l| l.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                    .to_lowercase();
                    words.iter().all(|w| hay.contains(w))
                })
                .collect();
            hits.sort_by(|a, b| a.key.cmp(&b.key));
            hits.truncate(parsed.limit);
            if hits.is_empty() {
                (deps.err)(&format!(
                    "no note mentions that. The commands might: bough tags sql \"SELECT cmd FROM \
                     command_history_fts f JOIN command_history h ON h.id = f.command_id WHERE \
                     f.cmd MATCH '{query}' LIMIT 10\""
                ));
                return 1;
            }
            if parsed.json {
                let rows: Vec<_> = hits.iter().map(|n| note_json(n, None, &root)).collect();
                (deps.out)(&serde_json::Value::Array(rows).to_string());
            } else {
                for n in &hits {
                    (deps.out)(&format!("{:<28} {}", n.key, n.title));
                }
            }
            0
        }

        NotesVerb::Check => {
            let cheap = bough_core::worker::create_cheap_tier();
            let Some(cheap) = cheap else {
                (deps.err)("no cheap tier here, so there is nothing to check with");
                return 1;
            };
            let targets: Vec<Note> = match &parsed.key {
                Some(key) => match notes::load(&root, key) {
                    Some(n) => vec![n],
                    None => {
                        (deps.err)(&format!("no note on {key}"));
                        return 1;
                    }
                },
                None => notes::list(&root),
            };
            if targets.is_empty() {
                (deps.err)("no notes to check");
                return 1;
            }
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(error) => {
                    (deps.err)(&error.to_string());
                    return 2;
                }
            };
            let mut raised = 0usize;
            for mut note in targets {
                // Already flagged: a second warning on the same claim adds
                // nothing, and only a human clears the first.
                if note.has_warning() || note.log.is_empty() || note.body.trim().is_empty() {
                    continue;
                }
                let prompt = bough_core::worker::notes::contradiction_gist(&note);
                let Some(reason) = rt.block_on(cheap.note_contradiction(&prompt)) else {
                    continue;
                };
                // INSERTED, never applied: the claim is left exactly as its
                // author wrote it, and resolving the conflict is a judgment
                // this model is not allowed to make.
                note.body = format!(
                    "{}\n\n> [!WARNING] {reason}\n> Raised by the cheap model on {}; \
                     resolve it with `bough notes write {}`.",
                    note.body.trim_end(),
                    local_date(now),
                    note.key
                );
                if notes::save(&root, &note).is_ok() {
                    raised += 1;
                    (deps.out)(&format!("⚠ {}  {reason}", note.key));
                }
            }
            if raised == 0 {
                (deps.out)("no contradictions found");
            }
            0
        }

        NotesVerb::List | NotesVerb::Stale => {
            let all = notes::list(&root);
            if all.is_empty() {
                (deps.err)(&format!(
                    "no notes yet at {} — the first one: bough notes write <tag>",
                    root.display()
                ));
                return 1;
            }
            // Drift needs the command memory. Without it the notes still list,
            // just without the "how far behind" column — a missing database is
            // not a reason to refuse to show prose that is right there.
            let owned;
            let db: Option<&dyn Db> = match deps.db {
                Some(db) => Some(db),
                None if db_path().exists() => match open_db(None, DbOptions::default()) {
                    Ok(db) => {
                        owned = db;
                        Some(&owned)
                    }
                    Err(_) => None,
                },
                None => None,
            };
            let cwd = deps.cwd.clone().unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let repo: Option<String> = if parsed.all_repos {
                None
            } else {
                Some(parsed.repo.clone().unwrap_or_else(|| workspace_repo(&cwd)))
            };

            let mut rows: Vec<(Note, Option<Drift>)> = all
                .into_iter()
                .map(|note| {
                    let drift = db.and_then(|db| {
                        drift_for(
                            db,
                            &note.key,
                            repo.as_deref(),
                            note.frontier(&host),
                            note.has_warning(),
                            500,
                        )
                        .ok()
                    });
                    (note, drift)
                })
                .collect();
            rows.sort_by(|a, b| {
                let sev =
                    |d: &Option<Drift>| d.as_ref().map(|d| d.severity()).unwrap_or((false, 0));
                sev(&b.1)
                    .cmp(&sev(&a.1))
                    .then_with(|| a.0.key.cmp(&b.0.key))
            });
            rows.truncate(parsed.limit);

            if parsed.json {
                let out: Vec<_> = rows
                    .iter()
                    .map(|(n, d)| note_json(n, d.as_ref(), &root))
                    .collect();
                (deps.out)(&serde_json::Value::Array(out).to_string());
                return 0;
            }
            for (note, drift) in &rows {
                let when = note
                    .synced
                    .get(&host)
                    .map(|ts| ago(now, *ts))
                    .unwrap_or_else(|| "never".to_string());
                let state = match drift {
                    None => "—".to_string(),
                    Some(d) if d.warned => format!("⚠ warning · {} behind", d.unfolded),
                    Some(d) if d.unfolded == 0 => "fresh".to_string(),
                    Some(d) => format!("{} commands since sync", d.unfolded),
                };
                (deps.out)(&format!("{:<28} {:<32} {when}", note.key, state));
            }
            if db.is_none() {
                (deps.err)("no command memory here, so nothing could be measured against it");
            }
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    struct Collect {
        root: PathBuf,
        out: Arc<Mutex<Vec<String>>>,
        err: Arc<Mutex<Vec<String>>>,
        stdin: String,
        session: Option<String>,
    }

    impl Collect {
        fn new() -> Collect {
            let root =
                std::env::temp_dir().join(format!("bough-notes-cli-{}", uuid::Uuid::new_v4()));
            Collect {
                root,
                out: Arc::new(Mutex::new(Vec::new())),
                err: Arc::new(Mutex::new(Vec::new())),
                stdin: String::new(),
                session: None,
            }
        }
        fn deps(&self) -> NotesDeps<'_> {
            let out = self.out.clone();
            let err = self.err.clone();
            let stdin = self.stdin.clone();
            NotesDeps {
                db: None,
                root: Some(self.root.clone()),
                cwd: Some("/nowhere".into()),
                now: Some(1_786_000_000_000),
                host: Some("test-host".into()),
                session: self.session.clone(),
                stdin: Arc::new(move || stdin.clone()),
                out: Arc::new(move |l: &str| out.lock().unwrap().push(l.to_string())),
                err: Arc::new(move |l: &str| err.lock().unwrap().push(l.to_string())),
            }
        }
        fn printed(&self) -> String {
            self.out.lock().unwrap().join("\n")
        }
        fn errors(&self) -> String {
            self.err.lock().unwrap().join("\n")
        }
    }

    impl Drop for Collect {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn parsing_is_total() {
        assert_eq!(
            parse_notes_args(&argv(&["show", "nased"])),
            Parsed::Args(NotesArgs {
                verb: NotesVerb::Show,
                key: Some("nased".into()),
                ..Default::default()
            })
        );
        // A bare word is a tag, like `bough tags`.
        match parse_notes_args(&argv(&["nased"])) {
            Parsed::Args(a) => {
                assert_eq!(a.verb, NotesVerb::Show);
                assert_eq!(a.key.as_deref(), Some("nased"));
            }
            other => panic!("{other:?}"),
        }
        for (input, needle) in [
            (argv(&["show"]), "exactly one TAG"),
            (argv(&["stale", "x"]), "stale takes no arguments"),
            (argv(&["search"]), "something to look for"),
            (argv(&["--limit", "-3"]), "positive number"),
            (argv(&["--nope"]), "unknown option"),
            (argv(&["frobnicate", "x"]), "unknown verb"),
        ] {
            match parse_notes_args(&input) {
                Parsed::UsageError(m) => assert!(m.contains(needle), "{input:?} → {m}"),
                other => panic!("{input:?} parsed: {other:?}"),
            }
        }
        assert!(matches!(
            parse_notes_args(&argv(&["-h"])),
            Parsed::UsageError(m) if m == USAGE
        ));
    }

    #[test]
    fn all_beats_an_explicit_repo() {
        match parse_notes_args(&argv(&["stale", "--repo", "r", "--all"])) {
            Parsed::Args(a) => assert_eq!(a.repo, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn write_then_show_round_trips() {
        let mut c = Collect::new();
        c.stdin = "DAG removal lands first.".into();
        assert_eq!(
            run_notes(&argv(&["write", "nased", "--title", "NASED"]), &c.deps()),
            0
        );
        assert!(c.printed().contains("wrote"));

        let c2 = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        assert_eq!(run_notes(&argv(&["show", "nased"]), &c2.deps()), 0);
        let shown = c2.printed();
        assert!(shown.contains("NASED"));
        assert!(shown.contains("DAG removal lands first."));
        assert!(shown.contains("(no log yet)"));
        assert!(
            shown.contains("bough tags show nased"),
            "every note points at the commands it is an interpretation of"
        );
        std::mem::forget(c2); // one Drop cleans the shared root
    }

    #[test]
    fn a_missing_note_names_the_two_ways_forward() {
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["show", "ghost"]), &c.deps()), 1);
        let e = c.errors();
        assert!(e.contains("bough notes write ghost"));
        assert!(e.contains("bough tags show ghost"), "{e}");
    }

    #[test]
    fn an_appended_line_records_who_wrote_it() {
        let mut c = Collect::new();
        c.stdin = "prose".into();
        run_notes(&argv(&["write", "pr.7134"]), &c.deps());

        // No BOUGH_SESSION: a human at a terminal.
        let human = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        assert_eq!(
            run_notes(
                &argv(&["append", "pr.7134", "cutover merged"]),
                &human.deps()
            ),
            0
        );
        assert!(human.printed().starts_with('*'));

        // Inside a turn: the session model.
        let model = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: Some("s1".into()),
        };
        assert_eq!(
            run_notes(
                &argv(&["append", "pr.7134", "backfill window closed"]),
                &model.deps()
            ),
            0
        );
        assert!(model.printed().starts_with('+'));

        let note = notes::load(&c.root, "pr.7134").unwrap();
        assert_eq!(note.log[0].source, Source::Human);
        assert_eq!(note.log[1].source, Source::Session);
        assert_eq!(note.body, "prose", "an append never touches the prose");
        std::mem::forget(human);
        std::mem::forget(model);
    }

    #[test]
    fn a_rewrite_keeps_the_derived_log() {
        let mut c = Collect::new();
        c.stdin = "first".into();
        run_notes(&argv(&["write", "k"]), &c.deps());
        let appended = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        run_notes(&argv(&["append", "k", "an outcome"]), &appended.deps());

        let mut rewritten = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        rewritten.stdin = "second".into();
        run_notes(&argv(&["write", "k"]), &rewritten.deps());

        let note = notes::load(&c.root, "k").unwrap();
        assert_eq!(note.body, "second");
        assert_eq!(
            note.log.len(),
            1,
            "the two zones are replaced independently"
        );
        std::mem::forget(appended);
        std::mem::forget(rewritten);
    }

    #[test]
    fn a_full_log_says_who_has_to_fold_it() {
        let mut c = Collect::new();
        c.stdin = "p".into();
        run_notes(&argv(&["write", "k"]), &c.deps());
        let mut note = notes::load(&c.root, "k").unwrap();
        for i in 0..notes::MAX_LOG_LINES {
            notes::append_log(&mut note, Source::Cheap, "2026-08-09", &format!("l{i}"));
        }
        notes::save(&c.root, &note).unwrap();

        let full = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        assert_eq!(
            run_notes(&argv(&["append", "k", "one more"]), &full.deps()),
            1
        );
        assert!(
            full.errors().contains("a judgment only you make"),
            "{}",
            full.errors()
        );
        std::mem::forget(full);
    }

    #[test]
    fn rebuild_drops_the_log_and_keeps_the_prose() {
        let mut c = Collect::new();
        c.stdin = "the claim".into();
        run_notes(&argv(&["write", "k"]), &c.deps());
        let a = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        run_notes(&argv(&["append", "k", "derived"]), &a.deps());

        let r = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        assert_eq!(run_notes(&argv(&["rebuild", "k"]), &r.deps()), 0);
        assert!(r.printed().contains("prose is untouched"));
        let note = notes::load(&c.root, "k").unwrap();
        assert!(note.log.is_empty());
        assert_eq!(note.body, "the claim");
        std::mem::forget(a);
        std::mem::forget(r);
    }

    #[test]
    fn search_needs_every_word_and_points_at_the_commands_when_it_misses() {
        let mut c = Collect::new();
        c.stdin = "the executor swap needs the dag removal".into();
        run_notes(&argv(&["write", "nased"]), &c.deps());

        let hit = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        assert_eq!(
            run_notes(&argv(&["search", "executor", "dag"]), &hit.deps()),
            0
        );
        assert!(hit.printed().contains("nased"));

        let miss = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        assert_eq!(
            run_notes(&argv(&["search", "executor", "kubernetes"]), &miss.deps()),
            1
        );
        assert!(miss.errors().contains("command_history_fts"));
        std::mem::forget(hit);
        std::mem::forget(miss);
    }

    #[test]
    fn listing_an_empty_memory_is_exit_one_not_a_crash() {
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&[]), &c.deps()), 1);
        assert!(c.errors().contains("bough notes write"));
    }

    #[test]
    fn the_date_on_a_line_is_local() {
        let stamped = local_date(1_786_000_000_000);
        assert_eq!(stamped.len(), 10);
        assert!(stamped.starts_with("202"));
    }

    #[test]
    fn check_refuses_to_touch_a_note_that_is_already_flagged() {
        // Idempotence, and the trust rule: a second warning on the same claim
        // adds nothing, and only a human clears the first.
        let mut c = Collect::new();
        c.stdin = "a claim\n\n> [!WARNING] already raised".into();
        run_notes(&argv(&["write", "k"]), &c.deps());
        let note = notes::load(&c.root, "k").unwrap();
        assert!(note.has_warning());
        let before = note.body.clone();

        let checked = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        // Exercised without a provider: `create_cheap_tier` is always Some, and
        // every call inside it resolves None with no key. The assertion is
        // that a checked note is byte-identical either way.
        run_notes(&argv(&["check", "k"]), &checked.deps());
        assert_eq!(notes::load(&c.root, "k").unwrap().body, before);
        std::mem::forget(checked);
    }

    #[test]
    fn check_on_a_missing_note_is_exit_one() {
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["check", "ghost"]), &c.deps()), 1);
    }

    #[test]
    fn path_prints_where_the_file_is() {
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["path", "linear.nme-1673"]), &c.deps()), 0);
        assert!(c.printed().ends_with("wiki/refs/linear.nme-1673.md"));
    }

    #[test]
    fn path_with_no_tag_answers_where_the_notes_live() {
        // The question asked before you know a tag to ask about.
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["path"]), &c.deps()), 0);
        // The injected root is a temp dir, so the assertion is on the `wiki`
        // segment, not on `~/.bough/notes` — which is what production resolves.
        assert!(c.printed().ends_with("/wiki"), "{}", c.printed());
        assert_eq!(c.printed(), c.root.join("wiki").to_string_lossy());
        assert!(c.errors().is_empty());
    }

    #[test]
    fn path_for_a_missing_note_still_prints_it_and_says_so_on_stderr() {
        // stdout must stay usable: `$EDITOR $(bough notes path new-thing)`
        // should open a new note, not fail. The absence goes to stderr.
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["path", "brand-new"]), &c.deps()), 0);
        assert!(c.printed().ends_with("wiki/tags/brand-new.md"));
        assert!(c.errors().contains("that is where one would go"));
    }

    #[test]
    fn json_carries_the_path_so_a_script_never_rebuilds_it() {
        let mut c = Collect::new();
        c.stdin = "prose".into();
        run_notes(&argv(&["write", "nased"]), &c.deps());

        let listed = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        assert_eq!(run_notes(&argv(&["--json"]), &listed.deps()), 0);
        let rows: serde_json::Value = serde_json::from_str(&listed.printed()).unwrap();
        let path = rows[0]["path"].as_str().unwrap();
        assert!(path.ends_with("wiki/tags/nased.md"), "{path}");
        assert!(
            std::path::Path::new(path).exists(),
            "the path is real: {path}"
        );

        let shown = Collect {
            root: c.root.clone(),
            out: Arc::new(Mutex::new(vec![])),
            err: Arc::new(Mutex::new(vec![])),
            stdin: String::new(),
            session: None,
        };
        run_notes(&argv(&["show", "nased", "--json"]), &shown.deps());
        let one: serde_json::Value = serde_json::from_str(&shown.printed()).unwrap();
        assert_eq!(one["path"].as_str().unwrap(), path);
        std::mem::forget(listed);
        std::mem::forget(shown);
    }
}
