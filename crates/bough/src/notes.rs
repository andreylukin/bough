//! `bough notes` — the note memory's one door, for the model and the human
//! alike.
//!
//! WHY A COMMAND AND NOT A HOST FUNCTION. Exactly `bough tags`'s argument: one
//! surface, and no bridge to keep in step with it. The model writes a note by
//! running `bough notes append`, which means the write itself lands in
//! `command_history` under its own tag — the memory records its own
//! maintenance, which a host function would have hidden.
//!
//! Conventions are `tags.rs`'s: parsing is pure and total, effects are
//! injected, `run_notes` returns an exit code and never touches a real
//! process.
//!
//! Exit codes:
//!
//!   0  answered
//!   1  nothing to answer with
//!   2  usage problem

use std::sync::Arc;

use bough_core::db::sqlite_db::{open_db, DbOptions};
use bough_core::history::tags::stats::{workspace_repo, TagSpread};
use bough_core::notes::{
    self, canonical_path, drift_for, is_warned, resolve, split_sections, Drift,
};
use bough_core::paths::db_path;
use bough_core::types::{Citation, Db, NoteAuthor, NoteRow, SectionRow, SectionWrite};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesVerb {
    List,
    Show,
    Write,
    Append,
    Search,
    Stale,
    Check,
    History,
    Tree,
    Cites,
    Rm,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotesArgs {
    pub verb: NotesVerb,
    pub key: Option<String>,
    pub text: Option<String>,
    pub title: Option<String>,
    pub repo: Option<String>,
    pub all_repos: bool,
    pub limit: usize,
    pub json: bool,
    /// `show`: include sections that resolve in from elsewhere.
    pub with_transclusions: bool,
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
            with_transclusions: true,
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
  tree            the hierarchy, stubs included
  show PATH       one note: its own sections, then what resolves into it
  write PATH      add or update sections from markdown on stdin
  rm PATH H       remove one section by heading
  append PATH T   add one line to the log (stdin when T is absent)
  search WORDS    sections matching every word
  stale           how far behind each note is, warnings first
  check [PATH]    ask the cheap model whether a log contradicts a claim
  history PATH    every superseded version of this note's sections
  cites PATH      what this note's claims rest on

  `write` UPSERTS by heading and never removes: a section carries its own
  tags, citations and history, and other notes may resolve it, so dropping
  one is an explicit `rm`. Superseded bodies stay in `history`.

  A PATH is one or more TAGS, colon separated: `nased`, `kubectl:rollout`.
  Depth 1 is a top-level note. `## Heading` starts a section; a `tags:` line
  under it NARROWS that section, which is what makes it appear elsewhere.

  --title T       write: the note's title (default: the path)
  --repo R        scope drift to a repo identity; default: this checkout's
  --all           measure drift across every repo
  --own           show: this note's own sections only
  --limit N       rows (default 20)
  --json          machine-readable output
  -h, --help      this

A note holds WHY, never a command: `bough tags show TAG` is the how.

exit: 0 answered · 1 nothing there · 2 usage";

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
            "--own" => {
                args.with_transclusions = false;
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
    let one = |verb: &str, v: NotesVerb, args: &mut NotesArgs| -> Option<Parsed> {
        if rest != 1 {
            return Some(Parsed::UsageError(format!(
                "{verb} needs exactly one PATH\n{USAGE}"
            )));
        }
        args.verb = v;
        args.key = Some(positional[1].clone());
        None
    };

    match first.as_deref() {
        Some("show") => {
            if let Some(e) = one("show", NotesVerb::Show, &mut args) {
                return e;
            }
        }
        Some("write") => {
            if let Some(e) = one("write", NotesVerb::Write, &mut args) {
                return e;
            }
        }
        Some("history") => {
            if let Some(e) = one("history", NotesVerb::History, &mut args) {
                return e;
            }
        }
        Some("cites") => {
            if let Some(e) = one("cites", NotesVerb::Cites, &mut args) {
                return e;
            }
        }
        Some("rm") => {
            if rest != 2 {
                return Parsed::UsageError(format!("rm needs a PATH and a HEADING\n{USAGE}"));
            }
            args.verb = NotesVerb::Rm;
            args.key = Some(positional[1].clone());
            args.text = Some(positional[2].clone());
        }
        Some("append") => {
            if !(1..=2).contains(&rest) {
                return Parsed::UsageError(format!(
                    "append needs a PATH and a line (or a PATH, with the line on stdin)\n{USAGE}"
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
        Some(v @ ("stale" | "tree")) => {
            if rest > 0 {
                return Parsed::UsageError(format!("{v} takes no arguments\n{USAGE}"));
            }
            args.verb = if v == "tree" {
                NotesVerb::Tree
            } else {
                NotesVerb::Stale
            };
        }
        Some("check") => {
            if rest > 1 {
                return Parsed::UsageError(format!("check takes at most one PATH\n{USAGE}"));
            }
            args.verb = NotesVerb::Check;
            args.key = positional.get(1).cloned();
        }
        // A bare word is a path, and `show` is what someone typing one wants.
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
    pub cwd: Option<String>,
    pub now: Option<i64>,
    /// `$BOUGH_SESSION` — set in every shell bough runs, absent in yours. What
    /// tells an appended line apart from one you typed.
    pub session: Option<String>,
    pub stdin: Arc<dyn Fn() -> String>,
    pub out: Arc<dyn Fn(&str)>,
    pub err: Arc<dyn Fn(&str)>,
}

impl<'a> NotesDeps<'a> {
    pub fn real() -> NotesDeps<'a> {
        NotesDeps {
            db: None,
            cwd: None,
            now: None,
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

fn spread_of(db: &dyn Db) -> TagSpread {
    db.tag_spread(None)
        .map(|(repos, by_tag)| TagSpread { repos, by_tag })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_section(s: &SectionRow, transcluded: bool, out: &dyn Fn(&str)) {
    if transcluded {
        // WHERE IT IS AUTHORED, always. A transcluded section that looked
        // native would turn one-home-many-appearances into an invisible copy,
        // and an edit would seem to change the wrong page.
        out(&format!("  ## {}   ← {}", s.heading, s.note_path));
    } else {
        out(&format!("  ## {}", s.heading));
    }
    for line in s.body.lines() {
        out(&format!("     {line}"));
    }
    if !s.citations.is_empty() {
        let cites: Vec<String> = s
            .citations
            .iter()
            .map(|c| format!("{}:{}", c.kind, c.reference))
            .collect();
        out(&format!("     └ {}", cites.join("  ")));
    }
    out("");
}

fn section_json(s: &SectionRow, transcluded: bool) -> serde_json::Value {
    json!({
        "id": s.id,
        "heading": s.heading,
        "body": s.body,
        "tags": s.tags,
        "author": s.author.as_str(),
        "authored_in": s.note_path,
        "transcluded": transcluded,
        "citations": s.citations.iter()
            .map(|c| json!({"kind": c.kind, "ref": c.reference}))
            .collect::<Vec<_>>(),
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

    // A note lives at a path every segment of which a COMMAND could carry, or
    // it does not live. Checked once, here, so no verb can create a page the
    // join can never reach. `search` is exempt — its argument is prose.
    let parsed = match (parsed.verb, &parsed.key) {
        (NotesVerb::Search, _) | (_, None) => parsed,
        (_, Some(raw)) => match canonical_path(raw) {
            Ok((path, _)) => NotesArgs {
                key: Some(path),
                ..parsed
            },
            Err(error) => {
                (deps.err)(&error.to_string());
                return 2;
            }
        },
    };

    let owned;
    let db: &dyn Db = match deps.db {
        Some(db) => db,
        None => {
            if !db_path().exists() {
                (deps.err)(&format!(
                    "no memory yet at {} — run something through bough first",
                    db_path().display()
                ));
                return 1;
            }
            match open_db(None, DbOptions::default()) {
                Ok(db) => {
                    owned = db;
                    &owned
                }
                Err(error) => {
                    (deps.err)(&error.to_string());
                    return 1;
                }
            }
        }
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

    match parsed.verb {
        NotesVerb::Show => {
            let path = parsed.key.clone().unwrap_or_default();
            let Ok(Some(note)) = db.note_by_path(&path) else {
                (deps.err)(&format!(
                    "no note at {path} yet — write one with: bough notes write {path}\n\
                     (what has already been RUN under it: bough tags show {path})"
                ));
                return 1;
            };
            let own = db.sections_for_note(note.id).unwrap_or_default();
            let elsewhere = if parsed.with_transclusions {
                db.sections_for_context(&note.tags, Some(note.id))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let spread = spread_of(db);
            let ranked = resolve::rank(&spread, elsewhere, &note.tags, Some(note.id));

            if parsed.json {
                (deps.out)(
                    &json!({
                        "path": note.path,
                        "title": note.title,
                        "tags": note.tags,
                        "sections": own.iter().map(|s| section_json(s, false)).collect::<Vec<_>>(),
                        "resolved": ranked.iter()
                            .map(|r| section_json(&r.section, true)).collect::<Vec<_>>(),
                    })
                    .to_string(),
                );
                return 0;
            }
            (deps.out)(&format!("{}  ({})", note.title, note.path));
            (deps.out)("");
            for s in &own {
                render_section(s, false, &*deps.out);
            }
            if !ranked.is_empty() {
                (deps.out)("  ── also true here ──────────────────────────────");
                (deps.out)("");
                for r in &ranked {
                    render_section(&r.section, true, &*deps.out);
                }
            }
            let log = db
                .note_log(note.id, parsed.limit as i64)
                .unwrap_or_default();
            if !log.is_empty() {
                (deps.out)("  log");
                for l in &log {
                    (deps.out)(&format!("    {} {}", l.source.glyph(), l.text));
                }
                (deps.out)("");
            }
            // The most DISTINCTIVE tag, by the same idf everything else ranks
            // by — neither the first (`note_tags` comes back alphabetised) nor
            // the last (the grammar's order is the author's, and they may have
            // led with the subject). `dev` is a useless next command; `nased`
            // is the one worth handing over.
            let subject = note
                .tags
                .iter()
                .max_by(|a, b| {
                    notes::idf(&spread, a)
                        .partial_cmp(&notes::idf(&spread, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .cloned()
                .unwrap_or_else(|| note.path.clone());
            (deps.out)(&format!(
                "  * you · + session model · ~ cheap model — commands: bough tags show {subject}"
            ));
            0
        }

        NotesVerb::Write => {
            let path = parsed.key.clone().unwrap_or_default();
            let Ok((_, tags)) = canonical_path(&path) else {
                return 2;
            };
            let markdown = (deps.stdin)();
            if markdown.trim().is_empty() {
                (deps.err)("nothing on stdin — the note's prose is the body of this command");
                return 2;
            }
            let title = parsed.title.clone().unwrap_or_else(|| path.clone());
            let Ok(note_id) = db.upsert_note(&path, &title, &tags, now) else {
                (deps.err)("could not write the note");
                return 2;
            };
            let author = if deps.session.is_some() {
                NoteAuthor::Session
            } else {
                NoteAuthor::Human
            };

            let mut written = 0usize;
            let mut dropped_all: Vec<Citation> = Vec::new();
            for (ord, section) in split_sections(&markdown).into_iter().enumerate() {
                if section.body.len() > notes::MAX_SECTION_BYTES {
                    (deps.err)(&format!(
                        "section `{}` is {} bytes, over the {} limit — notes are notes, not \
                         storage: put the detail in a file and cite it with [file:path]",
                        section.heading,
                        section.body.len(),
                        notes::MAX_SECTION_BYTES
                    ));
                    return 2;
                }
                let section_tags = section.tags.clone().unwrap_or_else(|| tags.clone());
                let cited = notes::parse_citations(&section.body);
                let (kept, dropped) = notes::validate_citations(db, &cited, &section_tags);
                dropped_all.extend(dropped);
                if db
                    .put_section(
                        &SectionWrite {
                            note_id,
                            ord: ord as i64,
                            heading: section.heading,
                            body: section.body,
                            tags: section.tags,
                            citations: kept,
                            author,
                        },
                        now,
                    )
                    .is_ok()
                {
                    written += 1;
                }
            }
            (deps.out)(&format!("wrote {path} — {written} section(s)"));
            for c in &dropped_all {
                // Named, never silently discarded: a citation that does not
                // resolve is the reader's signal that a claim is unsupported.
                (deps.err)(&format!(
                    "citation [{}:{}] does not resolve here and was not recorded",
                    c.kind, c.reference
                ));
            }
            0
        }

        NotesVerb::Append => {
            let path = parsed.key.clone().unwrap_or_default();
            let text = parsed.text.clone().unwrap_or_else(|| (deps.stdin)());
            let text = text.trim();
            if text.is_empty() {
                (deps.err)("nothing to append");
                return 2;
            }
            let Ok(Some(note)) = db.note_by_path(&path) else {
                (deps.err)(&format!(
                    "no note at {path} — start one with: bough notes write {path}"
                ));
                return 1;
            };
            let source = if deps.session.is_some() {
                NoteAuthor::Session
            } else {
                NoteAuthor::Human
            };
            let capped: String = text.chars().take(notes::MAX_LINE_CHARS).collect();
            match db.append_note_log(note.id, now, source, &capped) {
                Ok(true) => {
                    (deps.out)(&format!("{} {path}", source.glyph()));
                    0
                }
                Ok(false) => {
                    (deps.err)("that is already the last line");
                    0
                }
                Err(error) => {
                    (deps.err)(&error.to_string());
                    2
                }
            }
        }

        NotesVerb::Search => {
            let query = parsed.key.clone().unwrap_or_default();
            let words: Vec<String> = query.split_whitespace().map(str::to_string).collect();
            let hits = db
                .search_sections(&words, parsed.limit as i64)
                .unwrap_or_default();
            if hits.is_empty() {
                (deps.err)(&format!(
                    "no note mentions that. The commands might: bough tags sql \"SELECT cmd FROM \
                     command_history_fts f JOIN command_history h ON h.id = f.command_id WHERE \
                     f.cmd MATCH '{query}' LIMIT 10\""
                ));
                return 1;
            }
            if parsed.json {
                (deps.out)(
                    &serde_json::Value::Array(
                        hits.iter().map(|s| section_json(s, false)).collect(),
                    )
                    .to_string(),
                );
            } else {
                for s in &hits {
                    (deps.out)(&format!("{:<28} {}", s.note_path, s.heading));
                }
            }
            0
        }

        NotesVerb::History => {
            let path = parsed.key.clone().unwrap_or_default();
            let Ok(Some(note)) = db.note_by_path(&path) else {
                (deps.err)(&format!("no note at {path}"));
                return 1;
            };
            let mut any = false;
            for s in db.sections_for_note(note.id).unwrap_or_default() {
                let revs = db.section_revisions(s.id).unwrap_or_default();
                if revs.is_empty() {
                    continue;
                }
                any = true;
                (deps.out)(&format!("## {}", s.heading));
                (deps.out)(&format!(
                    "  {} now      {}",
                    s.author.glyph(),
                    s.body.lines().next().unwrap_or("")
                ));
                for r in revs {
                    (deps.out)(&format!(
                        "  {} rev {:<4} {}",
                        r.author.glyph(),
                        r.rev,
                        r.body.lines().next().unwrap_or("")
                    ));
                }
                (deps.out)("");
            }
            if !any {
                (deps.out)("no version has ever been superseded here");
            }
            0
        }

        NotesVerb::Rm => {
            let path = parsed.key.clone().unwrap_or_default();
            let heading = parsed.text.clone().unwrap_or_default();
            let Ok(Some(note)) = db.note_by_path(&path) else {
                (deps.err)(&format!("no note at {path}"));
                return 1;
            };
            let sections = db.sections_for_note(note.id).unwrap_or_default();
            let Some(target) = sections.iter().find(|s| s.heading == heading) else {
                let names: Vec<&str> = sections.iter().map(|s| s.heading.as_str()).collect();
                (deps.err)(&format!(
                    "no section `{heading}` on {path} — it has: {}",
                    names.join(", ")
                ));
                return 1;
            };
            if db.delete_section(target.id).is_err() {
                (deps.err)("could not remove it");
                return 2;
            }
            (deps.out)(&format!("removed {path} · {heading}"));
            0
        }

        NotesVerb::Cites => {
            let path = parsed.key.clone().unwrap_or_default();
            let Ok(Some(note)) = db.note_by_path(&path) else {
                (deps.err)(&format!("no note at {path}"));
                return 1;
            };
            let sections = db.sections_for_note(note.id).unwrap_or_default();
            if sections.is_empty() {
                (deps.err)("no sections here");
                return 1;
            }
            for s in &sections {
                if s.citations.is_empty() {
                    // AN UNCITED CLAIM IS THE INTERESTING ONE, and saying so is
                    // the whole value of the verb: it is the only signal that
                    // separates a claim resting on evidence from one resting on
                    // somebody's memory.
                    (deps.out)(&format!("## {}  (uncited)", s.heading));
                    continue;
                }
                (deps.out)(&format!("## {}", s.heading));
                for c in &s.citations {
                    (deps.out)(&format!("  {}:{}", c.kind, c.reference));
                }
            }
            0
        }

        NotesVerb::Check => {
            let Some(cheap) = bough_core::worker::create_cheap_tier() else {
                (deps.err)("no cheap tier here, so there is nothing to check with");
                return 1;
            };
            let targets: Vec<NoteRow> = match &parsed.key {
                Some(path) => match db.note_by_path(path) {
                    Ok(Some(n)) => vec![n],
                    _ => {
                        (deps.err)(&format!("no note at {path}"));
                        return 1;
                    }
                },
                None => db.list_notes().unwrap_or_default(),
            };
            if targets.is_empty() {
                (deps.err)("no notes to check");
                return 1;
            }
            let Ok(rt) = tokio::runtime::Runtime::new() else {
                (deps.err)("could not start a runtime");
                return 2;
            };
            let mut raised = 0usize;
            for note in targets {
                let sections = db.sections_for_note(note.id).unwrap_or_default();
                let log = db.note_log(note.id, 20).unwrap_or_default();
                // Already flagged: a second warning on the same note adds
                // nothing, and only a human clears the first.
                if log.is_empty() || is_warned(&sections) {
                    continue;
                }
                for s in sections {
                    if s.body.trim().is_empty() {
                        continue;
                    }
                    let prompt = bough_core::worker::notes::contradiction_gist(&s.body, &log);
                    let Some(reason) = rt.block_on(cheap.note_contradiction(&prompt)) else {
                        continue;
                    };
                    // INSERTED, never applied. The claim is left exactly as its
                    // author wrote it — and because the write pushes a revision,
                    // whoever resolves this later cannot lose it silently.
                    let body = format!(
                        "{}\n\n{} {reason}\n> Raised by the cheap model; resolve it with \
                         `bough notes write {}`.",
                        s.body.trim_end(),
                        notes::WARNING_PREFIX,
                        note.path
                    );
                    let _ = db.put_section(
                        &SectionWrite {
                            note_id: note.id,
                            ord: s.ord,
                            heading: s.heading.clone(),
                            body,
                            tags: Some(s.tags.clone()),
                            citations: s.citations.clone(),
                            author: NoteAuthor::Cheap,
                        },
                        now,
                    );
                    raised += 1;
                    (deps.out)(&format!("⚠ {} · {}  {reason}", note.path, s.heading));
                    break;
                }
            }
            if raised == 0 {
                (deps.out)("no contradictions found");
            }
            0
        }

        NotesVerb::Tree => {
            let notes_rows = db.list_notes().unwrap_or_default();
            if notes_rows.is_empty() {
                (deps.err)("no notes yet — the first one: bough notes write <tag>");
                return 1;
            }
            let paths: Vec<String> = notes_rows.iter().map(|n| n.path.clone()).collect();
            let stubs = notes::stubs_for(&paths);
            let mut all: Vec<(String, Option<&NoteRow>)> = notes_rows
                .iter()
                .map(|n| (n.path.clone(), Some(n)))
                .chain(stubs.iter().map(|s| (s.clone(), None)))
                .collect();
            all.sort_by(|a, b| a.0.cmp(&b.0));
            for (path, note) in &all {
                let indent = "  ".repeat(notes::depth(path).saturating_sub(1));
                let leaf = path.rsplit(':').next().unwrap_or(path);
                match note {
                    // A stub is a node nothing was written at — shown so the
                    // tree reads as a tree, never stored.
                    None => (deps.out)(&format!("{indent}{leaf}/")),
                    Some(n) => (deps.out)(&format!("{indent}{leaf}   {}", n.title)),
                }
            }
            0
        }

        NotesVerb::List | NotesVerb::Stale => {
            let notes_rows = db.list_notes().unwrap_or_default();
            if notes_rows.is_empty() {
                (deps.err)("no notes yet — the first one: bough notes write <tag>");
                return 1;
            }
            let mut rows: Vec<(NoteRow, Drift)> = notes_rows
                .into_iter()
                .map(|note| {
                    let sections = db.sections_for_note(note.id).unwrap_or_default();
                    let drift = drift_for(db, &note, repo.as_deref(), is_warned(&sections));
                    (note, drift)
                })
                .collect();
            rows.sort_by(|a, b| {
                b.1.severity()
                    .cmp(&a.1.severity())
                    .then(a.0.path.cmp(&b.0.path))
            });
            rows.truncate(parsed.limit);

            if parsed.json {
                (deps.out)(
                    &serde_json::Value::Array(
                        rows.iter()
                            .map(|(n, d)| {
                                json!({
                                    "path": n.path,
                                    "title": n.title,
                                    "tags": n.tags,
                                    "unfolded": d.unfolded,
                                    "warned": d.warned,
                                    "synced_ts": n.synced_ts,
                                })
                            })
                            .collect(),
                    )
                    .to_string(),
                );
                return 0;
            }
            for (note, drift) in &rows {
                let when = if note.synced_ts == 0 {
                    "never".to_string()
                } else {
                    ago(now, note.synced_ts)
                };
                let state = if drift.warned {
                    format!("⚠ warning · {} behind", drift.unfolded)
                } else if drift.unfolded == 0 {
                    "fresh".to_string()
                } else {
                    format!("{} commands since sync", drift.unfolded)
                };
                (deps.out)(&format!("{:<28} {:<32} {when}", note.path, state));
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
    use bough_core::schema::parts::{Session, SessionKind};
    use bough_core::types::CommandRecord;
    use std::sync::Mutex;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    struct Collect {
        out: Arc<Mutex<Vec<String>>>,
        err: Arc<Mutex<Vec<String>>>,
        stdin: String,
        session: Option<String>,
    }

    impl Collect {
        fn new() -> Collect {
            Collect {
                out: Arc::new(Mutex::new(Vec::new())),
                err: Arc::new(Mutex::new(Vec::new())),
                stdin: String::new(),
                session: None,
            }
        }
        fn deps<'a>(&self, db: &'a dyn Db) -> NotesDeps<'a> {
            let out = self.out.clone();
            let err = self.err.clone();
            let stdin = self.stdin.clone();
            NotesDeps {
                db: Some(db),
                cwd: Some("/nowhere".into()),
                now: Some(1_786_000_000_000),
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

    fn db() -> Box<dyn Db> {
        Box::new(open_db(Some(":memory:"), DbOptions::default()).unwrap())
    }

    fn seed_command(db: &dyn Db, tags: &str, ts: i64, session: &str) {
        if db.get_session(session).ok().flatten().is_none() {
            db.create_session(Session {
                id: session.to_string(),
                title: "t".into(),
                kind: SessionKind::Root,
                created_at: 0,
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
            })
            .unwrap();
        }
        db.record_command(&CommandRecord {
            session_id: session.to_string(),
            ts,
            repo: "/nowhere".into(),
            cmd: format!("some command {ts}"),
            tags: tags.to_string(),
            tag_list: tags.split(':').map(str::to_string).collect(),
            dirs: vec![],
            exit_code: Some(0),
            duration_ms: Some(1),
            output_head: String::new(),
            spill_path: None,
            source: "live".into(),
            message_id: None,
        })
        .unwrap();
    }

    fn write(db: &dyn Db, path: &str, markdown: &str) {
        let mut c = Collect::new();
        c.stdin = markdown.to_string();
        assert_eq!(
            run_notes(&argv(&["write", path]), &c.deps(db)),
            0,
            "{}",
            c.errors()
        );
    }

    #[test]
    fn parsing_is_total() {
        for (input, needle) in [
            (argv(&["show"]), "exactly one PATH"),
            (argv(&["stale", "x"]), "takes no arguments"),
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
        match parse_notes_args(&argv(&["nased"])) {
            Parsed::Args(a) => {
                assert_eq!(a.verb, NotesVerb::Show);
                assert_eq!(a.key.as_deref(), Some("nased"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_path_no_command_could_carry_is_refused_at_every_verb() {
        let db = db();
        for verb in [
            vec!["write", "wrapper-check"],
            vec!["show", "wrapper-check"],
            vec!["append", "wrapper-check", "a line"],
            vec!["history", "wrapper-check"],
        ] {
            let mut c = Collect::new();
            c.stdin = "prose".into();
            assert_eq!(run_notes(&argv(&verb), &c.deps(&*db)), 2, "{verb:?}");
            assert!(
                c.errors().contains("unreachable"),
                "{verb:?}: {}",
                c.errors()
            );
        }
    }

    #[test]
    fn write_then_show_round_trips_and_points_at_the_commands() {
        let db = db();
        write(&*db, "nased", "DAG removal lands before the executor swap.");
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["show", "nased"]), &c.deps(&*db)), 0);
        let shown = c.printed();
        assert!(shown.contains("DAG removal lands before the executor swap."));
        assert!(shown.contains("## Summary"), "no heading needed: {shown}");
        assert!(shown.contains("bough tags show nased"));
    }

    #[test]
    fn a_section_promoted_to_one_tag_appears_on_a_sibling_note() {
        // THE POINT OF THE REDESIGN: a lesson learned under one path shows up
        // under a sibling that shares the facet it was promoted to.
        let db = db();
        write(
            &*db,
            "nased:rollout:prod",
            "## Backfill window\nonly true of prod rollouts.\n\n\
             ## Executor ordering\ntags: nased\nDAG removal must land first.",
        );
        write(
            &*db,
            "nased:backfill:dev",
            "## Dev notes\nthe dev stack is smaller.",
        );

        let c = Collect::new();
        assert_eq!(
            run_notes(&argv(&["show", "nased:backfill:dev"]), &c.deps(&*db)),
            0
        );
        let shown = c.printed();
        assert!(shown.contains("the dev stack is smaller"), "its own prose");
        assert!(shown.contains("also true here"), "{shown}");
        assert!(shown.contains("DAG removal must land first"), "{shown}");
        assert!(
            shown.contains("← nased:rollout:prod"),
            "a transcluded section says where it is authored: {shown}"
        );
        assert!(
            !shown.contains("only true of prod rollouts"),
            "an unpromoted section stays home: {shown}"
        );
    }

    #[test]
    fn write_upserts_and_never_silently_drops_a_section() {
        // A section carries its own tags, citations and history, and other
        // notes may resolve it — so an incremental write must not destroy one
        // that simply was not mentioned.
        let db = db();
        write(&*db, "nased", "## A\nfirst.\n\n## B\nsecond.");
        write(&*db, "nased", "## A\nrewritten.");
        let note = db.note_by_path("nased").unwrap().unwrap();
        let headings: Vec<String> = db
            .sections_for_note(note.id)
            .unwrap()
            .into_iter()
            .map(|s| s.heading)
            .collect();
        assert_eq!(headings, vec!["A".to_string(), "B".to_string()]);

        // Removing one is explicit, and says what is there when it misses.
        let miss = Collect::new();
        assert_eq!(run_notes(&argv(&["rm", "nased", "C"]), &miss.deps(&*db)), 1);
        assert!(miss.errors().contains("it has: A, B"), "{}", miss.errors());

        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["rm", "nased", "B"]), &c.deps(&*db)), 0);
        assert_eq!(db.sections_for_note(note.id).unwrap().len(), 1);
    }

    #[test]
    fn the_footer_names_the_most_distinctive_tag_not_the_first() {
        // `nased:backfill:dev` handed over `dev`, which is a useless next
        // command. idf picks the word the note is actually about.
        let db = db();
        for _ in 0..3 {
            seed_command(&*db, "dev", 10, "s1");
        }
        seed_command(&*db, "nased", 20, "s1");
        write(&*db, "nased:dev", "prose");
        let c = Collect::new();
        run_notes(&argv(&["show", "nased:dev"]), &c.deps(&*db));
        assert!(
            c.printed().contains("bough tags show nased"),
            "{}",
            c.printed()
        );
    }

    #[test]
    fn own_only_suppresses_what_resolves_in() {
        let db = db();
        write(&*db, "nased:a", "## Shared\ntags: nased\npromoted.");
        write(&*db, "nased:b", "## Mine\nlocal.");
        let c = Collect::new();
        run_notes(&argv(&["show", "nased:b", "--own"]), &c.deps(&*db));
        assert!(c.printed().contains("local."));
        assert!(!c.printed().contains("promoted."));
    }

    #[test]
    fn a_rewrite_keeps_the_superseded_claim_on_the_record() {
        // What makes resolving a contradiction auditable rather than a silent
        // overwrite — the failure that makes model-arbitrated memory
        // untrustworthy.
        let db = db();
        write(&*db, "pr.7134", "## Status\nthe cutover is blocked.");
        write(&*db, "pr.7134", "## Status\nthe cutover merged green.");

        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["history", "pr.7134"]), &c.deps(&*db)), 0);
        let shown = c.printed();
        assert!(shown.contains("the cutover merged green"), "{shown}");
        assert!(
            shown.contains("the cutover is blocked"),
            "the old claim survives: {shown}"
        );
        assert!(shown.contains("rev 1"));
    }

    #[test]
    fn a_citation_that_does_not_resolve_is_named_and_not_recorded() {
        let db = db();
        seed_command(&*db, "nased", 100, "s1");
        // 1 exists and carries `nased`; 999 does not exist at all.
        write(
            &*db,
            "nased",
            "grounded in [cmd:1] but also claims [cmd:999].",
        );
        let c = Collect::new();
        run_notes(&argv(&["cites", "nased"]), &c.deps(&*db));
        assert!(c.printed().contains("command:1"), "{}", c.printed());
        assert!(!c.printed().contains("999"), "the invented id was dropped");
    }

    #[test]
    fn cites_says_which_claims_rest_on_nothing() {
        let db = db();
        write(
            &*db,
            "nased",
            "## Hunch\nI think the executor is the problem.",
        );
        let c = Collect::new();
        run_notes(&argv(&["cites", "nased"]), &c.deps(&*db));
        assert!(c.printed().contains("(uncited)"), "{}", c.printed());
    }

    #[test]
    fn an_appended_line_records_who_wrote_it() {
        let db = db();
        write(&*db, "pr.7134", "prose");

        let human = Collect::new();
        assert_eq!(
            run_notes(
                &argv(&["append", "pr.7134", "cutover merged"]),
                &human.deps(&*db)
            ),
            0
        );
        assert!(human.printed().starts_with('*'));

        let mut model = Collect::new();
        model.session = Some("s1".into());
        assert_eq!(
            run_notes(
                &argv(&["append", "pr.7134", "backfill closed"]),
                &model.deps(&*db)
            ),
            0
        );
        assert!(model.printed().starts_with('+'));

        let note = db.note_by_path("pr.7134").unwrap().unwrap();
        let log = db.note_log(note.id, 10).unwrap();
        assert_eq!(log[0].source, NoteAuthor::Human);
        assert_eq!(log[1].source, NoteAuthor::Session);
        assert_eq!(
            db.sections_for_note(note.id).unwrap()[0].body,
            "prose",
            "an append never touches the prose"
        );
    }

    #[test]
    fn the_tree_shows_stubs_for_nodes_nothing_was_written_at() {
        let db = db();
        write(&*db, "kubectl:rollout:nased", "deep.");
        write(&*db, "nased", "top.");
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&["tree"]), &c.deps(&*db)), 0);
        let shown = c.printed();
        assert!(shown.contains("kubectl/"), "a stub: {shown}");
        assert!(shown.contains("rollout/"), "a stub: {shown}");
        assert!(shown.contains("nased   nased"), "a real note: {shown}");
    }

    #[test]
    fn stale_counts_a_two_tag_note_once_per_command() {
        // A per-tag sum would report a two-tag note as twice as stale.
        let db = db();
        write(&*db, "kubectl:nased", "prose");
        seed_command(&*db, "kubectl:nased", 500, "s1");
        let c = Collect::new();
        run_notes(&argv(&["stale", "--json"]), &c.deps(&*db));
        let rows: serde_json::Value = serde_json::from_str(&c.printed()).unwrap();
        assert_eq!(rows[0]["unfolded"], 1);
    }

    #[test]
    fn search_finds_a_section_by_its_body() {
        let db = db();
        write(
            &*db,
            "nased",
            "## Ordering\nthe executor swap needs the dag removal",
        );
        let hit = Collect::new();
        assert_eq!(
            run_notes(&argv(&["search", "executor", "dag"]), &hit.deps(&*db)),
            0
        );
        assert!(hit.printed().contains("nased"));

        let miss = Collect::new();
        assert_eq!(
            run_notes(&argv(&["search", "kubernetes"]), &miss.deps(&*db)),
            1
        );
        assert!(miss.errors().contains("command_history_fts"));
    }

    #[test]
    fn an_empty_memory_is_exit_one_not_a_crash() {
        let db = db();
        let c = Collect::new();
        assert_eq!(run_notes(&argv(&[]), &c.deps(&*db)), 1);
        assert!(c.errors().contains("bough notes write"));
    }
}
