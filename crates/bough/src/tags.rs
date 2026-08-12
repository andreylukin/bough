//! `bough tags` — what the command memory knows, and what it tells the model
//! (port of `src/cli/tags.ts`).
//!
//! WHY THIS EXISTS. The tag memory has been shaping every turn from behind a
//! curtain: a priming note the user never sees, ranked by arithmetic they cannot
//! inspect, over a table they can only reach through `sqlite3` or by asking the
//! agent to query itself. The default view IS the priming note's ranking, with
//! the numbers it sorted by; `show` is the human's `history.sql()`; `stats` is
//! the measurement the whole tag arc has been missing.
//!
//! NO SERVER, AND ONLY READS. The database is opened directly — every query
//! here is a SELECT, and they live in `db/sqlite_db.rs` beside the ones the
//! prompt uses so the ranking this prints cannot drift from the ranking the
//! model gets.
//!
//! Conventions are `mcp.rs`'s: parsing is pure and total, effects are injected,
//! `run_tags` returns an exit code and never touches a real process.
//!
//! Exit codes:
//!
//!   0  answered
//!   1  there is no command memory yet
//!   2  usage problem
//!
//! NOT ASYNC, unlike the TS: the Rust embed layer is synchronous (rusqlite plus
//! a CPU-bound extension), so `similar` has nothing to await. The exit codes and
//! every printed string are unchanged.

use std::sync::Arc;

use bough_core::db::embed::create_embed_layer;
use bough_core::db::sqlite_db::{open_db, DbOptions};
use bough_core::history::tags::stats::{ranked_repo_tags, workspace_repo, RankedTag};
use bough_core::paths::db_path;
use bough_core::types::{Db, TagDiversityDay, TaggedCommand};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagsVerb {
    List,
    Show,
    Stats,
    Sql,
    Similar,
}

/// Bounded so one greedy SELECT cannot flood a terminal — or a tool result.
const MAX_ROWS: usize = 200;

/// The tables a query may read. Names only — the message a refusal shows.
const SURFACE: &str =
    "command_history, command_tags, command_dirs, command_history_fts, messages, messages_fts, sessions, turns";

#[derive(Debug, Clone, PartialEq)]
pub struct TagsArgs {
    pub verb: TagsVerb,
    /// `show` only: the tag to open. Also holds the `sql` query / `similar` text.
    pub tag: Option<String>,
    /// A repo identity (git origin URL, or a path). Absent = this checkout's.
    pub repo: Option<String>,
    /// `--all`: no repo scope at all, so the memory answers across projects.
    pub all_repos: bool,
    pub limit: usize,
    pub days: i64,
    pub json: bool,
    /// `show`: print the whole program each command ran in, not just its size.
    pub program: bool,
}

impl Default for TagsArgs {
    fn default() -> Self {
        TagsArgs {
            verb: TagsVerb::List,
            tag: None,
            repo: None,
            all_repos: false,
            limit: 20,
            days: 30,
            json: false,
            program: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Args(TagsArgs),
    UsageError(String),
}

pub const USAGE: &str = "usage: bough tags [VERB] [OPTIONS]

  (none)          this project's tag vocabulary — what the model is primed with
  show TAG        the commands recorded under TAG, newest first
  stats           tag coverage and vocabulary per day — did anything change?
  sql QUERY       a read-only SELECT over the memory and the transcripts
  similar TEXT    semantic recall, where the local vector layer exists

  --repo R        scope to a repo identity (origin URL or path); default: here
  --all           every repo the memory knows, not just this one
  --program       show: print the program each command ran in, not just its size
  --limit N       rows (default 20)
  --days N        stats: how far back to look (default 30)
  --json          machine-readable output
  -h, --help      this

exit: 0 answered · 1 no command memory yet · 2 usage";

/// Pure and total: arguments in, arguments or a usage error out. Never panics.
pub fn parse_tags_args(argv: &[String]) -> Parsed {
    let mut args = TagsArgs::default();
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
            "--program" => {
                args.program = true;
                i += 1;
                continue;
            }
            "--repo" => {
                let Some(v) = argv.get(i + 1) else {
                    return Parsed::UsageError(format!("--repo needs a value\n{USAGE}"));
                };
                args.repo = Some(v.clone());
                i += 2;
                continue;
            }
            "--limit" | "--days" => {
                let value = argv.get(i + 1);
                let n = value.and_then(|v| v.parse::<f64>().ok());
                match n {
                    Some(n) if n.is_finite() && n > 0.0 => {
                        if a == "--limit" {
                            args.limit = n.trunc() as usize;
                        } else {
                            args.days = n.trunc() as i64;
                        }
                    }
                    _ => {
                        return Parsed::UsageError(format!("{a} needs a positive number\n{USAGE}"))
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
    match first.as_deref() {
        Some(v @ ("sql" | "similar")) => {
            if rest != 1 {
                return Parsed::UsageError(format!(
                    "{v} needs exactly one quoted argument\n{USAGE}"
                ));
            }
            args.verb = if v == "sql" {
                TagsVerb::Sql
            } else {
                TagsVerb::Similar
            };
            args.tag = Some(positional[1].clone());
        }
        Some("show") => {
            if rest != 1 {
                return Parsed::UsageError(format!("show needs exactly one TAG\n{USAGE}"));
            }
            args.verb = TagsVerb::Show;
            args.tag = Some(positional[1].clone());
        }
        Some("stats") => {
            if rest > 0 {
                return Parsed::UsageError(format!("stats takes no arguments\n{USAGE}"));
            }
            args.verb = TagsVerb::Stats;
        }
        Some(word) => {
            // A bare word is the commonest thing to type and the likeliest to be
            // a tag. Guessing `show` for it beats a usage error that names three
            // verbs.
            if rest > 0 {
                return Parsed::UsageError(format!("unknown verb {word}\n{USAGE}"));
            }
            args.verb = TagsVerb::Show;
            args.tag = Some(word.to_string());
        }
        None => {}
    }
    // `--all` is the absence of a scope, and it must beat an explicit `--repo`:
    // asking for everything after naming one is a correction, not a
    // contradiction.
    if args.all_repos {
        args.repo = None;
    }
    Parsed::Args(args)
}

/// The vector layer as this command needs it, injected so a test needs no
/// extensions.
pub trait SimilarLayer {
    fn similar(&self, text: &str) -> Result<Vec<Value>, String>;
    fn close(&self);
}

pub struct TagsDeps<'a> {
    pub db: Option<&'a dyn Db>,
    /// The file `sql` opens read-only. Absent = the live `paths::db_path()`.
    pub db_file: Option<String>,
    /// The vector layer factory. Absent = the real `create_embed_layer`.
    pub embed: Option<Arc<dyn Fn() -> Option<Box<dyn SimilarLayer>>>>,
    /// Where "this checkout" is resolved from. Absent = the process's cwd.
    pub cwd: Option<String>,
    pub now: Option<i64>,
    pub out: Arc<dyn Fn(&str)>,
    pub err: Arc<dyn Fn(&str)>,
}

impl<'a> TagsDeps<'a> {
    pub fn real() -> TagsDeps<'a> {
        TagsDeps {
            db: None,
            db_file: None,
            embed: None,
            cwd: None,
            now: None,
            out: Arc::new(|l: &str| println!("{l}")),
            err: Arc::new(|l: &str| eprintln!("{l}")),
        }
    }
}

/// The real layer, behind the injected-factory shape.
struct RealLayer(bough_core::db::embed::EmbedLayer);

impl SimilarLayer for RealLayer {
    fn similar(&self, text: &str) -> Result<Vec<Value>, String> {
        self.0.similar(text).map_err(|e| e.to_string())
    }
    fn close(&self) {
        // `EmbedLayer::close` consumes; dropping the handle closes the
        // connection just the same, and the process is about to exit.
    }
}

/// The note above the commands, when there is one.
///
/// THE JOIN, in the one place it earns its keep: `show` is the command already
/// being run, so the prose arrives without a second habit. Sections resolved
/// from ELSEWHERE are included and say where they are authored — a lesson
/// about `atlas` is worth reading here whichever page it was written on.
fn render_note_header(db: &dyn Db, tag: &str, out: &dyn Fn(&str)) {
    let context = vec![tag.to_string()];
    let Ok(sections) = db.sections_for_context(&context, None) else {
        return;
    };
    if sections.is_empty() {
        return;
    }
    let spread = db
        .tag_spread(None)
        .map(|(repos, by_tag)| bough_core::history::tags::stats::TagSpread { repos, by_tag })
        .unwrap_or_default();
    let ranked = bough_core::notes::resolve::rank(&spread, sections, &context, None);
    if ranked.is_empty() {
        return;
    }
    out("note");
    for r in ranked.iter().take(3) {
        out(&format!(
            "  ## {}   ← {}",
            r.section.heading, r.section.note_path
        ));
        for line in r.section.body.trim().lines().take(4) {
            out(&format!("     {line}"));
        }
    }
    if let Ok(Some(note)) = db.note_by_path(tag) {
        for l in db.note_log(note.id, 3).unwrap_or_default() {
            out(&format!("  {} {}", l.source.glyph(), l.text));
        }
    }
    out(&format!("  (bough notes show {tag})"));
    out("");
}

/// A read-only SELECT over the whole database — what `history.sql()` used to be,
/// and now the only door to it.
///
/// READ-ONLY IS ENFORCED TWICE, both at the connection: the handle is opened
/// `SQLITE_OPEN_READ_ONLY` AND `PRAGMA query_only = ON`, which also covers
/// anything a clever statement ATTACHes. The keyword check on top exists only to
/// answer a write attempt with a sentence instead of a bare SQLITE_READONLY.
fn query_sql(path: &str, sql: &str) -> Result<Vec<Obj>, String> {
    let head: String = strip_leading_trivia(sql)
        .chars()
        .take(8)
        .collect::<String>()
        .to_uppercase();
    if !head.starts_with("SELECT") && !head.starts_with("WITH") {
        return Err(format!(
            "read-only: a query must start with SELECT or WITH. Queryable: {SURFACE}."
        ));
    }
    run_readonly(path, sql).map_err(|e| format!("{e}. Queryable: {SURFACE}."))
}

/// Drop leading whitespace and comments so `-- note\nSELECT …` still reads as a
/// SELECT (the TS regex, ported).
fn strip_leading_trivia(sql: &str) -> &str {
    let mut s = sql;
    loop {
        let t = s.trim_start();
        if let Some(rest) = t.strip_prefix("--") {
            s = match rest.find('\n') {
                Some(i) => &rest[i + 1..],
                None => "",
            };
            continue;
        }
        if let Some(rest) = t.strip_prefix("/*") {
            s = match rest.find("*/") {
                Some(i) => &rest[i + 2..],
                None => "",
            };
            continue;
        }
        return t;
    }
}

fn run_readonly(path: &str, sql: &str) -> Result<Vec<Obj>, rusqlite::Error> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.execute_batch("PRAGMA query_only = ON")?;
    // A concurrent writer holding the journal must surface as a brief wait, not
    // as a spurious "database is locked".
    conn.execute_batch("PRAGMA busy_timeout = 2000")?;
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        if out.len() >= MAX_ROWS {
            break;
        }
        // The driver's COLUMN ORDER, not an alphabetized map: `SELECT a, b`
        // must print `a` first, which is what the caller wrote it for.
        let mut obj = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            obj.push((leak(name), value_of(row.get_ref(i)?)));
        }
        out.push(Obj(obj));
    }
    Ok(out)
}

/// Column names come from the statement, not from a literal. The set is tiny
/// and bounded by one query; leaking it buys the one ordered-object shape.
fn leak(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn value_of(v: rusqlite::types::ValueRef<'_>) -> Value {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        // A blob has no JSON form; its size is the honest answer.
        ValueRef::Blob(b) => json!(format!("<blob {} bytes>", b.len())),
    }
}

/// The KNN row's field order, as `specs/history.md` §3 pins it.
const SIMILAR_KEYS: [&str; 6] = ["cmd", "tags", "repo", "exit_code", "ts", "distance"];

/// `2 days ago`, `3h ago` — a timestamp a reader can place without arithmetic.
fn ago(ts: i64, now: i64) -> String {
    let s = (((now - ts) as f64) / 1000.0).round().max(0.0);
    if s < 90.0 {
        return format!("{}s ago", s as i64);
    }
    let m = (s / 60.0).round();
    if m < 90.0 {
        return format!("{}m ago", m as i64);
    }
    let h = (m / 60.0).round();
    if h < 48.0 {
        return format!("{}h ago", h as i64);
    }
    format!("{}d ago", (h / 24.0).round() as i64)
}

/// Right-pad to `w` display columns. Tags and numbers here are ASCII.
fn pad(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - s.len()))
    }
}

fn lpad(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        format!("{}{s}", " ".repeat(w - s.len()))
    }
}

fn render_list(ranked: &[RankedTag], repo: &str, out: &dyn Fn(&str)) {
    out(repo);
    if ranked.is_empty() {
        out("  no tagged commands yet — the model tags them as it runs them");
        return;
    }
    out("");
    out(&format!(
        "  {}{}{}{}",
        pad("tag", 24),
        lpad("weight", 8),
        lpad("repos", 7),
        lpad("score", 8)
    ));
    for r in ranked {
        out(&format!(
            "  {}{}{}{}",
            pad(&r.tag, 24),
            lpad(&format!("{:.1}", r.weight), 8),
            lpad(&r.repos.to_string(), 7),
            lpad(&format!("{:.1}", r.score), 8),
        ));
    }
    out("");
    // The ordering rule, said once, because a table sorted by a column it does
    // not show is the kind of thing that reads as a bug.
    out("  ranked by weight × how FEW repos use the tag: a word every project uses");
    out("  names a tool, and this list is for the words that name this project.");
}

fn render_show(
    rows: &[TaggedCommand],
    tag: &str,
    now: i64,
    out: &dyn Fn(&str),
    program_: &dyn Fn(&str) -> Option<String>,
    show_program: bool,
) {
    if rows.is_empty() {
        out(&format!("no commands tagged \"{tag}\""));
        return;
    }
    out(&format!(
        "{} command{} tagged \"{tag}\"",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    ));
    out("");
    let mut last_tags = String::new();
    for r in rows {
        // The full tag string only when it CHANGES. Every row under one tag
        // usually carries the same one, and repeating it doubles the output to
        // say nothing.
        if r.tags != last_tags {
            out(&format!("  {}", r.tags));
            last_tags = r.tags.clone();
        }
        // The exit code first, because "what worked here" is the question this
        // answers.
        let mark = match r.exit_code {
            Some(0) => "✓",
            None => "·",
            _ => "✗",
        };
        let cmd = collapse_ws(&r.cmd);
        let cmd: String = cmd.chars().take(96).collect();
        out(&format!("    {mark} {} {cmd}", pad(&ago(r.ts, now), 9)));
        // …and the round it ran in, because on anything but a one-liner the
        // program is the reusable part and the command is a line inside it.
        let program = r.message_id.as_deref().and_then(program_);
        let Some(program) = program else { continue };
        if show_program {
            for line in program.split('\n') {
                out(&format!("      │ {line}"));
            }
        } else {
            let lines = program.split('\n').count();
            out(&format!(
                "      ↳ program: {lines} line{} · --program to see it",
                if lines == 1 { "" } else { "s" }
            ));
        }
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

fn render_stats(days: &[TagDiversityDay], out: &dyn Fn(&str)) {
    if days.is_empty() {
        out("no commands in that window");
        return;
    }
    out(&format!(
        "  {}{}{}{}{}{}{}{}",
        pad("day", 12),
        lpad("sessions", 9),
        lpad("cmds", 6),
        lpad("tagged", 8),
        lpad("vocab", 7),
        lpad("refs", 6),
        lpad("uses", 6),
        lpad("once", 6),
    ));
    for d in days {
        // `tagged` as a share, because the absolute count says nothing without
        // the total — and the share is the number that moves when a leg goes
        // untagged.
        let share = if d.commands == 0 {
            "—".to_string()
        } else {
            format!(
                "{}%",
                ((d.tagged as f64 / d.commands as f64) * 100.0).round() as i64
            )
        };
        // As a share of the day's vocabulary, for the same reason `tagged` is a
        // share: 572 singletons means nothing until you know it is 40% of 1,431.
        let once = if d.distinct_tags == 0 {
            "—".to_string()
        } else {
            format!(
                "{}%",
                ((d.singletons as f64 / d.distinct_tags as f64) * 100.0).round() as i64
            )
        };
        out(&format!(
            "  {}{}{}{}{}{}{}{}",
            pad(&d.day, 12),
            lpad(&d.sessions.to_string(), 9),
            lpad(&d.commands.to_string(), 6),
            lpad(&share, 8),
            lpad(&d.distinct_tags.to_string(), 7),
            lpad(&d.distinct_refs.to_string(), 6),
            lpad(&d.tag_uses.to_string(), 6),
            lpad(&once, 6),
        ));
    }
    out("");
    out("  vocab is DISTINCT coined tags that day; refs are `linear.*`-style pointers,");
    out("  counted apart so a busy ticket week does not read as a richer vocabulary;");
    out("  uses is how often any tag was applied. vocab rising with uses flat is the");
    out("  model naming more things, which is the point; uses rising with vocab flat is");
    out("  it repeating itself. once is the share of that day's vocabulary used");
    out("  EXACTLY ONCE — words that named something and were never reached for");
    out("  again. It is never zero (vocabulary growth is a power law); what matters");
    out("  is the share moving on a date a hygiene or prompt change landed.");
}

/// One JSON object with its keys in a FIXED order.
///
/// `serde_json`'s map is a `BTreeMap`, so `json!{}` would sort the keys — and
/// `JSON.stringify` emits them in field order. The bytes are the wire here (a
/// `--json` consumer diffing two harnesses sees key order), so these three
/// shapes are rendered from an ordered pair list instead. Every value in them
/// is a scalar, which is why this can stay this small.
struct Obj(Vec<(&'static str, Value)>);

fn ranked_json(r: &RankedTag) -> Obj {
    Obj(vec![
        ("tag", json!(r.tag)),
        ("weight", json!(r.weight)),
        ("repos", json!(r.repos)),
        ("score", json!(r.score)),
    ])
}

/// The `--json` wire shape, camelCase, exactly the TS row objects — including
/// their key ORDER.
fn command_json(r: &TaggedCommand, program: Option<String>) -> Obj {
    Obj(vec![
        ("ts", json!(r.ts)),
        ("repo", json!(r.repo)),
        ("cmd", json!(r.cmd)),
        ("tags", json!(r.tags)),
        ("exitCode", json!(r.exit_code)),
        ("durationMs", json!(r.duration_ms)),
        ("sessionId", json!(r.session_id)),
        ("messageId", json!(r.message_id)),
        ("program", json!(program)),
    ])
}

fn day_json(d: &TagDiversityDay) -> Obj {
    Obj(vec![
        ("day", json!(d.day)),
        ("sessions", json!(d.sessions)),
        ("commands", json!(d.commands)),
        ("tagged", json!(d.tagged)),
        ("distinctTags", json!(d.distinct_tags)),
        ("distinctRefs", json!(d.distinct_refs)),
        ("tagUses", json!(d.tag_uses)),
        ("singletons", json!(d.singletons)),
    ])
}

/// `JSON.stringify(rows, null, 2)` over an array of ordered objects, byte for
/// byte. Values are scalars, so their pretty form is their compact form.
fn print_rows(out: &dyn Fn(&str), rows: &[Obj]) {
    if rows.is_empty() {
        out("[]");
        return;
    }
    let mut text = String::from("[\n");
    for (i, row) in rows.iter().enumerate() {
        text.push_str("  {\n");
        for (j, (key, value)) in row.0.iter().enumerate() {
            text.push_str("    ");
            text.push_str(&Value::String((*key).to_string()).to_string());
            text.push_str(": ");
            text.push_str(&value.to_string());
            if j + 1 < row.0.len() {
                text.push(',');
            }
            text.push('\n');
        }
        text.push_str("  }");
        if i + 1 < rows.len() {
            text.push(',');
        }
        text.push('\n');
    }
    text.push(']');
    out(&text);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Run the command. Returns an exit code; every effect is injected, so the whole
/// thing is testable against an in-memory database and two collectors.
pub fn run_tags(argv: &[String], deps: &TagsDeps<'_>) -> i32 {
    let parsed = match parse_tags_args(argv) {
        Parsed::UsageError(message) => {
            (deps.err)(&message);
            // Help is exit 0; anything else is a usage failure.
            return if message == USAGE { 0 } else { 2 };
        }
        Parsed::Args(a) => a,
    };
    let now = deps.now.unwrap_or_else(now_ms);

    // `sql` never touches the injected handle — it opens its own read-only one.
    if parsed.verb == TagsVerb::Sql {
        let file = deps
            .db_file
            .clone()
            .unwrap_or_else(|| db_path().to_string_lossy().into_owned());
        return match query_sql(&file, parsed.tag.as_deref().unwrap_or("")) {
            Err(error) => {
                (deps.err)(&error);
                2
            }
            Ok(rows) => {
                print_rows(&*deps.out, &rows);
                0
            }
        };
    }

    if parsed.verb == TagsVerb::Similar {
        let layer = match &deps.embed {
            Some(factory) => factory(),
            None => {
                create_embed_layer(None).map(|l| Box::new(RealLayer(l)) as Box<dyn SimilarLayer>)
            }
        };
        let Some(layer) = layer else {
            (deps.err)(
                "no local embedding layer here, so there is nothing to be similar with. \
Keyword search always works: bough tags sql \"SELECT h.cmd FROM \
command_history_fts f JOIN command_history h ON h.id = f.command_id \
WHERE f.cmd MATCH 'docker' ORDER BY h.ts DESC LIMIT 10\"",
            );
            return 1;
        };
        let answer = layer.similar(parsed.tag.as_deref().unwrap_or(""));
        // `finally`: the handle closes whichever way the query went.
        layer.close();
        return match answer {
            Ok(rows) => {
                let rows: Vec<Obj> = rows
                    .into_iter()
                    .take(MAX_ROWS)
                    .map(|r| {
                        Obj(SIMILAR_KEYS
                            .iter()
                            .map(|k| (*k, r.get(*k).cloned().unwrap_or(Value::Null)))
                            .collect())
                    })
                    .collect();
                print_rows(&*deps.out, &rows);
                0
            }
            Err(error) => {
                (deps.err)(&format!("similar failed: {error}"));
                1
            }
        };
    }

    // Everything below reads the memory. An absent database is not an error to
    // report as a crash — it is "nothing has run through bough yet".
    let owned;
    let db: &dyn Db = match deps.db {
        Some(db) => db,
        None => {
            let path = db_path();
            if !path.exists() {
                (deps.err)(&format!(
                    "no command memory yet at {} — run something through bough first",
                    path.display()
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
    // `--all` is the one way to see across projects; otherwise the scope is this
    // checkout's identity, which is what the memory is keyed by.
    let repo: Option<String> = if parsed.all_repos {
        None
    } else {
        Some(parsed.repo.clone().unwrap_or_else(|| workspace_repo(&cwd)))
    };

    match parsed.verb {
        TagsVerb::Show => {
            let tag = parsed.tag.clone().unwrap_or_default();
            let rows = match db.commands_for_tag(&tag, repo.as_deref(), Some(parsed.limit as i64)) {
                Ok(rows) => rows,
                Err(error) => {
                    (deps.err)(&error.to_string());
                    return 1;
                }
            };
            let program_of = |id: &str| db.program_for_message(id).ok().flatten();
            if parsed.json {
                let rows: Vec<Obj> = rows
                    .iter()
                    .map(|r| command_json(r, r.message_id.as_deref().and_then(program_of)))
                    .collect();
                if let Ok(sections) = db.sections_for_context(std::slice::from_ref(&tag), None) {
                    if !sections.is_empty() {
                        (deps.out)(
                            &json!({
                                "notes": sections.iter().map(|s| json!({
                                    "heading": s.heading,
                                    "body": s.body,
                                    "authored_in": s.note_path,
                                })).collect::<Vec<_>>()
                            })
                            .to_string(),
                        );
                    }
                }
                print_rows(&*deps.out, &rows);
            } else {
                // A missing note memory is silence, never an error — this
                // cannot break the command it decorates.
                render_note_header(db, &tag, &*deps.out);
                render_show(&rows, &tag, now, &*deps.out, &program_of, parsed.program);
            }
            0
        }
        TagsVerb::Stats => {
            let since = now - parsed.days * 24 * 60 * 60 * 1000;
            let rows = match db.tag_diversity_by_day(since, repo.as_deref()) {
                Ok(rows) => rows,
                Err(error) => {
                    (deps.err)(&error.to_string());
                    return 1;
                }
            };
            if parsed.json {
                // The RAW rows, unsliced — the limit is a rendering budget.
                let rows: Vec<Obj> = rows.iter().map(day_json).collect();
                print_rows(&*deps.out, &rows);
            } else {
                render_stats(&rows[..rows.len().min(parsed.limit)], &*deps.out);
            }
            0
        }
        _ => {
            // The default view is the priming note's own ranking. A repo-less
            // (`--all`) list has no project to be distinctive against, so it is
            // scoped to the checkout — there is nothing meaningful to rank
            // "every project's tags" by.
            let scope = repo.unwrap_or_else(|| workspace_repo(&cwd));
            let ranked = match ranked_repo_tags(db, &scope, now, parsed.limit) {
                Ok(r) => r,
                Err(error) => {
                    (deps.err)(&error.to_string());
                    return 1;
                }
            };
            if parsed.json {
                let rows: Vec<Obj> = ranked.iter().map(ranked_json).collect();
                print_rows(&*deps.out, &rows);
            } else {
                render_list(&ranked, &scope, &*deps.out);
            }
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_core::db::sqlite_db::SqliteDb;
    use bough_core::history::tags::stats::reset_stats_memo;
    use bough_core::schema::parts::{Message, Part, Role, Session, SessionKind};
    use bough_core::types::CommandRecord;
    use std::cell::RefCell;
    use std::rc::Rc;

    const T0: i64 = 1_785_499_200_000; // 2026-08-03T12:00:00Z
    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[derive(Clone, Default)]
    struct Collector {
        out: Rc<RefCell<Vec<String>>>,
        err: Rc<RefCell<Vec<String>>>,
    }

    impl Collector {
        // `TagsDeps`'s sinks are `Arc<dyn Fn(&str)>` with no `Send + Sync` — the
        // CLI runs on one thread — so a collector closure over `RefCell` is a
        // legal inhabitant of the field even though it is not shareable.
        #[allow(clippy::arc_with_non_send_sync)]
        fn deps<'a>(&self, db: Option<&'a dyn Db>) -> TagsDeps<'a> {
            let out = self.out.clone();
            let err = self.err.clone();
            TagsDeps {
                db,
                db_file: None,
                embed: None,
                cwd: None,
                now: Some(T0),
                out: Arc::new(move |l: &str| out.borrow_mut().push(l.to_string())),
                err: Arc::new(move |l: &str| err.borrow_mut().push(l.to_string())),
            }
        }
        fn text(&self) -> String {
            self.out.borrow().join("\n")
        }
        fn errs(&self) -> String {
            self.err.borrow().join("\n")
        }
    }

    fn record(db: &SqliteDb, repo: &str, cmd: &str, tags: &str, exit: i64, ts: i64) {
        record_with(db, repo, cmd, tags, exit, ts, None);
    }

    fn record_with(
        db: &SqliteDb,
        repo: &str,
        cmd: &str,
        tags: &str,
        exit: i64,
        ts: i64,
        message_id: Option<&str>,
    ) {
        db.record_command(&CommandRecord {
            session_id: "s1".into(),
            ts,
            repo: repo.into(),
            cmd: cmd.into(),
            tags: tags.into(),
            tag_list: if tags.is_empty() {
                vec![]
            } else {
                tags.split(':').map(str::to_string).collect()
            },
            dirs: vec![],
            exit_code: Some(exit),
            duration_ms: Some(40),
            output_head: String::new(),
            spill_path: None,
            source: "live".into(),
            message_id: message_id.map(str::to_string),
        })
        .unwrap();
    }

    /// A session row, only the fields this file cares about.
    fn session_row(id: &str, title: &str) -> Session {
        Session {
            id: id.into(),
            title: title.into(),
            kind: SessionKind::Root,
            created_at: T0,
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
        }
    }

    fn session(db: &SqliteDb) {
        db.create_session(session_row("s1", "t")).unwrap();
    }

    /// A memory with two repos, so distinctiveness has something to contrast
    /// against.
    fn seeded() -> SqliteDb {
        let db = open_db(Some(":memory:"), DbOptions::default()).unwrap();
        session(&db);
        // `git` is used more than `composer` in THIS repo, and is also used in
        // the other one. That is the whole case the ranking exists for.
        for i in 0..6 {
            record(
                &db,
                "mine",
                &format!("git status {i}"),
                "git:status:worktree",
                0,
                T0 - i,
            );
        }
        for i in 0..3 {
            record(
                &db,
                "mine",
                &format!("bun test {i}"),
                "bun:test:composer",
                0,
                T0 - i,
            );
        }
        record(
            &db,
            "mine",
            "bun test failing",
            "bun:test:composer",
            1,
            T0 - 10,
        );
        // …and `git` is in every OTHER repo the memory knows, which is what
        // makes it a tool name.
        for i in 0..4 {
            record(
                &db,
                "other",
                &format!("git push {i}"),
                "git:push:main",
                0,
                T0 - i,
            );
        }
        for r in 2..7 {
            record(
                &db,
                &format!("other{r}"),
                "git log",
                "git:log:history",
                0,
                T0 - r,
            );
        }
        // A day earlier, and an untagged leg — what `stats` reports as lost
        // coverage.
        record(&db, "mine", "rg todo", "rg:search:todo", 0, T0 - DAY);
        record(&db, "mine", "echo untagged", "", 0, T0 - DAY);
        db
    }

    // tags.test.ts: "parsing is pure and total, and a bare word is a tag"
    #[test]
    fn parsing_is_pure_and_total_and_a_bare_word_is_a_tag() {
        assert_eq!(parse_tags_args(&[]), Parsed::Args(TagsArgs::default()));
        match parse_tags_args(&argv(&["show", "git"])) {
            Parsed::Args(a) => {
                assert_eq!(a.verb, TagsVerb::Show);
                assert_eq!(a.tag.as_deref(), Some("git"));
            }
            other => panic!("{other:?}"),
        }
        // `bough tags git` is what a hand reaches for, and it means `show git`.
        match parse_tags_args(&argv(&["git"])) {
            Parsed::Args(a) => {
                assert_eq!(a.verb, TagsVerb::Show);
                assert_eq!(a.tag.as_deref(), Some("git"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(parse_tags_args(&argv(&["-h"])), Parsed::UsageError(m) if m == USAGE));
        for (cli, needle) in [
            (argv(&["--limit", "0"]), "positive"),
            (argv(&["--repo"]), "needs a value"),
            (argv(&["--nope"]), "unknown option"),
            (argv(&["show"]), "exactly one TAG"),
            (argv(&["stats", "x"]), "stats takes no arguments"),
            (argv(&["sql"]), "exactly one quoted argument"),
        ] {
            match parse_tags_args(&cli) {
                Parsed::UsageError(m) => assert!(m.contains(needle), "{cli:?}: {m}"),
                other => panic!("{cli:?} → {other:?}"),
            }
        }
        // `--all` after `--repo` is a correction, not a contradiction.
        match parse_tags_args(&argv(&["--repo", "x", "--all"])) {
            Parsed::Args(a) => assert_eq!(a.repo, None),
            other => panic!("{other:?}"),
        }
    }

    // tags.test.ts: "--help is not a failure"
    #[test]
    fn help_is_not_a_failure() {
        let db = seeded();
        let c = Collector::default();
        assert_eq!(run_tags(&argv(&["--help"]), &c.deps(Some(&db))), 0);
        assert_eq!(c.err.borrow()[0], USAGE);
    }

    #[test]
    fn a_note_is_printed_above_the_commands_it_interprets() {
        // The join, in the one place it earns its keep: `show` is the command
        // the user already runs, so the prose arrives without a second habit.
        let db = seeded();
        let c = Collector::default();
        let note_id = db
            .upsert_note("bun", "Why bun and not node", &["bun".to_string()], T0)
            .unwrap();
        db.put_section(
            &bough_core::types::SectionWrite {
                note_id,
                ord: 0,
                heading: "Why bun".into(),
                body: "the TUI tests need bun's test runner".into(),
                tags: None,
                citations: vec![],
                author: bough_core::types::NoteAuthor::Human,
            },
            T0,
        )
        .unwrap();
        db.append_note_log(
            note_id,
            T0,
            bough_core::types::NoteAuthor::Cheap,
            "watch mode flakes in CI",
        )
        .unwrap();

        assert_eq!(
            run_tags(
                &argv(&["show", "bun", "--repo", "mine"]),
                &c.deps(Some(&db))
            ),
            0
        );
        let text = c.text();
        assert!(text.contains("## Why bun"), "{text}");
        assert!(text.contains("the TUI tests need bun's test runner"));
        assert!(text.contains("~ watch mode flakes in CI"), "{text}");
        assert!(text.contains("(bough notes show bun)"));
        let note_at = text.find("note").unwrap();
        let cmd_at = text.find("bun test").unwrap_or(usize::MAX);
        assert!(note_at < cmd_at, "the prose comes first");
    }

    #[test]
    fn a_tag_with_no_note_prints_exactly_what_it_always_did() {
        // A missing note memory is silence. This is what makes the join safe
        // to add to a command everything already depends on.
        let db = seeded();
        let c = Collector::default();
        assert_eq!(
            run_tags(
                &argv(&["show", "bun", "--repo", "mine"]),
                &c.deps(Some(&db))
            ),
            0
        );
        assert!(!c.text().contains("bough notes show"));
        assert!(c.errs().is_empty());
    }

    // tags.test.ts: "the default view is the priming note's ranking"
    #[test]
    fn the_default_view_is_the_priming_notes_ranking_arithmetic_shown() {
        reset_stats_memo();
        let db = seeded();
        let c = Collector::default();
        assert_eq!(run_tags(&argv(&["--repo", "mine"]), &c.deps(Some(&db))), 0);
        let text = c.text();
        // `git` outweighs `bun` here (6 successes to 3) and LOSES, because it is
        // used in both repos and `bun` in only this one. That inversion IS the
        // recommendation.
        let at = |t: &str| {
            text.find(&format!("\n  {t} "))
                .map(|i| i as i64)
                .unwrap_or(-1)
        };
        let composer = at("composer");
        let git = at("git");
        assert!(composer >= 0 && git >= 0, "{text}");
        assert!(
            composer < git,
            "this project's own word should outrank the tool:\n{text}"
        );
        assert!(text.contains("tag  "), "{text}");
        for column in ["tag", "weight", "repos", "score"] {
            assert!(text.contains(column), "{column} missing:\n{text}");
        }
        assert!(text.contains("how FEW repos use the tag"), "{text}");
    }

    // tags.test.ts: "show answers what worked, newest first, exit code first"
    #[test]
    fn show_answers_what_worked_with_the_exit_code_first() {
        let db = seeded();
        let c = Collector::default();
        assert_eq!(
            run_tags(
                &argv(&["show", "bun", "--repo", "mine"]),
                &c.deps(Some(&db))
            ),
            0
        );
        let text = c.text();
        assert!(text.contains("4 commands tagged \"bun\""), "{text}");
        assert!(text.contains("✓ ") && text.contains("bun test 0"), "{text}");
        assert!(
            text.lines()
                .any(|l| l.contains("✗") && l.contains("bun test failing")),
            "{text}"
        );
    }

    // tags.test.ts: "show is scoped to this repo unless --all says otherwise"
    #[test]
    fn show_is_scoped_to_this_repo_unless_all_says_otherwise() {
        let db = seeded();
        let mine = Collector::default();
        run_tags(
            &argv(&["show", "git", "--repo", "mine"]),
            &mine.deps(Some(&db)),
        );
        assert!(
            !mine.text().contains("git push"),
            "the other repo's commands leaked"
        );

        let all = Collector::default();
        run_tags(&argv(&["show", "git", "--all"]), &all.deps(Some(&db)));
        assert!(all.text().contains("git push"), "{}", all.text());
    }

    // tags.test.ts: "stats reports coverage and vocabulary per day"
    #[test]
    fn stats_reports_coverage_and_vocabulary_per_day() {
        let db = seeded();
        let c = Collector::default();
        assert_eq!(
            run_tags(&argv(&["stats", "--repo", "mine"]), &c.deps(Some(&db))),
            0
        );
        let text = c.text();
        for column in ["day", "sessions", "cmds", "tagged", "vocab", "refs", "uses"] {
            assert!(text.contains(column), "{column} missing:\n{text}");
        }
        // The day with the untagged leg reports less than 100% coverage.
        assert!(text.contains("50%"), "{text}");
        assert!(text.contains("100%"), "{text}");
    }

    // tags.test.ts: "--json is the same answer without the rendering"
    #[test]
    fn json_is_the_same_answer_without_the_rendering() {
        let db = seeded();
        let c = Collector::default();
        run_tags(
            &argv(&["show", "bun", "--repo", "mine", "--json"]),
            &c.deps(Some(&db)),
        );
        let rows: Vec<Value> = serde_json::from_str(&c.text()).unwrap();
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().any(|r| r["exitCode"] == json!(1)), "{rows:?}");
        // camelCase, the TS wire shape.
        assert!(rows[0].get("sessionId").is_some(), "{rows:?}");
    }

    // tags.test.ts: "a recalled command reaches the PROGRAM that ran it"
    #[test]
    fn a_recalled_command_reaches_the_program_that_ran_it() {
        let db = open_db(Some(":memory:"), DbOptions::default()).unwrap();
        session(&db);
        let program = "const r = await bash(\"psql -f migrations/004.sql\", \"psql:migrate:demand\");\nconsole.log(r);";
        db.create_message(Message {
            id: "m1".into(),
            session_id: "s1".into(),
            role: Role::Supervisor,
            parts: vec![
                Part::Text {
                    text: "running the migration".into(),
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": program }),
                },
            ],
            pending: false,
            created_at: T0,
        })
        .unwrap();
        record_with(
            &db,
            "mine",
            "psql -f migrations/004.sql",
            "psql:migrate:demand:linear.eng-1234",
            0,
            T0,
            Some("m1"),
        );

        // Recalled by the REFERENCE as readily as by a word — same table, same
        // join, which is the point of one namespace.
        let c = Collector::default();
        run_tags(
            &argv(&["show", "linear.eng-1234", "--repo", "mine"]),
            &c.deps(Some(&db)),
        );
        let text = c.text();
        assert!(text.contains("psql -f migrations/004.sql"), "{text}");
        // By default the program is a pointer.
        assert!(
            text.contains("↳ program: 2 lines · --program to see it"),
            "{text}"
        );
        assert!(!text.contains("console.log"), "{text}");

        let full = Collector::default();
        run_tags(
            &argv(&["show", "linear.eng-1234", "--repo", "mine", "--program"]),
            &full.deps(Some(&db)),
        );
        assert!(full.text().contains("│ console.log(r);"), "{}", full.text());

        // A row with no message still recalls, it just has no program to offer.
        record(
            &db,
            "mine",
            "psql -c 'select 1'",
            "psql:probe:demand",
            0,
            T0 - 5,
        );
        let old = Collector::default();
        run_tags(
            &argv(&["show", "probe", "--repo", "mine"]),
            &old.deps(Some(&db)),
        );
        assert!(old.text().contains("select 1"), "{}", old.text());
        assert!(!old.text().contains("↳ program"), "{}", old.text());
    }

    // tags.test.ts: "references are recalled but never primed"
    #[test]
    fn references_are_recalled_but_never_primed() {
        reset_stats_memo();
        let db = seeded();
        record(
            &db,
            "mine",
            "bun test src/tui",
            "bun:test:linear.eng-1234",
            0,
            T0,
        );
        let c = Collector::default();
        run_tags(&argv(&["--repo", "mine"]), &c.deps(Some(&db)));
        assert!(!c.text().contains("linear.eng-1234"), "{}", c.text());

        let shown = Collector::default();
        run_tags(
            &argv(&["show", "linear.eng-1234", "--repo", "mine"]),
            &shown.deps(Some(&db)),
        );
        assert!(
            shown.text().contains("bun test src/tui"),
            "{}",
            shown.text()
        );
    }

    // tags.test.ts: "sql answers a SELECT and refuses everything else"
    #[test]
    fn sql_answers_a_select_and_refuses_everything_else() {
        let dir = std::env::temp_dir().join(format!("bough-tags-sql-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bough.db").to_string_lossy().into_owned();
        {
            let db = open_db(Some(&file), DbOptions::default()).unwrap();
            db.create_session(session_row("s1", "wire the panel"))
                .unwrap();
        }

        let ok = Collector::default();
        let mut deps = ok.deps(None);
        deps.db_file = Some(file.clone());
        assert_eq!(
            run_tags(&argv(&["sql", "SELECT title FROM sessions"]), &deps),
            0
        );
        assert_eq!(
            serde_json::from_str::<Value>(&ok.text()).unwrap(),
            json!([{ "title": "wire the panel" }])
        );

        for bad in [
            "DELETE FROM sessions",
            "UPDATE sessions SET title = 'x'",
            "DROP TABLE sessions",
            "PRAGMA writable_schema = ON",
        ] {
            let c = Collector::default();
            let mut deps = c.deps(None);
            deps.db_file = Some(file.clone());
            assert_eq!(run_tags(&argv(&["sql", bad]), &deps), 2, "{bad}");
            assert!(
                c.errs().contains("must start with SELECT or WITH"),
                "{}",
                c.errs()
            );
            assert!(c.errs().contains(SURFACE), "{}", c.errs());
        }

        // A malformed query answers with the driver's own words.
        let broken = Collector::default();
        let mut deps = broken.deps(None);
        deps.db_file = Some(file.clone());
        assert_eq!(
            run_tags(&argv(&["sql", "SELECT nope FROM sessions"]), &deps),
            2
        );
        assert!(broken.errs().contains("nope"), "{}", broken.errs());

        // Leading whitespace and a block comment are trivia, not a verb: the
        // prefix check looks past them. (A `--`-comment cannot be tested here —
        // an argv word starting with `-` is an option, in TS too.)
        let commented = Collector::default();
        let mut deps = commented.deps(None);
        deps.db_file = Some(file.clone());
        assert_eq!(
            run_tags(
                &argv(&["sql", "  /* why */ SELECT title FROM sessions"]),
                &deps
            ),
            0,
            "{}",
            commented.errs()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The write really cannot land: the gate is structural, not a keyword trick.
    #[test]
    fn a_write_smuggled_past_the_keyword_check_still_cannot_land() {
        let dir = std::env::temp_dir().join(format!("bough-tags-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bough.db").to_string_lossy().into_owned();
        {
            let db = open_db(Some(&file), DbOptions::default()).unwrap();
            db.create_session(session_row("s1", "keep me")).unwrap();
        }
        // `WITH` passes the prefix check; the connection still refuses the write.
        let c = Collector::default();
        let mut deps = c.deps(None);
        deps.db_file = Some(file.clone());
        let code = run_tags(
            &argv(&["sql", "WITH x AS (SELECT 1) DELETE FROM sessions"]),
            &deps,
        );
        assert_eq!(code, 2, "{}", c.text());
        let db = open_db(Some(&file), DbOptions::default()).unwrap();
        assert!(
            db.get_session("s1").unwrap().is_some(),
            "the row was deleted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A greedy SELECT cannot flood a terminal — or a tool result.
    #[test]
    fn sql_output_is_capped_at_two_hundred_rows() {
        let dir = std::env::temp_dir().join(format!("bough-tags-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bough.db").to_string_lossy().into_owned();
        drop(open_db(Some(&file), DbOptions::default()).unwrap());
        let c = Collector::default();
        let mut deps = c.deps(None);
        deps.db_file = Some(file.clone());
        let code = run_tags(
            &argv(&[
                "sql",
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 1000) SELECT i FROM n",
            ]),
            &deps,
        );
        assert_eq!(code, 0, "{}", c.errs());
        let rows: Vec<Value> = serde_json::from_str(&c.text()).unwrap();
        assert_eq!(rows.len(), MAX_ROWS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct FakeLayer {
        rows: Vec<Value>,
        closed: Rc<RefCell<bool>>,
    }

    impl SimilarLayer for FakeLayer {
        fn similar(&self, _text: &str) -> Result<Vec<Value>, String> {
            Ok(self.rows.clone())
        }
        fn close(&self) {
            *self.closed.borrow_mut() = true;
        }
    }

    // tags.test.ts: "similar says why it cannot answer, and names what works"
    #[test]
    #[allow(clippy::arc_with_non_send_sync)]
    fn similar_says_why_it_cannot_answer_and_names_what_always_works() {
        let db = seeded();
        let c = Collector::default();
        let mut deps = c.deps(Some(&db));
        deps.embed = Some(Arc::new(|| None));
        assert_eq!(
            run_tags(&argv(&["similar", "get into the container"]), &deps),
            1
        );
        assert!(
            c.errs().contains("no local embedding layer"),
            "{}",
            c.errs()
        );
        assert!(c.errs().contains("bough tags sql"), "{}", c.errs());
        assert!(c.errs().contains("MATCH 'docker'"), "{}", c.errs());

        // …and answers through the layer when there is one, closing it after.
        let live = Collector::default();
        let closed = Rc::new(RefCell::new(false));
        let flag = closed.clone();
        let mut deps = live.deps(Some(&db));
        deps.embed = Some(Arc::new(move || {
            Some(Box::new(FakeLayer {
                rows: vec![json!({ "cmd": "docker exec -it web sh", "distance": 0.2 })],
                closed: flag.clone(),
            }) as Box<dyn SimilarLayer>)
        }));
        assert_eq!(run_tags(&argv(&["similar", "docker"]), &deps), 0);
        assert!(
            live.text().contains("docker exec -it web sh"),
            "{}",
            live.text()
        );
        assert!(*closed.borrow(), "the layer must be closed");
    }

    // tags.test.ts: "an empty memory says so rather than printing an empty table"
    #[test]
    fn an_empty_memory_says_so_rather_than_printing_an_empty_table() {
        reset_stats_memo();
        let db = open_db(Some(":memory:"), DbOptions::default()).unwrap();
        let c = Collector::default();
        let mut deps = c.deps(Some(&db));
        deps.cwd = Some("/nowhere".into());
        assert_eq!(run_tags(&[], &deps), 0);
        assert!(c.text().contains("no tagged commands yet"), "{}", c.text());
    }
}
