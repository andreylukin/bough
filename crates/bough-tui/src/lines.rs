//! The transcript as a flat list of pre-wrapped visual lines (port of
//! `src/tui/lines.ts`).
//!
//! INVARIANTS (from the TS header):
//! - **the transcript is data before it is a component**: `build_lines` yields
//!   one entry per PHYSICAL row, pre-wrapped, pre-styled, each carrying its
//!   click target and copy text;
//! - **folding is decided by predicates the caller owns** — and expand-all
//!   must NOT lift the per-block line caps;
//! - **a running program is visible while it runs**: live `tool.log` lines
//!   render under a call with no result and are REPLACED, not duplicated, by
//!   the finalized output when the `tool_result` lands (the `else` arm).
//!
//! The part-folding helpers (`segment_parts`, `tool_summary`, `output_text`,
//! `code_gist`) mirror `src/tui/format.ts` and live here until format.rs
//! absorbs them. `program_summary` names what a program DID; `code_gist` is
//! its fallback for a program nothing in the heuristic recognizes, which is
//! exactly what the TS does.

use std::collections::HashMap;

use bough_core::schema::parts::{
    is_delegated_kind, AskStatus, BackgroundJob, Message, Part, Role, SessionKind, TurnStatus,
    WorkflowStatus,
};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

use crate::ansi::{wrap_line, MIN_WRAP};
use crate::format::{accent, bold, danger, dim, highlight_code, info, md, osc8, surface, warn};
use crate::format::{fmt_tokens, fmt_usd};
use crate::store::selectors::{clip, one_line, plural};
use crate::store::state::{MarkKind, TranscriptMark};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VLine {
    pub text: String,
    /// Click target: a tool-group key toggles its fold; `<key>!full` lifts a
    /// block's line cap; `open:<sessionId>` descends into a subagent's branch.
    pub click: Option<String>,
    /// The raw, unstyled, unwrapped text of this line's section.
    pub copy: Option<String>,
    /// The single unwrapped LINE this row was laid out from (copy-across-wrap
    /// rejoins; deduped by the reader).
    pub src: Option<String>,
}

fn vl(text: impl Into<String>) -> VLine {
    VLine {
        text: text.into(),
        ..Default::default()
    }
}

fn wrap(text: &str, w: usize) -> Vec<String> {
    wrap_line(text, w.max(MIN_WRAP))
}

fn push(out: &mut Vec<VLine>, text: &str, w: usize, click: Option<&str>) {
    // EVERY wrapped row carries its own raw source, so a copy that spans a
    // wrap pastes the line, not where the window broke it.
    for l in wrap(text, w) {
        out.push(VLine {
            text: l,
            click: click.map(str::to_string),
            copy: None,
            src: Some(text.to_string()),
        });
    }
}

/// One accent: green is bough's; the user speaks plain; a harness-injected
/// note is amber — a `system` message is neither of them talking.
fn role_label(role: Role) -> String {
    match role {
        Role::User => bold("you"),
        Role::Supervisor => bold(&accent("bough")),
        Role::System => bold(&warn("system")),
    }
}

// ---- system notes the UI re-renders as cards --------------------------------

/// The tools bough actually grants. Anything else in a `tool_call` is a name
/// the model invented, labelled as prose rather than printed as an identifier.
const GRANTED_TOOLS: [&str; 2] = ["run_steps", "stop"];

fn call_code(input: &Value) -> &str {
    input.get("code").and_then(Value::as_str).unwrap_or("")
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubagentNote {
    pub title: String,
    pub session_id: String,
    pub status: String,
    pub ok: bool,
    pub files: Vec<String>,
    /// The note could not recover a file list ("not reported", not "none").
    pub files_unknown: bool,
    pub report: Option<String>,
}

static SUB_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\[subagent finished\] "(.*)" \(([^)]+)\) — (.+)\.$"#).unwrap()
});
static SUB_FILES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Changed files: (.+)\.$").unwrap());
static SUB_REPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^Report:\n((?s).*?)\nIt worked in THIS session's checkout").unwrap()
});

pub fn parse_subagent_note(text: &str) -> Option<SubagentNote> {
    let head = SUB_HEAD.captures(text)?;
    let (title, session_id, status) = (&head[1], &head[2], &head[3]);
    let files_line = SUB_FILES.captures(text).map(|c| c[1].to_string());
    // "not reported" is a fact about the harness's knowledge, not a file.
    let files_unknown = files_line.as_deref().is_none_or(|f| f == "not reported");
    let files: Vec<String> = match &files_line {
        Some(f) if f != "none" && !files_unknown => {
            f.split(", ").map(|s| s.trim().to_string()).collect()
        }
        _ => Vec::new(),
    };
    let report = SUB_REPORT.captures(text).map(|c| c[1].trim().to_string());
    // Only "finished" is success. FAILED / STOPPED / ORPHANED each mean
    // something different and the card must not flatten them into one mark.
    Some(SubagentNote {
        title: title.to_string(),
        session_id: session_id.to_string(),
        status: status.to_string(),
        ok: status.starts_with("finished"),
        files,
        files_unknown,
        report,
    })
}

/// The `[background]` wake note. The quoted name is optional — a note in an
/// old transcript predates names entirely.
static BG_NOTE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\[background\] (\S+)(?: "[^"]*")? finished"#).unwrap());

pub fn parse_bg_note(text: &str) -> Option<String> {
    BG_NOTE_RE.captures(text.trim()).map(|c| c[1].to_string())
}

/// The `[image]` note, kept for OLD transcripts — the record outlives the
/// feature that wrote it.
static IMAGE_NOTE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^\[image\] (\S+)(?: — (.*))?$").unwrap());

pub fn parse_image_note(text: &str) -> Option<(String, Option<String>)> {
    let c = IMAGE_NOTE_RE.captures(text.trim())?;
    let note = c
        .get(2)
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty());
    Some((c[1].to_string(), note))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowNoteStatus {
    Done,
    Error,
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowNote {
    pub status: WorkflowNoteStatus,
    pub name: String,
    pub succeeded: Option<String>,
}

static WF_NOTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\[workflow (done|error|stopped)\] "(.*)" \([^)]+\) — ((?s).*)$"#).unwrap()
});
static WF_SUCCEEDED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+/\d+ agents succeeded\.)").unwrap());

pub fn parse_workflow_note(text: &str) -> Option<WorkflowNote> {
    let c = WF_NOTE_RE.captures(text)?;
    let status = match &c[1] {
        "done" => WorkflowNoteStatus::Done,
        "error" => WorkflowNoteStatus::Error,
        _ => WorkflowNoteStatus::Stopped,
    };
    Some(WorkflowNote {
        status,
        name: c[2].to_string(),
        succeeded: WF_SUCCEEDED_RE.captures(&c[3]).map(|m| m[1].to_string()),
    })
}

// ---- blocks -----------------------------------------------------------------

/// Logical lines shown before "+N more" — the program, then its output.
pub const CODE_LINES: usize = 14;
pub const OUTPUT_LINES: usize = 20;
/// Report lines a finished-subagent card shows before "+N more".
pub const REPORT_LINES: usize = 6;

struct BlockOpts<'a> {
    max_lines: usize,
    style: &'a dyn Fn(&str) -> String,
    click: &'a str,
    full_key: Option<&'a str>,
    raised: bool,
}

/// A gutter-framed block: each logical line wraps to the remaining width and
/// every physical line carries a dim `│`. A truncated block ends on a
/// "+N more lines" row whose target is `full_key`, so lifting one block's cap
/// is separate from the fold itself.
fn push_block(out: &mut Vec<VLine>, text: &str, w: usize, opts: BlockOpts) {
    let finish = |l: String| if opts.raised { surface(&l, w) } else { l };
    let logical: Vec<&str> = text.split('\n').collect();
    let shown = &logical[..logical.len().min(opts.max_lines)];
    for line in shown {
        for l in wrap(line, w.saturating_sub(2)) {
            let styled = if l.is_empty() {
                String::new()
            } else {
                (opts.style)(&l)
            };
            out.push(VLine {
                text: finish(format!("{} {}", dim("│"), styled)),
                click: Some(opts.click.to_string()),
                copy: None,
                // The block's own line: a copy across a wrap rejoins it and
                // leaves the gutter behind.
                src: Some((*line).to_string()),
            });
        }
    }
    if logical.len() > shown.len() {
        out.push(VLine {
            text: finish(format!(
                "{} {}",
                dim("│"),
                dim(&format!("… +{} more lines", logical.len() - shown.len()))
            )),
            click: Some(opts.full_key.unwrap_or(opts.click).to_string()),
            copy: None,
            src: None,
        });
    }
}

/// Linkify bare URLs in one output line — OSC 8, trailing punctuation left out
/// of the target. Local minimal port of format.ts `linkifyUrls`.
fn linkify_urls(line: &str) -> String {
    static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://\S+").unwrap());
    let mut out = String::new();
    let mut last = 0;
    for m in URL_RE.find_iter(line) {
        out.push_str(&line[last..m.start()]);
        let raw = m.as_str();
        let trimmed = raw.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        out.push_str(&osc8(trimmed, trimmed));
        out.push_str(&raw[trimmed.len()..]);
        last = m.end();
    }
    out.push_str(&line[last..]);
    out
}

/// Program output reads dim — except the line that says the program died.
fn style_output_line(line: &str, is_error: bool) -> String {
    let l = linkify_urls(line);
    if is_error || line.starts_with("[program error]") {
        danger(&l)
    } else {
        dim(&l)
    }
}

/// Split the harness's own trailing notes off a tool result's output — the
/// `[history]` tag hints and the `[rules]` report, both rewritten for display
/// with the model-facing ` — …` tail cut. Order between the two kinds does
/// not matter — the loop walks backwards taking whichever prefix is last.
pub fn split_margin_notes(text: &str) -> (String, Vec<String>) {
    static HIST_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^tags previously used in (.+?): (.+?)( — .*)?$").unwrap());
    let mut lines: Vec<&str> = text.split('\n').collect();
    let mut hints: Vec<String> = Vec::new();
    while let Some(last) = lines.last() {
        if let Some(rest) = last.strip_prefix("[rules] ") {
            // One shape for every rule note: `rules: ` + what changed.
            hints.insert(
                0,
                format!("rules: {}", rest.split(" — ").next().unwrap_or("")),
            );
            lines.pop();
            continue;
        }
        if let Some(raw) = last.strip_prefix("[history] ") {
            let hint = match HIST_RE.captures(raw) {
                Some(c) => format!(
                    "{} also remembers: {}",
                    &c[1],
                    c[2].split(", ").collect::<Vec<_>>().join(" · ")
                ),
                None => raw.to_string(),
            };
            hints.insert(0, hint);
            lines.pop();
            continue;
        }
        break;
    }
    (lines.join("\n").trim_end().to_string(), hints)
}

// ---- part folding (format.ts mirrors — see module header) -------------------

/// One renderable segment of a message. Consecutive tool parts fold into ONE
/// `Tools` group; prose splits groups; ask/workflow/image stand alone.
pub enum Segment<'a> {
    Text(&'a str),
    Reasoning(&'a str),
    Image(&'a Part),
    Ask(&'a Part),
    Workflow(&'a Part),
    Tools(Vec<&'a Part>),
}

pub fn segment_parts(parts: &[Part]) -> Vec<Segment<'_>> {
    let mut segs: Vec<Segment> = Vec::new();
    for p in parts {
        match p {
            Part::Text { text } => segs.push(Segment::Text(text)),
            Part::Reasoning { text, .. } => segs.push(Segment::Reasoning(text)),
            Part::Image { .. } => segs.push(Segment::Image(p)),
            Part::Ask { .. } => segs.push(Segment::Ask(p)),
            Part::Workflow { .. } => segs.push(Segment::Workflow(p)),
            Part::ToolCall { .. } | Part::ToolResult { .. } => match segs.last_mut() {
                Some(Segment::Tools(list)) => list.push(p),
                _ => segs.push(Segment::Tools(vec![p])),
            },
        }
    }
    segs
}

/// A tool result's output as text (JSON-stringified when structured).
pub fn output_text(result: &Part) -> String {
    match result {
        Part::ToolResult { output, .. } => match output {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        _ => String::new(),
    }
}

pub struct ToolSummary<'a> {
    pub calls: Vec<&'a Part>,
    pub results: HashMap<&'a str, &'a Part>,
    pub running: bool,
    pub has_error: bool,
    pub errors: usize,
    pub interrupted: bool,
}

/// The facts a collapsed tool header shows without expanding. No "check
/// passed" verdict — bough has no acceptance gate.
pub fn tool_summary<'a>(parts: &[&'a Part]) -> ToolSummary<'a> {
    let calls: Vec<&Part> = parts
        .iter()
        .copied()
        .filter(|p| matches!(p, Part::ToolCall { .. }))
        .collect();
    let mut results: HashMap<&str, &Part> = HashMap::new();
    for p in parts {
        if let Part::ToolResult { call_id, .. } = p {
            results.insert(call_id.as_str(), p);
        }
    }
    let running = calls.iter().any(|c| match c {
        Part::ToolCall { id, .. } => !results.contains_key(id.as_str()),
        _ => false,
    });
    let errors = results
        .values()
        .filter(|r| matches!(r, Part::ToolResult { is_error: true, .. }))
        .count();
    let interrupted = results.values().any(|r| {
        matches!(
            r,
            Part::ToolResult {
                interrupted: Some(true),
                ..
            }
        )
    });
    ToolSummary {
        calls,
        results,
        running,
        has_error: errors > 0,
        errors,
        interrupted,
    }
}

/// One-line excerpt of a tool call's input: the first meaningful code line, or
/// compact JSON.
pub fn code_gist(input: &Value, max: usize) -> String {
    let src = match input.get("code").and_then(Value::as_str) {
        Some(code) => code.to_string(),
        None => {
            if input.is_null() {
                serde_json::to_string(input).unwrap_or_default()
            } else {
                input.to_string()
            }
        }
    };
    let line = src
        .trim()
        .split('\n')
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("//"))
        .unwrap_or("");
    clip(line, max)
}

/// A host-fn name that must NOT match its member-call twin.
///
/// The TS wrote `(?<![.\w])name`; Rust's `regex` has no lookbehind, so the
/// boundary is CONSUMED instead: `(?:^|[^.\w])name`. Same rule, and `\b` is
/// still wrong for the same reason — `\b` matches between a dot and a letter,
/// so every host name would also match its twin on an object. Seen on screen:
/// a program whose only host call was `artifact()` was headlined `collected
/// subagent reports · published an artifact`, because it also contained
/// `functions.join("\n")`. The same trap waited in `.write(` on a stream,
/// `.search(` on a string, and `.view(`/`.ask(` on anything.
fn host_re(pattern: &str) -> Regex {
    Regex::new(&format!(r"(?:^|[^.\w]){pattern}")).unwrap()
}

/// How many times `re` matches. Non-overlapping, which is what a tally wants.
fn count_re(re: &Regex, code: &str) -> usize {
    re.find_iter(code).count()
}

/// The distinct capture-1 values of `re`, in order of appearance.
fn captured_paths(re: &Regex, code: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in re.captures_iter(code) {
        if let Some(m) = c.get(1) {
            let p = m.as_str().to_string();
            if !p.is_empty() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// The basename. What a reviewer would call the file.
fn base_name(path: &str) -> String {
    path.split('/')
        .rfind(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// A path with `${…}` in it is a TEMPLATE, not a name.
///
/// Seen on a fresh walk: `▸ 1 step · wrote ${cartPath}` — the placeholder
/// printed at the reader. An interpolated path is unnamed, so it falls through
/// to the counting branch and reads `wrote 1 file`.
fn is_named(path: &str) -> bool {
    !path.contains("${")
}

/// Two names in full, then `first +N more` — the row is shared with the step
/// count and the status chips.
fn name_list(paths: &[String]) -> String {
    if paths.len() <= 2 {
        paths
            .iter()
            .map(|p| base_name(p))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        format!("{} +{} more", base_name(&paths[0]), paths.len() - 1)
    }
}

/// What a program DID, named the way a reviewer would name it.
///
/// The collapsed step header used to be the program's first line of code,
/// clipped:
///
///   `▸ 1 step  run_steps · const out = await bash(`node --input-type=module -e "`
///
/// — which reads as debug output rather than as a UI, and answers none of the
/// questions a reader has (which files did it touch? did it run something?).
/// Every comparable harness names the operation and its target: `Update(app.mjs)`,
/// `Read 1 file`, `Ran 1 shell command`.
///
/// bough writes ONE program per round rather than one call, so the equivalent
/// is a tally of the host functions it called: `read app.mjs · ran 1 command`.
/// Derived by scanning the source for host-function call sites, which is a
/// heuristic and is allowed to be — **it is a LABEL.** When nothing is
/// recognized this returns `""` and the caller falls back to the code gist, so
/// an unusual program degrades to what was shown before rather than to nothing.
///
/// `running` puts the verbs in the present tense. A call with no result yet is
/// a call still in flight, and "ran 1 command" under a shell that has been
/// blocked for ten seconds is a statement the reader acts on and should not.
pub fn program_summary(code: &str, max: usize, running: bool) -> String {
    if code.is_empty() {
        return String::new();
    }
    let mut bits: Vec<String> = Vec::new();

    // `patch` is NOT in this alternation, and that is the whole point: it takes
    // ONE string — the patch body — not a path, so matching it here captured
    // the entire template literal and the header read
    // `wrote cart.js#8902] SWAP 3.=3: + for (…`. Its files are the `[path#hash]`
    // section tags inside that body, and one call may carry several.
    static WROTE: LazyLock<Regex> = LazyLock::new(|| {
        // The boundary guards the HOST names only: `fs.writeFileSync` is a
        // write, `stream.write` is not.
        Regex::new(
            r#"(?:(?:^|[^.\w])(?:write|edit)|writeFileSync|writeFile)\s*\(\s*["'`]([^"'`]+)"#,
        )
        .unwrap()
    });
    static PATCH_CALL: LazyLock<Regex> = LazyLock::new(|| host_re(r"patch\s*\("));
    static PATCH_TAG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^\]\s#]+)#[^\]\s]*\]").unwrap());
    // Three spellings, because a program reads a file three ways in practice:
    // the host verb, the `Bun.file` the prompt recommends ("there is no
    // read()"), and `node:fs` — models reach for whatever the prompt says.
    // `readFileSync`/`readFile` are matched WITH a possible dot
    // (`fs.readFileSync`) unlike the host names, because those two words mean
    // the same thing on any object.
    static READ: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:(?:^|[^.\w])(?:view|read)|Bun\.file|readFileSync|readFile)\s*\(\s*["'`]([^"'`]+)"#).unwrap()
    });

    let mut wrote = captured_paths(&WROTE, code);
    if PATCH_CALL.is_match(code) {
        for p in captured_paths(&PATCH_TAG, code) {
            if !wrote.contains(&p) {
                wrote.push(p);
            }
        }
    }
    wrote.retain(|p| is_named(p));
    let mut read = captured_paths(&READ, code);
    read.retain(|p| is_named(p));

    if !wrote.is_empty() {
        bits.push(format!(
            "{} {}",
            if running { "writing" } else { "wrote" },
            name_list(&wrote)
        ));
    }
    if !read.is_empty() {
        bits.push(format!(
            "{} {}",
            if running { "reading" } else { "read" },
            name_list(&read)
        ));
    }
    // A call whose path is a VARIABLE yields no name —
    // `fs.readFileSync(filePath, "utf8")` — and naming nothing is not a reason
    // to fall back to a line of source. Count it instead. `patch` belongs in
    // the COUNT even though it is deliberately absent from the naming
    // alternation: a program that builds its tag — ``patch(`[${path}#3AF0]…`)``,
    // exactly what a skill told to view-then-patch writes — named nothing AND
    // counted nothing.
    if wrote.is_empty() {
        static N: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?:(?:^|[^.\w])(?:write|edit|patch)|writeFileSync|writeFile)\s*\(")
                .unwrap()
        });
        let n = count_re(&N, code);
        if n > 0 {
            bits.push(format!(
                "{} {}",
                if running { "writing" } else { "wrote" },
                plural(n as i64, "file")
            ));
        }
    }
    if read.is_empty() {
        static N: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?:(?:^|[^.\w])view|Bun\.file|readFileSync|readFile)\s*\(").unwrap()
        });
        let n = count_re(&N, code);
        if n > 0 {
            bits.push(format!(
                "{} {}",
                if running { "reading" } else { "read" },
                plural(n as i64, "file")
            ));
        }
    }

    // `execSync`/`spawnSync`/`Bun.$` are running a command too: a program that
    // reached for `node:child_process` instead of `bash()` was unrecognized, so
    // the header fell back to
    // `const { execSync } = require("node:child_process");`.
    //
    // The SYNC names only, plus `Bun.$`. Bare `spawn(` and `exec(` are excluded
    // and that is not fussiness: `spawn` IS bough's detached-delegation verb,
    // so counting it here made a fan-out report `ran 1 command · 1 subagent`.
    static BASH: LazyLock<Regex> = LazyLock::new(|| host_re(r"bash\s*\("));
    static SH: LazyLock<Regex> = LazyLock::new(|| host_re(r"sh\s*\("));
    static EXEC: LazyLock<Regex> =
        LazyLock::new(|| host_re(r"(?:execSync|execFileSync|spawnSync)\s*\("));
    static BUN_SH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Bun\.\$").unwrap());
    let shells = count_re(&BASH, code)
        + count_re(&SH, code)
        + count_re(&EXEC, code)
        + count_re(&BUN_SH, code);
    if shells > 0 {
        bits.push(format!(
            "{} {} command{}",
            if running { "running" } else { "ran" },
            shells,
            if shells == 1 { "" } else { "s" }
        ));
    }
    // `bashBg(` does not match `bash\s*\(`, so a round that only backgrounded a
    // command was unrecognized — the one round whose whole point is that
    // something is still running after it returns.
    static BG: LazyLock<Regex> = LazyLock::new(|| host_re(r"bashBg\s*\("));
    static KILL: LazyLock<Regex> = LazyLock::new(|| host_re(r"bashKill\s*\("));
    static WAIT: LazyLock<Regex> = LazyLock::new(|| host_re(r"bashWait\s*\("));
    static OUTPUT: LazyLock<Regex> = LazyLock::new(|| host_re(r"bashOutput\s*\("));
    let bg = count_re(&BG, code);
    if bg > 0 {
        bits.push(format!(
            "started {bg} background command{}",
            if bg == 1 { "" } else { "s" }
        ));
    // The three shell-management verbs, which complete the set. Observed
    // unnamed: `▸ 1 step · const output = await bashOutput("bg_1");` — a round
    // whose whole act was reading a job's output, headlined with the line that
    // read it.
    } else if count_re(&KILL, code) > 0 {
        bits.push("killed a background command".into());
    } else if count_re(&WAIT, code) > 0 {
        bits.push(
            if running {
                "waiting for a background command"
            } else {
                "waited for a background command"
            }
            .into(),
        );
    } else if count_re(&OUTPUT, code) > 0 {
        bits.push("read a background command's output".into());
    }

    // Delegation is FOUR verbs, not one. Counting only `agent(` meant the round
    // that fanned three subagents out with `spawn()` matched nothing and fell
    // back to the gist — the header read `const tasks = [`, raw source, on the
    // single round a reader most needs named. `join`/`adopt` collect reports
    // for spawns issued in an earlier round, so they are named only when no
    // spawn happens here.
    static AGENTS: LazyLock<Regex> = LazyLock::new(|| host_re(r"(?:agent|spawn)\s*\("));
    static JOINS: LazyLock<Regex> = LazyLock::new(|| host_re(r"(?:join|adopt)\s*\("));
    // `node:path` exports a BARE `join`, so a program that did
    // `const { join } = await import("node:path")` was headlined "collected
    // subagent reports". Requiring `await` does not separate them — the real
    // delegation pattern is `Promise.all(ids.map((id) => join(id)))`, with no
    // await in sight. What DOES separate them is the destructure: a program
    // that pulled `join` out of `path` has shadowed the host verb and cannot be
    // calling it. BOTH spellings, because the one I did not write is the one
    // the model used.
    static SHADOW_DYN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\{[^}]*\bjoin\b[^}]*\}\s*=\s*(?:await\s+)?(?:import|require)\s*\(\s*["'`](?:node:)?path"#).unwrap()
    });
    static SHADOW_STATIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"import\s*\{[^}]*\bjoin\b[^}]*\}\s*from\s*["'`](?:node:)?path"#).unwrap()
    });
    let shadows_join = SHADOW_DYN.is_match(code) || SHADOW_STATIC.is_match(code);
    let agents = count_re(&AGENTS, code);
    if agents > 0 {
        bits.push(format!(
            "{agents} subagent{}",
            if agents == 1 { "" } else { "s" }
        ));
    } else if !shadows_join && count_re(&JOINS, code) > 0 {
        bits.push(
            if running {
                "collecting subagent reports"
            } else {
                "collected subagent reports"
            }
            .into(),
        );
    }

    // `workflow(…)` starts one; `workflow.status(…)` asks after one already
    // running. Naming only the first left every poll round falling back to the
    // gist, so waiting for a fan-out read as
    // `await new Promise(r => setTimeout(r, 2000));`.
    static WORKFLOW: LazyLock<Regex> = LazyLock::new(|| host_re(r"workflow\s*\("));
    static WORKFLOW_MEMBER: LazyLock<Regex> = LazyLock::new(|| host_re(r"workflow\.\w+\s*\("));
    static TIMEOUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"setTimeout\s*\(").unwrap());
    if count_re(&WORKFLOW, code) > 0 {
        bits.push(
            if running {
                "running a workflow"
            } else {
                "ran a workflow"
            }
            .into(),
        );
    } else if count_re(&WORKFLOW_MEMBER, code) > 0 {
        bits.push("checked the workflow run".into());
    } else if TIMEOUT.is_match(code) {
        bits.push(if running { "waiting" } else { "waited" }.into());
    }

    static ASK: LazyLock<Regex> = LazyLock::new(|| host_re(r"ask\s*\("));
    static ARTIFACT: LazyLock<Regex> = LazyLock::new(|| host_re(r"artifact\s*\("));
    if count_re(&ASK, code) > 0 {
        bits.push("asked you a question".into());
    }
    if count_re(&ARTIFACT, code) > 0 {
        bits.push(
            if running {
                "publishing an artifact"
            } else {
                "published an artifact"
            }
            .into(),
        );
    }

    // THE REST OF THE HOST SURFACE. Everything not named here falls back to a
    // line of source, which is the state this function exists to end — a
    // program whose only call was `state.set({key, value})` was headlined
    // `await state.set({key: "campaign", value: "overnight-ux"});`.
    static STATE_W: LazyLock<Regex> = LazyLock::new(|| host_re(r"state\.(?:set|delete)\s*\("));
    static STATE_R: LazyLock<Regex> = LazyLock::new(|| host_re(r"state\.(?:get|list)\s*\("));
    static SCHEDULE: LazyLock<Regex> = LazyLock::new(|| host_re(r"schedule\.\w+\s*\("));
    static FETCH: LazyLock<Regex> = LazyLock::new(|| host_re(r"fetch\s*\("));
    static IMAGE: LazyLock<Regex> = LazyLock::new(|| host_re(r"image\s*\("));
    if count_re(&STATE_W, code) > 0 {
        bits.push("wrote session state".into());
    } else if count_re(&STATE_R, code) > 0 {
        bits.push("read session state".into());
    }
    if count_re(&SCHEDULE, code) > 0 {
        bits.push("changed a schedule".into());
    }
    let fetched = count_re(&FETCH, code);
    if fetched > 0 {
        bits.push(format!(
            "{} {}",
            if running { "fetching" } else { "fetched" },
            plural(fetched as i64, "URL")
        ));
    }
    if count_re(&IMAGE, code) > 0 {
        bits.push("attached an image".into());
    }

    // Matched in the COMMAND TEXT, not as a call shape: MCP is reached by
    // running `bough mcp call`, the same way `ast-grep` is. There is no host
    // function left to count.
    static MCP_CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bbough mcp call\b").unwrap());
    static MCP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bbough mcp\b").unwrap());
    static AST_GREP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bast-grep\b").unwrap());
    let mcp_calls = count_re(&MCP_CALL, code);
    if mcp_calls > 0 {
        bits.push(plural(mcp_calls as i64, "MCP call"));
    } else if count_re(&MCP, code) > 0 {
        bits.push("checked the MCP servers".into());
    }
    // `ast-grep` rides inside a shell string rather than arriving as its own
    // host function, so this matches the command text, not a call shape. It
    // stays in the list because a structural search is a different ACT from a
    // text sweep and the header is the only place that distinction is visible.
    if count_re(&AST_GREP, code) > 0 {
        bits.push("searched by structure".into());
    }

    static SEARCHES: LazyLock<Regex> = LazyLock::new(|| host_re(r"(?:grep|glob|search)\s*\("));
    if count_re(&SEARCHES, code) > 0 && bits.is_empty() {
        bits.push(
            if running {
                "searching the tree"
            } else {
                "searched the tree"
            }
            .into(),
        );
    }

    // Nothing recognized: the caller falls back to the code gist rather than to
    // an empty header.
    if bits.is_empty() {
        return String::new();
    }
    let joined = bits.join(" · ");
    if joined.chars().count() > max {
        let head: String = joined.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", head.trim_end())
    } else {
        joined
    }
}

// ---- the tool fold ----------------------------------------------------------

static EXIT_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[exit code \d+").unwrap());

fn call_fields(call: &Part) -> (&str, &str, &Value) {
    match call {
        Part::ToolCall { id, name, input } => (id, name, input),
        _ => ("", "", &Value::Null),
    }
}

fn result_flags(res: &Part) -> (bool, bool) {
    match res {
        Part::ToolResult {
            is_error,
            interrupted,
            ..
        } => (*is_error, interrupted.unwrap_or(false)),
        _ => (false, false),
    }
}

/// The program as the renderer derives it: `code` verbatim, anything else JSON.
fn call_input_text(input: &Value) -> String {
    match input.get("code").and_then(Value::as_str) {
        Some(code) => code.to_string(),
        None => serde_json::to_string_pretty(input).unwrap_or_default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn tool_group_lines(
    out: &mut Vec<VLine>,
    parts: &[&Part],
    key: &str,
    expanded: bool,
    full: bool,
    w: usize,
    tool_logs: Option<&HashMap<String, Vec<String>>>,
    // `declined`: the user declined an `ask()` in this MESSAGE — passed in
    // because this function only ever sees the tool parts.
    declined: bool,
) {
    let cap_code = if full { usize::MAX } else { CODE_LINES };
    let cap_out = if full { usize::MAX } else { OUTPUT_LINES };
    let s = tool_summary(parts);
    if s.calls.is_empty() {
        return;
    }
    // A command that failed inside an otherwise-fine round: non-error results
    // whose output carries `[exit code N`.
    let failed = s
        .results
        .values()
        .filter(|r| {
            let (is_error, _) = result_flags(r);
            !is_error && EXIT_CODE_RE.is_match(&output_text(r))
        })
        .count();
    // THE STATUS COMES FIRST, where nothing can clip it. Precedence: running →
    // declined → partial errors → all-error → interrupted → failed-commands.
    let state = if s.running {
        warn("⚙ ")
    } else if s.has_error && declined {
        warn("⏹ declined  ")
    } else if s.errors > 0 && s.errors < s.calls.len() {
        warn(&format!("⚠ {} of {} failed  ", s.errors, s.calls.len()))
    } else if s.has_error {
        danger("✗ error  ")
    } else if s.interrupted {
        warn("⏹ interrupted  ")
    } else if failed > 0 {
        warn(&format!("⚠ {} failed  ", plural(failed as i64, "command")))
    } else {
        String::new()
    };
    // GRANTED tool names only, and only when they DIFFER.
    let mut names: Vec<&str> = Vec::new();
    for c in &s.calls {
        let (_, name, _) = call_fields(c);
        if GRANTED_TOOLS.contains(&name) && !names.contains(&name) {
            names.push(name);
        }
    }
    let count = format!(
        "{} {}",
        s.calls.len(),
        if s.calls.len() == 1 { "step" } else { "steps" }
    );
    let mut head = format!("{} {}", if expanded { "▾" } else { "▸" }, state);
    head.push_str(&dim(&if names.len() > 1 {
        format!("{count}  {}", names.join(" · "))
    } else {
        count
    }));
    let mut steps: Vec<String> = Vec::new();
    // A collapsed group carries WHAT IT DID — every call's gist.
    if !expanded {
        let gists: Vec<String> = s
            .calls
            .iter()
            .filter_map(|call| {
                let (id, name, input) = call_fields(call);
                let live = !s.results.contains_key(id);
                if name != "run_steps" {
                    // The model reached for a host function AS a tool.
                    return Some(format!("called {name} as a tool"));
                }
                let g = {
                    let ps = program_summary(call_code(input), 64, live);
                    if ps.is_empty() {
                        code_gist(input, 60)
                    } else {
                        ps
                    }
                };
                if g.is_empty() {
                    None
                } else {
                    Some(g)
                }
            })
            .collect();
        // REPEATS COLLAPSE to `… ×N`.
        static XN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" ×(\d+)$").unwrap());
        let mut merged: Vec<String> = Vec::new();
        for g in gists {
            let bare = merged.last().map(|l| XN_RE.replace(l, "").to_string());
            if bare.as_deref() == Some(g.as_str()) {
                let last = merged.last().unwrap().clone();
                let n: u32 = XN_RE
                    .captures(&last)
                    .and_then(|c| c[1].parse().ok())
                    .unwrap_or(1)
                    + 1;
                *merged.last_mut().unwrap() = format!("{g} ×{n}");
            } else {
                merged.push(g);
            }
        }
        // ONE ROW PER STEP once there is more than one; a single step stays on
        // the header.
        if merged.len() == 1 {
            head.push_str(&dim(&format!(
                " · {}",
                clip(&merged[0], w.saturating_sub(16).max(20))
            )));
        } else if merged.len() > 1 {
            steps = merged
                .iter()
                .map(|g| clip(g, w.saturating_sub(6).max(20)))
                .collect();
        }
    }
    // Never wrapped: the whole visual row stays one click target.
    out.push(VLine {
        text: head,
        click: Some(key.to_string()),
        copy: None,
        src: None,
    });
    for step in steps {
        out.push(VLine {
            text: format!("  {}", dim(&step)),
            click: Some(key.to_string()),
            copy: None,
            src: None,
        });
    }
    if !expanded {
        return;
    }
    for call in &s.calls {
        let (id, name, input) = call_fields(call);
        let res = s.results.get(id).copied();
        let status = match res {
            None => warn("⚙ running"),
            Some(r) => {
                let (is_error, interrupted) = result_flags(r);
                if is_error {
                    danger("✗ error")
                } else if interrupted {
                    warn("⏹ interrupted")
                } else {
                    accent("✓ done")
                }
            }
        };
        // The ◇ marker takes the call's status color.
        let mark = match res {
            Some(r) => {
                let (is_error, interrupted) = result_flags(r);
                if is_error {
                    danger("◇")
                } else if interrupted {
                    warn("◇")
                } else {
                    accent("◇")
                }
            }
            None => accent("◇"),
        };
        // WHAT IT DID, not what it was called.
        let label = if name == "run_steps" {
            let ps = program_summary(call_code(input), 64, res.is_none());
            if ps.is_empty() {
                name.to_string()
            } else {
                ps
            }
        } else {
            format!("{name} (as a tool)")
        };
        push(out, &format!("{mark} {label} {status}"), w, Some(key));
        let input_text = call_input_text(input);
        if !input_text.is_empty() {
            let full_key = format!("{key}!full");
            push_block(
                out,
                &input_text,
                w,
                BlockOpts {
                    max_lines: cap_code,
                    style: &|l| highlight_code(l, "js"),
                    click: key,
                    full_key: Some(&full_key),
                    raised: true,
                },
            );
        }
        if let Some(r) = res.filter(|r| !output_text(r).is_empty()) {
            // Directory tag hints ride the result's LOGS; pulled out of the
            // │-block and rendered as `#` marginalia after it.
            let (is_error, _) = result_flags(r);
            let (body, hints) = split_margin_notes(&output_text(r));
            if !body.is_empty() {
                out.push(VLine {
                    text: dim("↳ output"),
                    click: Some(key.to_string()),
                    copy: None,
                    src: None,
                });
                let full_key = format!("{key}!full");
                push_block(
                    out,
                    &body,
                    w,
                    BlockOpts {
                        max_lines: cap_out,
                        style: &|l| style_output_line(l, is_error),
                        click: key,
                        full_key: Some(&full_key),
                        raised: true,
                    },
                );
            }
            for h in hints {
                out.push(VLine {
                    text: format!("  {}", dim(&format!("# {h}"))),
                    click: None,
                    copy: Some(format!("# {h}")),
                    src: None,
                });
            }
        } else if res.is_none() {
            // Still running: the program's console lines as they stream in.
            // The finalized `tool_result` replaces them with the same lines
            // joined — which is why this arm is an `else`, not an addition.
            if let Some(live) = tool_logs.and_then(|t| t.get(id)).filter(|l| !l.is_empty()) {
                out.push(VLine {
                    text: dim("↳ output (live)"),
                    click: Some(key.to_string()),
                    copy: None,
                    src: None,
                });
                let full_key = format!("{key}!full");
                push_block(
                    out,
                    &live.join("\n"),
                    w,
                    BlockOpts {
                        max_lines: cap_out,
                        style: &|l| style_output_line(l, false),
                        click: key,
                        full_key: Some(&full_key),
                        raised: true,
                    },
                );
            }
        }
    }
}

/// The whole group as plain text, for a right-click copy.
fn tool_group_copy(parts: &[&Part]) -> String {
    let s = tool_summary(parts);
    s.calls
        .iter()
        .map(|call| {
            let (id, name, input) = call_fields(call);
            let mut block = format!("◇ {name}\n{}", call_input_text(input));
            if let Some(r) = s.results.get(id) {
                let out = output_text(r);
                if !out.is_empty() {
                    block.push_str(&format!("\n↳ output\n{out}"));
                }
            }
            block
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ---- messages ---------------------------------------------------------------

fn workflow_note_lines(
    out: &mut Vec<VLine>,
    text: &str,
    key: &str,
    expanded: bool,
    full: bool,
    w: usize,
) {
    let note = parse_workflow_note(text).unwrap();
    let outcome = match note.status {
        WorkflowNoteStatus::Done => accent("✓ finished"),
        WorkflowNoteStatus::Error => danger("✗ failed"),
        WorkflowNoteStatus::Stopped => warn("■ stopped"),
    };
    let facts = [
        note.succeeded.clone(),
        Some(format!(
            "click to {}",
            if expanded { "collapse" } else { "expand" }
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    out.push(VLine {
        text: format!(
            "{} {} {} {}  {}",
            if expanded { "▾" } else { "▸" },
            dim("workflow"),
            bold(&note.name),
            outcome,
            dim(&facts)
        ),
        click: Some(key.to_string()),
        copy: None,
        src: None,
    });
    if !expanded {
        return;
    }
    let full_key = format!("{key}!full");
    push_block(
        out,
        text,
        w,
        BlockOpts {
            max_lines: if full { usize::MAX } else { OUTPUT_LINES },
            style: &|l| dim(l),
            click: key,
            full_key: Some(&full_key),
            raised: false,
        },
    );
}

fn text_parts_joined(msg: &Message, sep: &str) -> String {
    msg.parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(sep)
}

/// Strip a trailing `<stop>` the model wrote as PROSE instead of calling the
/// tool. End-of-message only; a fence must hold nothing else. Presentational
/// only — the stored text is untouched.
fn without_stop_sentinel(text: &str) -> String {
    static STOP_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\n*(?:```[a-z]*\n)?\s*<stop>\s*(?:\n```)?\s*$").unwrap());
    STOP_RE.replace(text, "").trim_end().to_string()
}

#[allow(clippy::too_many_arguments)]
// `copy` is bound late because each arm produces it alongside the lines it
// pushes, and one arm `continue`s without producing anything at all.
#[allow(clippy::needless_late_init)]
pub fn message_lines(
    msg: &Message,
    is_expanded: &dyn Fn(&str) -> bool,
    is_full: &dyn Fn(&str) -> bool,
    w: usize,
    streaming: Option<&str>,
    tool_logs: Option<&HashMap<String, Vec<String>>>,
    runs: Option<&HashMap<String, RunCardView>>,
    now: i64,
) -> Vec<VLine> {
    let mut out: Vec<VLine> = Vec::new();
    let mut body: Vec<VLine> = Vec::new();
    let inner = w.saturating_sub(2);
    // An image note collapses to ONE line with no role label.
    if msg.role == Role::System {
        let texts: Vec<&Part> = msg
            .parts
            .iter()
            .filter(|p| matches!(p, Part::Text { .. }))
            .collect();
        let imgs: Vec<&Part> = msg
            .parts
            .iter()
            .filter(|p| matches!(p, Part::Image { .. }))
            .collect();
        if texts.len() == 1 && imgs.len() == 1 {
            if let Part::Text { text } = texts[0] {
                if let Some((path, note)) = parse_image_note(text) {
                    if let Part::Image { name, size, .. } = imgs[0] {
                        let kb = ((*size as f64) / 1024.0).round().max(1.0) as i64;
                        let short = name.split('/').next_back().unwrap_or(name).trim();
                        out.push(vl(""));
                        out.push(VLine {
                            text: format!(
                                "  {}",
                                dim(&format!(
                                    "🖼 {short}{} · {kb} KB",
                                    note.map(|n| format!(" — {n}")).unwrap_or_default()
                                ))
                            ),
                            click: None,
                            copy: Some(path),
                            src: None,
                        });
                        return out;
                    }
                }
            }
        }
    }
    // A workflow completion note: compact, expandable transcript receipt.
    if msg.role == Role::System {
        let text = text_parts_joined(msg, "\n");
        if parse_workflow_note(&text).is_some() {
            let key = format!("{}:workflow", msg.id);
            out.push(vl(""));
            workflow_note_lines(
                &mut out,
                &text,
                &key,
                is_expanded(&key),
                is_full(&key),
                w.saturating_sub(2),
            );
            return out
                .into_iter()
                .map(|l| {
                    if l.text.is_empty() {
                        l
                    } else {
                        VLine {
                            text: format!("  {}", l.text),
                            copy: Some(text.clone()),
                            ..l
                        }
                    }
                })
                .collect();
        }
    }
    out.push(vl(""));
    out.push(vl(role_label(msg.role)));
    // Bodies hang two columns under the role label so turns read as blocks.
    let declined = msg.parts.iter().any(|p| {
        matches!(
            p,
            Part::Ask {
                status: AskStatus::Declined,
                ..
            }
        )
    });
    for (i, s) in segment_parts(&msg.parts).iter().enumerate() {
        let key = format!("{}:{i}", msg.id);
        let mut seg: Vec<VLine> = Vec::new();
        let copy: String;
        match s {
            Segment::Text(text) => {
                let shown = without_stop_sentinel(text);
                push(&mut seg, &md(&shown, Some(inner)), inner, None);
                copy = shown;
            }
            Segment::Reasoning(text) => {
                // Thinking folds like a tool step. Empty reasoning renders
                // nothing at all.
                if text.trim().is_empty() {
                    continue;
                }
                let logical: Vec<&str> = text.split('\n').collect();
                if is_expanded(&key) {
                    seg.push(VLine {
                        text: format!(
                            "▾ {}",
                            dim(&format!(
                                "thinking ({})",
                                plural(logical.len() as i64, "line")
                            ))
                        ),
                        click: Some(key.clone()),
                        copy: None,
                        src: None,
                    });
                    let full_key = format!("{key}!full");
                    push_block(
                        &mut seg,
                        text,
                        inner,
                        BlockOpts {
                            max_lines: if is_full(&key) {
                                usize::MAX
                            } else {
                                OUTPUT_LINES
                            },
                            style: &|l| dim(l),
                            click: &key,
                            full_key: Some(&full_key),
                            raised: false,
                        },
                    );
                } else {
                    let gist = logical
                        .iter()
                        .map(|l| l.trim())
                        .find(|l| !l.is_empty())
                        .unwrap_or("");
                    seg.push(VLine {
                        text: format!("▸ {}", dim(&format!("thinking · {}", clip(gist, 60)))),
                        click: Some(key.clone()),
                        copy: None,
                        src: None,
                    });
                }
                copy = (*text).to_string();
            }
            Segment::Image(p) => {
                let Part::Image {
                    path, name, size, ..
                } = p
                else {
                    continue;
                };
                let kb = ((*size as f64) / 1024.0).round().max(1.0) as i64;
                seg.push(vl(dim(&format!("🖼 {name} ({kb} KB)"))));
                copy = path.clone();
            }
            Segment::Ask(p) => {
                // A settled `ask()` — one always-visible line, never folded.
                let Part::Ask {
                    question,
                    status,
                    answer,
                    ..
                } = p
                else {
                    continue;
                };
                let outcome = match status {
                    AskStatus::Answered => bold(answer.as_deref().unwrap_or("")),
                    AskStatus::Declined => dim("declined"),
                    AskStatus::Interrupted => dim("interrupted"),
                };
                push(
                    &mut seg,
                    &format!("{} {question} {} {outcome}", warn("?"), dim("→")),
                    inner,
                    None,
                );
                let status_word = match status {
                    AskStatus::Answered => answer.clone().unwrap_or_default(),
                    AskStatus::Declined => "declined".to_string(),
                    AskStatus::Interrupted => "interrupted".to_string(),
                };
                copy = format!("{question} → {status_word}");
            }
            Segment::Workflow(p) => {
                let Part::Workflow {
                    id,
                    name,
                    description,
                    rerun_of,
                } = p
                else {
                    continue;
                };
                workflow_card_lines(
                    &mut seg,
                    id,
                    name,
                    description,
                    rerun_of.is_some(),
                    runs.and_then(|r| r.get(id)),
                    now,
                );
                copy = format!("{name} — {description} ({id})");
            }
            Segment::Tools(parts) => {
                tool_group_lines(
                    &mut seg,
                    parts,
                    &key,
                    is_expanded(&key),
                    is_full(&key),
                    inner,
                    tool_logs,
                    declined,
                );
                copy = tool_group_copy(parts);
            }
        }
        for l in seg {
            body.push(VLine {
                copy: Some(copy.clone()),
                ..l
            });
        }
    }
    if let Some(streaming) = streaming.filter(|s| !s.is_empty()) {
        let mut seg: Vec<VLine> = Vec::new();
        push(&mut seg, &format!("{}▌", md(streaming, None)), inner, None);
        for l in seg {
            body.push(VLine {
                copy: Some(streaming.to_string()),
                ..l
            });
        }
    }
    out.extend(body.into_iter().map(|l| {
        if l.text.is_empty() {
            l
        } else {
            VLine {
                text: format!("  {}", l.text),
                ..l
            }
        }
    }));
    out
}

// ---- subagent branches ------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchStatus {
    Done,
    Error,
    Interrupted,
    Orphaned,
}

/// A subagent branch, anchored to the turn that spawned it.
#[derive(Clone, Debug, Default)]
pub struct Branch {
    pub id: String,
    pub title: String,
    pub busy: bool,
    /// The branch's last SETTLED turn status — `running` never reported.
    pub status: Option<BranchStatus>,
    /// The persisted delegation outcome — whether its TURN completed.
    pub ok: Option<bool>,
    /// The message whose turn spawned it — where the card is drawn.
    pub origin_message_id: Option<String>,
    /// Parsed completion note once it finished.
    pub note: Option<SubagentNote>,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
}

/// What `branches_from` reads off a delegated child's session row.
#[derive(Clone, Debug)]
pub struct ChildRow {
    pub id: String,
    pub title: String,
    pub kind: SessionKind,
    pub busy: bool,
    pub last_turn_status: Option<TurnStatus>,
    pub outcome_ok: Option<bool>,
    pub origin_message_id: Option<String>,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
}

/// The branch rows a transcript draws. DELEGATED children only — forks,
/// compactions and handoffs must not dress up as subagent reports. A note
/// whose session is not among the children yields nothing here, deliberately:
/// the raw note then still renders (never both, never neither).
pub fn branches_from(thread: &[Message], children: &[ChildRow]) -> Vec<Branch> {
    let mut notes: HashMap<String, SubagentNote> = HashMap::new();
    for m in thread {
        if m.role != Role::System {
            continue;
        }
        let text = text_parts_joined(m, "\n");
        if let Some(note) = parse_subagent_note(&text) {
            notes.insert(note.session_id.clone(), note);
        }
    }
    children
        .iter()
        .filter(|row| is_delegated_kind(row.kind))
        .map(|row| {
            // `running` is not a settled status and must not be reported as one.
            let status = match row.last_turn_status {
                Some(TurnStatus::Done) => Some(BranchStatus::Done),
                Some(TurnStatus::Error) => Some(BranchStatus::Error),
                Some(TurnStatus::Interrupted) => Some(BranchStatus::Interrupted),
                Some(TurnStatus::Orphaned) => Some(BranchStatus::Orphaned),
                _ => None,
            };
            Branch {
                id: row.id.clone(),
                title: row.title.clone(),
                busy: row.busy,
                status,
                ok: row.outcome_ok,
                origin_message_id: row.origin_message_id.clone(),
                note: notes.get(&row.id).cloned(),
                tokens: row.tokens,
                cost_usd: row.cost_usd,
            }
        })
        .collect()
}

/// ` · 18k tok · $0.01` for a settled delegated session, or "" when nothing is
/// known. Zero tokens is a fact, not missing data.
fn billed(tokens: Option<i64>, cost_usd: Option<f64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = tokens {
        parts.push(format!("{} tok", fmt_tokens(t)));
    }
    if let Some(c) = cost_usd.filter(|c| *c > 0.0) {
        parts.push(fmt_usd(c));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" · {}", parts.join(" · "))
    }
}

/// Two most significant units only; nobody needs seconds on a two-day run.
fn fmt_elapsed(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else if s < 86400 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86400, (s % 86400) / 3600)
    }
}

/// The card for a finished subagent: header, files, capped report, next action.
fn subagent_note_lines(
    out: &mut Vec<VLine>,
    note: &SubagentNote,
    w: usize,
    full: bool,
    spent: &str,
) {
    let open = format!("open:{}", note.session_id);
    // Amber = stopped or orphaned (not the agent's fault); red is a genuine
    // failure. Four outcomes, four readings.
    let halted = note.status.starts_with("ORPHANED") || note.status.starts_with("STOPPED");
    let dot = if note.ok {
        accent("◆")
    } else if halted {
        warn("◆")
    } else {
        danger("◆")
    };
    let status_tag = if note.ok {
        accent(&note.status)
    } else if halted {
        warn(&note.status)
    } else {
        danger(&note.status)
    };
    let title = note
        .title
        .strip_prefix("subagent · ")
        .unwrap_or(&note.title);
    out.push(VLine {
        text: format!(
            "{dot} {}  {}{}",
            bold(title),
            clip(&status_tag, 200),
            dim(spent)
        ),
        click: Some(open.clone()),
        copy: None,
        src: None,
    });
    let file_note = if note.files_unknown {
        "changed files not reported".to_string()
    } else if !note.files.is_empty() {
        format!(
            "{} file{} · {}",
            note.files.len(),
            if note.files.len() == 1 { "" } else { "s" },
            note.files.join(", ")
        )
    } else {
        "no file changes".to_string()
    };
    push(out, &dim(&format!("  {file_note}")), w, Some(&open));
    if let Some(report) = &note.report {
        // Physical (post-wrap) lines are what floods the screen, so cap those.
        let physical: Vec<String> = md(report, None)
            .split('\n')
            .flat_map(|line| wrap(line, w.saturating_sub(2)))
            .collect();
        let shown = if full {
            physical.len()
        } else {
            physical.len().min(REPORT_LINES)
        };
        for l in &physical[..shown] {
            out.push(VLine {
                text: format!("{} {l}", dim("│")),
                click: Some(format!("report:{}", note.session_id)),
                copy: None,
                src: None,
            });
        }
        if physical.len() > shown {
            out.push(VLine {
                text: format!(
                    "{} {}",
                    dim("│"),
                    dim(&format!("… +{} more", physical.len() - shown))
                ),
                click: Some(format!("report:{}!full", note.session_id)),
                copy: None,
                src: None,
            });
        }
    }
    // There is nothing to merge; and the row names what actually opens it.
    push(
        out,
        &dim("  ↳ click to open it · or find it in the tree (^t) · its edits are already here"),
        w,
        Some(&open),
    );
}

fn branch_card_lines(out: &mut Vec<VLine>, b: &Branch, w: usize, is_full: &dyn Fn(&str) -> bool) {
    let inner = w.saturating_sub(2);
    let mut body: Vec<VLine> = Vec::new();
    let copy: String;
    if let Some(note) = &b.note {
        subagent_note_lines(
            &mut body,
            note,
            inner,
            is_full(&format!("report:{}", note.session_id)),
            &billed(b.tokens, b.cost_usd),
        );
        copy = note.report.clone().unwrap_or_else(|| note.title.clone());
    } else {
        // A blocking subagent reports in-band and leaves no note: the card
        // reads the session's own status. Blue = in flight, amber = stopped or
        // orphaned, red = failed.
        let (dot, tail) = if b.busy {
            (info("◆"), info(" ⋯ working"))
        } else {
            match (b.status, b.ok) {
                (Some(BranchStatus::Orphaned), _) => {
                    (warn("◆"), warn(" ◼ interrupted — the server restarted"))
                }
                (Some(BranchStatus::Interrupted), _) => (warn("◆"), warn(" ◼ interrupted")),
                (Some(BranchStatus::Error), _) | (_, Some(false)) => {
                    (danger("◆"), danger(" ✗ failed"))
                }
                _ => (accent("◆"), accent(" ✓ done")),
            }
        };
        let title = b.title.strip_prefix("subagent · ").unwrap_or(&b.title);
        body.push(VLine {
            text: format!(
                "{dot} {title}{}{}",
                dim(&tail),
                dim(&billed(b.tokens, b.cost_usd))
            ),
            click: Some(format!("open:{}", b.id)),
            copy: None,
            src: None,
        });
        copy = b.title.clone();
    }
    out.push(vl(""));
    out.extend(body.into_iter().map(|l| {
        if l.text.is_empty() {
            VLine {
                copy: Some(copy.clone()),
                ..l
            }
        } else {
            VLine {
                copy: Some(copy.clone()),
                text: format!("  {}", l.text),
                ..l
            }
        }
    }));
}

// ---- background shells ------------------------------------------------------

/// A background shell as the transcript shows it: the wire row plus what only
/// the UI fetched.
#[derive(Clone, Debug)]
pub struct JobView {
    pub job: BackgroundJob,
    pub tail: Vec<String>,
    pub output_lines: i64,
}

impl std::ops::Deref for JobView {
    type Target = BackgroundJob;
    fn deref(&self) -> &BackgroundJob {
        &self.job
    }
}

fn job_status_text(job: &JobView) -> String {
    use bough_core::schema::parts::JobStatus;
    if job.status == JobStatus::Running {
        return warn("⋯ running");
    }
    // A SIGNALLED JOB IS NOT A FINISHED ONE: `exitCode` is null for a
    // signalled process, and `?? 0` once read that as success.
    if let Some(sig) = &job.signal {
        return warn(&format!("◼ stopped ({sig})"));
    }
    match job.exit_code.unwrap_or(0) {
        0 => accent("✓ done"),
        code => danger(&format!("✗ exit {code}")),
    }
}

/// A background shell's card, kept after the exit — a build that failed while
/// you were reading something else must leave a user-visible trace.
pub fn job_card_lines(out: &mut Vec<VLine>, job: &JobView, w: usize, now: i64) {
    use bough_core::schema::parts::JobStatus;
    let inner = w.saturating_sub(2);
    let mut body: Vec<VLine> = Vec::new();
    let glyph = if job.status == JobStatus::Running || job.signal.is_some() {
        warn("⚙")
    } else if job.exit_code.unwrap_or(0) == 0 {
        accent("⚙")
    } else {
        danger("⚙")
    };
    let took = fmt_elapsed(job.exited_at.unwrap_or(now) - job.started_at);
    // The command is NOT repeated when it is already the title.
    let titled = !job.name.is_empty() && one_line(&job.name) != one_line(&job.command);
    let facts: Vec<String> = [
        if job.name.is_empty() {
            String::new()
        } else {
            job.id.clone()
        },
        if titled {
            clip(&one_line(&job.command), 60)
        } else {
            String::new()
        },
        took,
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    body.push(vl(format!(
        "{glyph} {} {}  {}",
        bold(if job.name.is_empty() {
            &job.id
        } else {
            &job.name
        }),
        job_status_text(job),
        dim(&facts.join(" · "))
    )));
    for line in &job.tail {
        for l in wrap(line, inner.saturating_sub(2)) {
            body.push(vl(format!("{} {}", dim("│"), dim(&l))));
        }
    }
    if job.output_lines > job.tail.len() as i64 {
        body.push(vl(dim(&format!(
            "  {} total",
            plural(job.output_lines, "line")
        ))));
    }
    let copy = std::iter::once(format!("{} · {}", job.id, job.command))
        .chain(job.tail.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n");
    out.push(vl(""));
    // Clicking the card OPENS the job — an exited job is off the rail, so the
    // card is the only door left to its output.
    let click = format!("job:{}:{}", job.session_id, job.id);
    out.extend(body.into_iter().map(|l| VLine {
        copy: Some(copy.clone()),
        click: Some(click.clone()),
        text: format!("  {}", l.text),
        src: l.src,
    }));
}

// ---- workflow runs ----------------------------------------------------------

/// What the transcript card needs from a run — structurally a
/// `WorkflowSummary`, declared here so this module stays a leaf.
#[derive(Clone, Debug)]
pub struct RunCardView {
    pub id: String,
    pub status: WorkflowStatus,
    pub agents: crate::store::state::WorkflowAgentCounts,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

fn run_status_text(run: &RunCardView) -> String {
    match run.status {
        WorkflowStatus::Running => warn("⋯ running"),
        WorkflowStatus::Paused => warn("⏸ paused"),
        WorkflowStatus::Stopped => dim("■ stopped"),
        WorkflowStatus::Error => danger("✗ error"),
        WorkflowStatus::Orphaned => danger("✗ orphaned"),
        // A run whose agents mostly failed is NOT a ✓.
        WorkflowStatus::Done => {
            if run.agents.failed > 0 {
                warn("⚠ done")
            } else {
                accent("✓ done")
            }
        }
    }
}

/// A launched workflow's card — the transcript's permanent handle on a
/// detached run. The part carries identity only; every number is read live
/// from the run row.
pub fn workflow_card_lines(
    out: &mut Vec<VLine>,
    id: &str,
    name: &str,
    description: &str,
    rerun: bool,
    run: Option<&RunCardView>,
    now: i64,
) {
    let mut body: Vec<VLine> = Vec::new();
    let glyph = match run {
        None => dim("⧉"),
        Some(r) if r.status == WorkflowStatus::Running || r.status == WorkflowStatus::Paused => {
            warn("⧉")
        }
        Some(r) if r.status == WorkflowStatus::Done && r.agents.failed == 0 => accent("⧉"),
        Some(_) => danger("⧉"),
    };
    // No run row yet: the card still renders — the alternative is a launch
    // that left no trace at all.
    let status = run
        .map(run_status_text)
        .unwrap_or_else(|| dim("· launched"));
    let mut facts: Vec<String> = Vec::new();
    if let Some(r) = run {
        facts.push(format!(
            "{}/{} agents",
            r.agents.done + r.agents.cached,
            r.agents.total
        ));
        if r.agents.failed > 0 {
            facts.push(format!("{} failed", r.agents.failed));
        }
        facts.push(fmt_elapsed(r.finished_at.unwrap_or(now) - r.created_at));
    }
    if rerun {
        facts.push("rerun".to_string());
    }
    let mut described = vec![clip(&one_line(description), 56)];
    described.extend(facts);
    body.push(vl(format!(
        "{glyph} {} {status}  {}",
        bold(name),
        dim(&described
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "))
    )));
    // The way in, named on the card; click FIRST (^w is composer-owned).
    body.push(vl(format!(
        "{}{}",
        dim("⎿ "),
        dim("click to open the run view · or ^w")
    )));
    let copy = format!("{name} — {description} ({id})");
    out.extend(body.into_iter().map(|l| VLine {
        copy: Some(copy.clone()),
        click: Some(format!("workflow:{id}")),
        text: l.text,
        src: l.src,
    }));
}

// ---- the whole transcript ---------------------------------------------------

#[derive(Default)]
pub struct BuildOptions {
    pub streaming: HashMap<String, String>,
    pub branches: Vec<Branch>,
    pub tool_logs: Option<HashMap<String, Vec<String>>>,
    pub jobs: Vec<JobView>,
    /// Live run rows for the `workflow` parts in the thread.
    pub runs: Vec<RunCardView>,
    /// The session's permanent ledger, oldest first — interleaved by time.
    pub marks: Vec<TranscriptMark>,
    /// Installed skill names. None = not fetched, and no row is claimed.
    pub skills: Option<Vec<String>>,
    /// Injected clock — elapsed times are the only thing here that needs one.
    pub now: Option<i64>,
    /// Command-history tags this session was primed with — the top `#` row.
    pub primed_tags: Vec<String>,
    /// Labels of the `AGENTS.md` files every turn injects — the second `#` row.
    pub project_rules: Vec<String>,
}

/// One `#` margin row: a prefix and a `·`-joined list, elided at the
/// terminal's width. `suffix` is spent only when the list actually fit.
fn margin_row(prefix: &str, items: &[String], w: usize, suffix: &str) -> VLine {
    let mut shown = String::new();
    let mut elided = false;
    for item in items {
        let next = if shown.is_empty() {
            item.clone()
        } else {
            format!("{shown} · {item}")
        };
        if format!("{prefix}{next}").chars().count() > w.saturating_sub(2) {
            shown = format!("{shown} …");
            elided = true;
            break;
        }
        shown = next;
    }
    let with_suffix = format!("{prefix}{shown}{suffix}");
    let row = if !elided && !suffix.is_empty() && with_suffix.chars().count() <= w.saturating_sub(2)
    {
        with_suffix
    } else {
        format!("{prefix}{shown}")
    };
    VLine {
        text: dim(&row),
        click: None,
        copy: Some(row),
        src: None,
    }
}

/// The two `#` margin rows, in the order the transcript emits them
/// (lines.ts:1225-1240). Tags ABOVE rules: the tag set is this session's, the
/// rule sheet is the project's, and the more static of the two sits under.
/// An empty list is no row — never an empty one.
pub(crate) fn margin_rows(
    primed_tags: &[String],
    project_rules: &[String],
    w: usize,
) -> Vec<VLine> {
    let mut out = Vec::new();
    // The memory margin: `#` means remembered, not happening now.
    if !primed_tags.is_empty() {
        out.push(margin_row("# this repo remembers: ", primed_tags, w, ""));
    }
    if !project_rules.is_empty() {
        out.push(margin_row("# rules: ", project_rules, w, " · /rules"));
    }
    out
}

/// One mark as one row, two columns in. A destructive mark is amber — the
/// only row that reports something the user cannot get back.
fn mark_line(mark: &TranscriptMark) -> VLine {
    VLine {
        text: format!(
            "  {}",
            if mark.kind == MarkKind::Destructive {
                warn(&mark.text)
            } else {
                dim(&mark.text)
            }
        ),
        click: None,
        copy: Some(mark.text.clone()),
        src: None,
    }
}

/// The installed skills a user message NAMED, in order. Matched against the
/// INSTALLED list, so a row appears only when a skill really was loaded.
pub fn skills_named(text: &str, installed: &[String]) -> Vec<String> {
    if installed.is_empty() {
        return Vec::new();
    }
    static SKILL_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(^|\s)/([a-z0-9][a-z0-9:_-]*)").unwrap());
    let known: Vec<String> = installed.iter().map(|n| n.to_lowercase()).collect();
    let mut found: Vec<String> = Vec::new();
    for c in SKILL_RE.captures_iter(text) {
        let name = c[2].to_lowercase();
        if known.contains(&name) && !found.contains(&name) {
            found.push(name);
        }
    }
    found
}

pub fn build_lines(
    thread: &[Message],
    is_expanded: &dyn Fn(&str) -> bool,
    is_full: &dyn Fn(&str) -> bool,
    w: usize,
    opts: &BuildOptions,
) -> Vec<VLine> {
    use bough_core::schema::parts::JobStatus;
    let now = opts.now.unwrap_or(0);
    let runs_by_id: HashMap<String, RunCardView> = opts
        .runs
        .iter()
        .map(|r| (r.id.clone(), r.clone()))
        .collect();
    // A note that already renders as a card is dropped from the raw thread,
    // and so is a job wake note while its card is showing.
    let noted_ids: Vec<&str> = opts
        .branches
        .iter()
        .filter_map(|b| b.note.as_ref().map(|n| n.session_id.as_str()))
        .collect();
    let job_ids: Vec<&str> = opts.jobs.iter().map(|j| j.id.as_str()).collect();
    let mut by_origin: HashMap<String, Vec<&Branch>> = HashMap::new();
    let mut origin_order: Vec<String> = Vec::new();
    let mut orphans: Vec<&Branch> = Vec::new();
    for b in &opts.branches {
        // A running subagent lives in the pinned rail; the transcript keeps
        // its finished report.
        if b.busy && b.note.is_none() {
            continue;
        }
        match &b.origin_message_id {
            Some(id) => {
                if !by_origin.contains_key(id) {
                    origin_order.push(id.clone());
                }
                by_origin.entry(id.clone()).or_default().push(b);
            }
            None => orphans.push(b),
        }
    }
    let mut out: Vec<VLine> = margin_rows(&opts.primed_tags, &opts.project_rules, w);
    // The ledger, drained in step with the thread.
    let mut marks: Vec<&TranscriptMark> = opts.marks.iter().collect();
    marks.sort_by_key(|m| m.at);
    let mut mark_at = 0usize;
    // Exited job cards, drained the same way — IN TIME ORDER, not at the tail.
    let mut timed: Vec<&JobView> = opts
        .jobs
        .iter()
        .filter(|j| j.status != JobStatus::Running)
        .collect();
    timed.sort_by_key(|j| j.exited_at.unwrap_or(j.started_at));
    let mut job_at = 0usize;

    let flush = |out: &mut Vec<VLine>, mark_at: &mut usize, job_at: &mut usize, until: i64| {
        while *mark_at < marks.len() && marks[*mark_at].at <= until {
            out.push(mark_line(marks[*mark_at]));
            *mark_at += 1;
        }
        while *job_at < timed.len()
            && timed[*job_at]
                .exited_at
                .unwrap_or(timed[*job_at].started_at)
                <= until
        {
            job_card_lines(out, timed[*job_at], w, now);
            *job_at += 1;
        }
    };

    // True once a pending reply has rendered: any user message after it was
    // posted into a running turn and is only QUEUED server-side.
    let mut mid_turn = false;
    for m in thread {
        if m.role == Role::System {
            let t = text_parts_joined(m, "\n");
            if let Some(parsed) = parse_subagent_note(&t) {
                if noted_ids.contains(&parsed.session_id.as_str()) {
                    continue;
                }
            }
            if let Some(bg) = parse_bg_note(&t) {
                if job_ids.contains(&bg.as_str()) {
                    continue;
                }
            }
        }
        flush(&mut out, &mut mark_at, &mut job_at, m.created_at);
        out.extend(message_lines(
            m,
            is_expanded,
            is_full,
            w,
            opts.streaming.get(&m.id).map(String::as_str),
            opts.tool_logs.as_ref(),
            Some(&runs_by_id),
            now,
        ));
        // Named directly under the message that named it.
        if m.role == Role::User {
            if let Some(skills) = opts.skills.as_ref().filter(|s| !s.is_empty()) {
                let text = text_parts_joined(m, " ");
                let named = skills_named(&text, skills);
                if !named.is_empty() {
                    out.push(VLine {
                        text: format!(
                            "  {}",
                            dim(&format!(
                                "↳ skill loaded: {}",
                                named
                                    .iter()
                                    .map(|n| format!("/{n}"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                        ),
                        click: None,
                        copy: Some(named.join(", ")),
                        src: None,
                    });
                }
            }
        }
        // An honest ack under a steered message.
        if mid_turn && m.role == Role::User {
            out.push(vl(format!(
                "  {}",
                dim("⧖ queued — the agent sees this after the current step")
            )));
        }
        if m.pending {
            mid_turn = true;
        }
        if let Some(list) = by_origin.remove(&m.id) {
            for b in list {
                branch_card_lines(&mut out, b, w, is_full);
            }
        }
    }
    // Everything that happened after the last message.
    while mark_at < marks.len() {
        out.push(mark_line(marks[mark_at]));
        mark_at += 1;
    }
    // Anything left is anchored to a message not in this thread.
    let mut tail: Vec<&Branch> = orphans;
    for id in &origin_order {
        if let Some(list) = by_origin.remove(id) {
            tail.extend(list);
        }
    }
    if !tail.is_empty() {
        out.push(vl(""));
        out.push(vl(format!(
            "  {}",
            dim("subagents with no spawn point in this thread")
        )));
    }
    for b in tail {
        branch_card_lines(&mut out, b, w, is_full);
    }
    // Whatever exited AFTER the last message — then still-running jobs, which
    // have no exit time and belong at the bottom next to the composer.
    while job_at < timed.len() {
        job_card_lines(&mut out, timed[job_at], w, now);
        job_at += 1;
    }
    for job in &opts.jobs {
        if job.status == JobStatus::Running {
            job_card_lines(&mut out, job, w, now);
        }
    }
    out
}

// ---- the viewport window ----------------------------------------------------

/// Rows the transcript body occupies inside the chat's TOTAL height. Two
/// strips always reserved, plus one per queued message, plus one for a notice.
pub fn chat_body_height(height: usize, queued: usize, has_notice: bool) -> usize {
    height
        .saturating_sub(queued + 2 + usize::from(has_notice))
        .max(1)
}

pub struct VisibleSlice<'a> {
    pub start: usize,
    pub rows: &'a [VLine],
    /// What remains below — keeps a scrolled-up reader from mistaking an old
    /// frame for the current one.
    pub more: usize,
    pub pct: u32,
}

/// The slice a viewport of `height` rows shows, `scroll_off` lines up from the
/// live tail.
pub fn visible_slice(lines: &[VLine], height: usize, scroll_off: usize) -> VisibleSlice<'_> {
    let h = height.max(1);
    let max_off = lines.len().saturating_sub(h);
    let off = scroll_off.min(max_off);
    let start = lines.len().saturating_sub(h + off);
    VisibleSlice {
        start,
        rows: &lines[start..(start + h).min(lines.len())],
        more: off,
        pct: if max_off == 0 {
            100
        } else {
            ((start as f64 / max_off as f64) * 100.0).round() as u32
        },
    }
}

/// The transcript line under a screen slot — the exact inverse of the render
/// loop INCLUDING the pad: a short conversation hangs from the BOTTOM, so the
/// first `body - rows.len()` slots are empty air and resolve to None.
pub fn line_at_slot(
    lines: &[VLine],
    body: usize,
    scroll_off: usize,
    slot: usize,
) -> Option<&VLine> {
    let v = visible_slice(lines, body, scroll_off);
    let pad = body.max(1).saturating_sub(v.rows.len());
    if slot < pad {
        return None;
    }
    v.rows.get(slot - pad)
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/lines.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::{strip_ansi, width};
    use serde_json::json;

    fn open(_: &str) -> bool {
        true
    }
    fn closed(_: &str) -> bool {
        false
    }

    fn msg(id: &str, over: Value) -> Message {
        let mut base = json!({
            "id": id, "sessionId": "s1", "role": "supervisor", "parts": [],
            "pending": false, "createdAt": 1,
        });
        for (k, v) in over.as_object().unwrap() {
            base[k] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn call(id: &str, code: &str) -> Value {
        json!({"type": "tool_call", "id": id, "name": "run_steps", "input": {"code": code}})
    }

    fn result(call_id: &str, output: &str) -> Value {
        json!({"type": "tool_result", "callId": call_id, "output": output, "isError": false})
    }

    fn result_over(call_id: &str, output: &str, over: Value) -> Value {
        let mut base = result(call_id, output);
        for (k, v) in over.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    fn joined(lines: &[VLine]) -> String {
        strip_ansi(
            &lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    fn mlines(m: &Message, exp: fn(&str) -> bool, full: fn(&str) -> bool, w: usize) -> Vec<VLine> {
        message_lines(m, &exp, &full, w, None, None, None, 0)
    }

    // ---- wrapping ----

    #[test]
    fn every_emitted_line_fits_the_width_long_prose_long_code_long_output() {
        let long_prose = "word ".repeat(60);
        let long_code = format!("const x = {};", "'y'.repeat(1) + ".repeat(20));
        let long_out = "0123456789".repeat(12);
        let m = msg(
            "m1",
            json!({"parts": [
                {"type": "text", "text": long_prose},
                call("c1", &long_code),
                result("c1", &long_out),
            ]}),
        );
        for l in mlines(&m, open, closed, 60) {
            assert!(
                width(&l.text) <= 60,
                "{} cols: {:?}",
                width(&l.text),
                strip_ansi(&l.text)
            );
        }
    }

    #[test]
    fn a_hard_wrapped_block_keeps_its_gutter_on_every_physical_line() {
        let long_out = "x".repeat(150);
        let m = msg(
            "m1",
            json!({"parts": [call("c1", "a()"), result("c1", &long_out)]}),
        );
        let lines = mlines(&m, open, closed, 60);
        let block: Vec<&VLine> = lines
            .iter()
            .filter(|l| strip_ansi(&l.text).contains('x'))
            .collect();
        assert!(block.len() >= 2, "the long line must wrap");
        for l in block {
            assert!(
                strip_ansi(&l.text).trim_start().starts_with('│'),
                "{:?}",
                l.text
            );
        }
    }

    // ---- tool folds ----

    #[test]
    fn one_round_of_program_plus_result_is_one_step_not_two_entries() {
        let m = msg(
            "m1",
            json!({"parts": [call("c1", "a()"), result("c1", "1"), call("c2", "b()"), result("c2", "2")]}),
        );
        let lines = mlines(&m, closed, closed, 120);
        let heads: Vec<&VLine> = lines
            .iter()
            .filter(|l| strip_ansi(&l.text).contains("steps"))
            .collect();
        assert_eq!(heads.len(), 1);
        assert!(strip_ansi(&heads[0].text).contains("2 steps"));
    }

    #[test]
    fn a_running_call_is_visible_on_the_collapsed_header_and_shows_live_console_output() {
        let m = msg(
            "m1",
            json!({"pending": true, "parts": [call("c1", "console.log('x')")]}),
        );
        let mut logs: HashMap<String, Vec<String>> = HashMap::new();
        logs.insert("c1".into(), vec!["first".into(), "second".into()]);
        let head_lines = message_lines(&m, &closed, &closed, 120, None, Some(&logs), None, 0);
        let head = head_lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("step"))
            .unwrap();
        let head_text = strip_ansi(&head.text);
        // The live marker leads the row, where a 100-column screen cannot clip it.
        assert!(head_text.contains('⚙'), "{head_text}");
        assert!(
            head_text.find('⚙').unwrap() < head_text.find("step").unwrap(),
            "{head_text}"
        );

        let live = joined(&message_lines(
            &m,
            &open,
            &closed,
            120,
            None,
            Some(&logs),
            None,
            0,
        ));
        assert!(live.contains("↳ output (live)"));
        assert!(live.contains("first") && live.contains("second"));

        // Once the result lands the live buffer is REPLACED, not appended.
        let done = msg(
            "m1",
            json!({"pending": true, "parts": [call("c1", "console.log('x')"), result("c1", "first\nsecond")]}),
        );
        let settled = joined(&message_lines(
            &done,
            &open,
            &closed,
            120,
            None,
            Some(&logs),
            None,
            0,
        ));
        assert!(!settled.contains("(live)"));
        assert_eq!(settled.matches("first").count(), 1);
    }

    #[test]
    fn caps_a_long_program_and_a_long_output_truncate_only_full_lifts_them() {
        let code = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = (0..60)
            .map(|i| format!("out {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let m = msg(
            "m1",
            json!({"parts": [call("c1", &code), result("c1", &out)]}),
        );

        let capped = joined(&mlines(&m, open, closed, 120));
        assert!(capped.contains("line 13")); // CODE_LINES = 14
        assert!(!capped.contains("line 14"));
        assert!(capped.contains("out 19")); // OUTPUT_LINES = 20
        assert!(!capped.contains("out 20"));
        assert!(capped.contains("more lines"));

        // The cap-lift is its own key, so expand-all cannot dump the whole thing.
        let lines = mlines(&m, open, closed, 120);
        let more = lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("more lines"))
            .unwrap();
        assert_eq!(more.click.as_deref(), Some("m1:0!full"));
        let full = joined(&mlines(&m, open, open, 120));
        assert!(full.contains("line 39") && full.contains("out 59"));
        assert!(!full.contains("more lines"));
    }

    #[test]
    fn an_interrupted_result_keeps_its_partial_output_and_never_reads_done() {
        let m = msg(
            "m1",
            json!({"parts": [
                call("c1", "loop()"),
                result_over("c1", "tick-1\ntick-2", json!({"interrupted": true})),
            ]}),
        );
        let text = joined(&mlines(&m, open, closed, 120));
        assert!(text.contains("tick-1") && text.contains("tick-2"));
        assert!(text.contains("⏹ interrupted"));
        assert!(!text.contains("✓ done"));
        // …and it is legible without expanding.
        let lines = mlines(&m, closed, closed, 120);
        let head = lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("step"))
            .unwrap();
        assert!(strip_ansi(&head.text).contains("⏹ interrupted"));
    }

    #[test]
    fn an_errored_result_is_marked_on_the_closed_header() {
        let m = msg(
            "m1",
            json!({"parts": [
                call("c1", "boom()"),
                result_over("c1", "[program error] boom", json!({"isError": true})),
            ]}),
        );
        let lines = mlines(&m, closed, closed, 120);
        let head = lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("step"))
            .unwrap();
        assert!(strip_ansi(&head.text).contains("✗ error"), "{}", head.text);
    }

    #[test]
    fn a_stop_the_model_wrote_as_prose_is_not_shown_to_the_user() {
        let fenced = msg(
            "m1",
            json!({"parts": [{"type": "text", "text": "The image is filled with green.\n\n```\n<stop>\n```"}]}),
        );
        let out = joined(&mlines(&fenced, closed, closed, 120));
        assert!(out.contains("filled with green"), "{out}");
        assert!(!out.contains("<stop>"), "{out}");

        let bare = msg(
            "m2",
            json!({"parts": [{"type": "text", "text": "Done.\n<stop>"}]}),
        );
        let out2 = joined(&mlines(&bare, closed, closed, 120));
        assert!(out2.contains("Done."), "{out2}");
        assert!(!out2.contains("<stop>"), "{out2}");

        // NOT stripped mid-message.
        let about = msg(
            "m3",
            json!({"parts": [{"type": "text", "text": "Call `<stop>` in the same response as your final text."}]}),
        );
        assert!(joined(&mlines(&about, closed, closed, 120)).contains("<stop>"));
    }

    #[test]
    fn declining_a_question_is_not_a_failed_round() {
        let m = msg(
            "m1",
            json!({"parts": [
                call("c1", "await ask('Enable strict mode?', ['yes', 'no'])"),
                result_over("c1", "the user declined", json!({"isError": true})),
                {"type": "ask", "id": "a1", "question": "Enable strict mode?", "status": "declined"},
            ]}),
        );
        let lines = mlines(&m, closed, closed, 120);
        let head = lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("step"))
            .unwrap();
        let head_text = strip_ansi(&head.text);
        assert!(head_text.contains("⏹ declined"), "{head_text}");
        assert!(!head_text.contains("✗ error"), "{head_text}");

        // An ANSWERED question whose program then genuinely failed is an error.
        let failed = msg(
            "m2",
            json!({"parts": [
                call("c1", "await ask('pick', ['a'])"),
                result_over("c1", "[program error] boom", json!({"isError": true})),
                {"type": "ask", "id": "a1", "question": "pick", "status": "answered", "answer": "a"},
            ]}),
        );
        let lines2 = mlines(&failed, closed, closed, 120);
        let head2 = lines2
            .iter()
            .find(|l| strip_ansi(&l.text).contains("step"))
            .unwrap();
        assert!(
            strip_ansi(&head2.text).contains("✗ error"),
            "{}",
            head2.text
        );
    }

    #[test]
    fn a_round_that_failed_once_and_then_worked_is_amber_with_a_count_not_red() {
        let m = msg(
            "m1",
            json!({"parts": [
                call("c1", "one()"),
                result_over("c1", "[program error] no", json!({"isError": true})),
                call("c2", "two()"),
                result("c2", "ok"),
            ]}),
        );
        let lines = mlines(&m, closed, closed, 120);
        let head = lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("step"))
            .unwrap();
        let text = strip_ansi(&head.text);
        assert!(text.contains("⚠ 1 of 2 failed"), "{text}");
        assert!(!text.contains("✗ error"), "{text}");
    }

    #[test]
    fn a_command_that_exited_non_zero_is_flagged_on_the_collapsed_row() {
        let m = msg(
            "m1",
            json!({"parts": [
                call("c1", "await bash('make', 'build')"),
                result("c1", "make: *** error\n[exit code 2]"),
            ]}),
        );
        let lines = mlines(&m, closed, closed, 120);
        let head = lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("step"))
            .unwrap();
        assert!(
            strip_ansi(&head.text).contains("⚠ 1 command failed"),
            "{}",
            head.text
        );
    }

    // ---- reasoning / other part kinds ----

    #[test]
    fn reasoning_folds_a_collapsed_gist_line_an_expanded_gutter_block() {
        let m = msg(
            "m1",
            json!({"parts": [
                {"type": "reasoning", "text": "Let me look at the auth flow.\nSecond thought.\nThird."},
                {"type": "text", "text": "done"},
            ]}),
        );
        let collapsed = mlines(&m, closed, closed, 120);
        let head = collapsed
            .iter()
            .find(|l| strip_ansi(&l.text).contains("thinking"))
            .unwrap();
        assert!(strip_ansi(&head.text).contains('▸'));
        assert!(strip_ansi(&head.text).contains("Let me look at the auth flow."));
        assert_eq!(head.click.as_deref(), Some("m1:0"));
        assert!(!joined(&collapsed).contains("Second thought."));

        let expanded = mlines(&m, open, closed, 120);
        assert!(expanded.iter().any(|l| strip_ansi(&l.text).contains('▾')
            && strip_ansi(&l.text).contains("thinking (3 lines)")));
        assert!(joined(&expanded).contains("Second thought."));
        // The prose is never folded — it is the answer.
        assert!(joined(&collapsed).contains("done"));
    }

    #[test]
    fn reasoning_with_no_text_renders_nothing_at_all() {
        let m = msg(
            "m1",
            json!({"parts": [{"type": "reasoning", "text": "  \n "}, {"type": "text", "text": "hi"}]}),
        );
        assert!(!joined(&mlines(&m, closed, closed, 120)).contains("thinking"));
    }

    #[test]
    fn a_settled_ask_renders_as_one_always_visible_q_to_a_line() {
        let answered = msg(
            "m1",
            json!({"parts": [{"type": "ask", "id": "q1", "question": "Ship it?", "status": "answered", "answer": "yes"}]}),
        );
        let lines = mlines(&answered, closed, closed, 120);
        let line = lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("Ship it?"))
            .unwrap();
        assert!(strip_ansi(&line.text).contains('→') && strip_ansi(&line.text).contains("yes"));
        assert_eq!(line.copy.as_deref(), Some("Ship it? → yes"));

        let declined = msg(
            "m2",
            json!({"parts": [{"type": "ask", "id": "q2", "question": "Ship it?", "status": "declined"}]}),
        );
        assert!(joined(&mlines(&declined, closed, closed, 120)).contains("declined"));
    }

    #[test]
    fn an_image_part_renders_as_a_compact_placeholder_and_copies_its_path() {
        let m = msg(
            "m1",
            json!({"role": "user", "parts": [
                {"type": "text", "text": "what is this?"},
                {"type": "image", "path": "/home/u/.bough/attachments/x.png", "mediaType": "image/png", "name": "shot.png", "size": 34567},
            ]}),
        );
        let lines = mlines(&m, closed, closed, 120);
        let img = lines
            .iter()
            .find(|l| strip_ansi(&l.text).contains("🖼"))
            .unwrap();
        assert!(strip_ansi(&img.text).contains("shot.png (34 KB)"));
        assert_eq!(
            img.copy.as_deref(),
            Some("/home/u/.bough/attachments/x.png")
        );
    }

    #[test]
    fn an_image_system_note_collapses_onto_the_placeholder_no_role_label_no_path() {
        let m = msg(
            "m1",
            json!({"role": "system", "parts": [
                {"type": "text", "text": "[image] /tmp/shot.png — the failing screen"},
                {"type": "image", "path": "/tmp/shot.png", "mediaType": "image/png", "name": "shot.png", "size": 2048},
            ]}),
        );
        let lines = mlines(&m, closed, closed, 120);
        assert_eq!(lines.len(), 2); // one spacer, one line
        assert!(strip_ansi(&lines[1].text).contains("shot.png — the failing screen · 2 KB"));
        assert!(!joined(&lines).contains("system"));
        assert!(!joined(&lines).contains("/tmp/shot.png"));
    }

    #[test]
    fn a_workflow_completion_report_is_folded_in_the_transcript() {
        let report = [
            "[workflow done] \"audit all handlers\" (wf-1) — 2/2 agents succeeded.",
            "Replay: not a relaunch — this run started fresh and journalled as it went.",
            "Result:",
            &serde_json::to_string_pretty(&json!({"findings": ["one", "two"]})).unwrap(),
        ]
        .join("\n");
        let m = msg(
            "wf-note",
            json!({"role": "system", "parts": [{"type": "text", "text": report}]}),
        );
        let collapsed = mlines(&m, closed, closed, 120);
        let head = collapsed
            .iter()
            .find(|l| strip_ansi(&l.text).contains("audit all handlers"))
            .unwrap();
        let head_text = strip_ansi(&head.text);
        assert!(
            head_text.contains('▸') && head_text.contains("2/2 agents succeeded"),
            "{head_text}"
        );
        assert!(!joined(&collapsed).contains("\"findings\""));
        assert_eq!(head.click.as_deref(), Some("wf-note:workflow"));

        let expanded = mlines(&m, open, closed, 120);
        assert!(joined(&expanded).contains("\"findings\""));
        assert!(expanded
            .iter()
            .any(|l| strip_ansi(&l.text).contains("▾ workflow")));
    }

    // ---- system-note parsing ----

    fn note(status: &str, files: &str, report: Option<&str>) -> String {
        [
            format!("[subagent finished] \"extract token logic\" (sub-1) — {status}."),
            format!("Changed files: {files}."),
            match report {
                Some(r) => format!("Report:\n{r}"),
                None => "No report.".to_string(),
            },
            "It worked in THIS session's checkout, so its edits are already here — read them before building on top; there is nothing to merge.".to_string(),
        ]
        .join("\n")
    }

    #[test]
    fn parse_subagent_note_the_real_note_shape() {
        let p = parse_subagent_note(&note(
            "finished",
            "a.ts, b.ts",
            Some("# Findings\nAll good."),
        ))
        .unwrap();
        assert_eq!(p.title, "extract token logic");
        assert_eq!(p.session_id, "sub-1");
        assert!(p.ok);
        assert_eq!(p.files, vec!["a.ts", "b.ts"]);
        assert_eq!(p.report.as_deref(), Some("# Findings\nAll good."));
    }

    #[test]
    fn parse_subagent_note_the_four_outcomes_stay_distinguishable() {
        assert!(
            parse_subagent_note(&note("finished", "none", None))
                .unwrap()
                .ok
        );
        let failed = parse_subagent_note(&note(
            "FAILED — its turn errored. Nothing retried it",
            "x",
            None,
        ))
        .unwrap();
        assert!(!failed.ok);
        assert!(failed.status.starts_with("FAILED"));
        assert!(
            parse_subagent_note(&note("STOPPED — it was interrupted", "x", None))
                .unwrap()
                .status
                .starts_with("STOPPED")
        );
        assert!(
            parse_subagent_note(&note("ORPHANED — the server restarted", "x", None))
                .unwrap()
                .status
                .starts_with("ORPHANED")
        );
        assert!(parse_subagent_note("just a normal message").is_none());
    }

    #[test]
    fn parse_subagent_note_not_reported_is_unknown_not_a_file_named_so() {
        let p = parse_subagent_note(&note("finished", "not reported", None)).unwrap();
        assert!(p.files.is_empty());
        assert!(p.files_unknown);
    }

    #[test]
    fn parse_bg_note_and_parse_image_note() {
        assert_eq!(
            parse_bg_note("[background] bg_2 finished (exit 1) — command \"make\", 3 lines")
                .as_deref(),
            Some("bg_2")
        );
        assert_eq!(parse_bg_note("hello"), None);
        assert_eq!(
            parse_image_note("[image] /tmp/a.png — note"),
            Some(("/tmp/a.png".to_string(), Some("note".to_string())))
        );
        assert_eq!(
            parse_image_note("[image] /tmp/a.png"),
            Some(("/tmp/a.png".to_string(), None))
        );
        assert_eq!(parse_image_note("not an image note"), None);
    }

    // ---- the whole transcript ----

    fn branch(id: &str) -> Branch {
        Branch {
            id: id.to_string(),
            title: id.to_string(),
            ..Default::default()
        }
    }

    fn build(thread: &[Message], w: usize, opts: &BuildOptions) -> Vec<VLine> {
        build_lines(thread, &closed, &closed, w, opts)
    }

    #[test]
    fn a_finished_subagents_card_replaces_its_raw_note_at_the_spawn_point() {
        let note_text = note("finished", "a.ts", Some("Found three call sites."));
        let thread = vec![
            msg(
                "u1",
                json!({"role": "user", "parts": [{"type": "text", "text": "go"}]}),
            ),
            msg(
                "a1",
                json!({"parts": [call("c1", "await agent('x')"), result("c1", "{}")]}),
            ),
            msg(
                "n1",
                json!({"role": "system", "parts": [{"type": "text", "text": note_text}]}),
            ),
        ];
        let b = Branch {
            id: "sub-1".into(),
            title: "extract token logic".into(),
            origin_message_id: Some("a1".into()),
            note: parse_subagent_note(&note_text),
            ..Default::default()
        };
        let opts = BuildOptions {
            branches: vec![b],
            ..Default::default()
        };
        let text = joined(&build(&thread, 100, &opts));
        assert!(!text.contains("[subagent finished]")); // the raw wall is gone
        assert!(text.contains("extract token logic"));
        assert!(text.contains("Found three call sites."));
        assert!(text.contains("its edits are already here"), "{text}");
        assert!(text.contains("click to open it"), "{text}");
        assert!(!text.contains("^s opens it"), "{text}");

        // With no branch to draw the card, the note itself must survive.
        let bare = joined(&build(&thread, 100, &BuildOptions::default()));
        assert!(bare.contains("[subagent finished]"));
    }

    #[test]
    fn a_running_subagent_is_left_to_the_rail_a_card_with_no_spawn_point_tails_out() {
        let thread = vec![msg(
            "a1",
            json!({"parts": [{"type": "text", "text": "working"}]}),
        )];
        let running = Branch {
            busy: true,
            origin_message_id: Some("a1".into()),
            title: "live one".into(),
            ..branch("sub-1")
        };
        let opts = BuildOptions {
            branches: vec![running],
            ..Default::default()
        };
        assert!(!joined(&build(&thread, 100, &opts)).contains("live one"));
        // A branch whose spawn turn a fork dropped still renders, at the tail.
        let stranded = Branch {
            origin_message_id: Some("gone".into()),
            title: "stranded".into(),
            ..branch("sub-2")
        };
        let opts = BuildOptions {
            branches: vec![stranded],
            ..Default::default()
        };
        let out = joined(&build(&thread, 100, &opts));
        assert!(out.contains("subagents with no spawn point in this thread"));
        assert!(out.contains("stranded"));
    }

    #[test]
    fn branch_cards_state_the_real_outcome_failed_and_orphaned_never_read_done() {
        let thread = vec![msg("a1", json!({"parts": [{"type": "text", "text": "x"}]}))];
        let render = |b: Branch| {
            let opts = BuildOptions {
                branches: vec![Branch {
                    origin_message_id: Some("a1".into()),
                    title: "child".into(),
                    id: "s".into(),
                    ..b
                }],
                ..Default::default()
            };
            joined(&build(&thread, 100, &opts))
        };
        let base = Branch::default;
        assert!(render(Branch {
            status: Some(BranchStatus::Done),
            ok: Some(true),
            ..base()
        })
        .contains("✓ done"));
        assert!(render(Branch {
            status: Some(BranchStatus::Error),
            ..base()
        })
        .contains("✗ failed"));
        assert!(render(Branch {
            status: Some(BranchStatus::Done),
            ok: Some(false),
            ..base()
        })
        .contains("✗ failed"));
        assert!(render(Branch {
            status: Some(BranchStatus::Interrupted),
            ..base()
        })
        .contains("◼ interrupted"));
        assert!(render(Branch {
            status: Some(BranchStatus::Orphaned),
            ..base()
        })
        .contains("the server restarted"));

        // WHAT IT COST, next to the outcome.
        let paid = render(Branch {
            status: Some(BranchStatus::Done),
            ok: Some(true),
            tokens: Some(18_000),
            cost_usd: Some(0.031),
            ..base()
        });
        assert!(paid.contains("18k tok"), "{paid}");
        assert!(paid.contains("$0.03"), "{paid}");
        // Zero tokens is a fact, not missing data.
        assert!(render(Branch {
            status: Some(BranchStatus::Interrupted),
            tokens: Some(0),
            ..base()
        })
        .contains("0 tok"));
    }

    #[test]
    fn a_message_steered_into_a_running_turn_carries_a_queued_ack() {
        let with_pending = vec![
            msg(
                "u1",
                json!({"role": "user", "parts": [{"type": "text", "text": "go"}]}),
            ),
            msg(
                "a1",
                json!({"pending": true, "parts": [{"type": "text", "text": "working"}]}),
            ),
            msg(
                "u2",
                json!({"role": "user", "parts": [{"type": "text", "text": "also this"}]}),
            ),
        ];
        assert!(joined(&build(&with_pending, 100, &BuildOptions::default())).contains("⧖ queued"));
        // The first user message, before any pending reply, is not queued.
        assert!(
            !joined(&build(&with_pending[..1], 100, &BuildOptions::default())).contains("⧖ queued")
        );
    }

    #[test]
    fn marks_land_where_they_happened_and_a_destructive_one_outlives_its_toast() {
        let thread = vec![
            msg(
                "u1",
                json!({"role": "user", "parts": [{"type": "text", "text": "go"}], "createdAt": 10}),
            ),
            msg(
                "a1",
                json!({"parts": [{"type": "text", "text": "done"}], "createdAt": 20}),
            ),
            msg(
                "u2",
                json!({"role": "user", "parts": [{"type": "text", "text": "again"}], "createdAt": 40}),
            ),
        ];
        let mark = |id: &str, at: i64, kind: MarkKind, text: &str| TranscriptMark {
            id: id.to_string(),
            session_id: "s1".to_string(),
            at,
            kind,
            text: text.to_string(),
        };
        let opts = BuildOptions {
            marks: vec![
                mark("m2", 30, MarkKind::Turn, "✓ 14s · 3.2k tok · $0.021"),
                mark("m1", 15, MarkKind::Destructive, "reverted README.md"),
                mark("m3", 90, MarkKind::Destructive, "killed bg_7"),
            ],
            ..Default::default()
        };
        let lines = build(&thread, 100, &opts);
        let rows: Vec<String> = lines
            .iter()
            .map(|l| strip_ansi(&l.text).trim().to_string())
            .collect();
        let at = |text: &str| rows.iter().position(|r| r == text).unwrap();
        assert!(at("reverted README.md") > at("go"));
        assert!(at("reverted README.md") < at("done"));
        assert!(at("✓ 14s · 3.2k tok · $0.021") > at("done"));
        assert!(at("✓ 14s · 3.2k tok · $0.021") < at("again"));
        // A mark newer than every message still renders — at the tail.
        assert_eq!(at("killed bg_7"), rows.len() - 1);
        // A copy yields the line itself, not the indent it hangs from.
        let killed = lines
            .iter()
            .find(|l| strip_ansi(&l.text).trim() == "killed bg_7")
            .unwrap();
        assert_eq!(killed.copy.as_deref(), Some("killed bg_7"));
    }

    fn job(id: &str, over: Value) -> JobView {
        let mut base = json!({
            "id": id, "name": "test run", "sessionId": "s1", "pid": 10,
            "command": "deno test", "status": "running", "startedAt": 35_000,
        });
        let over_obj = over.as_object().unwrap().clone();
        let tail: Vec<String> = over_obj
            .get("tail")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_default();
        let output_lines = over_obj
            .get("outputLines")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        for (k, v) in &over_obj {
            if k != "tail" && k != "outputLines" {
                base[k] = v.clone();
            }
        }
        JobView {
            job: serde_json::from_value(base).unwrap(),
            tail,
            output_lines,
        }
    }

    #[test]
    fn job_cards_a_running_shell_looks_alive_an_exited_one_states_its_outcome() {
        let now = 100_000;
        let running = job(
            "bg_1",
            json!({"startedAt": now - 65_000, "tail": ["running tests"], "outputLines": 12}),
        );
        let failed = job(
            "bg_2",
            json!({"startedAt": now - 65_000, "status": "exited", "exitCode": 1, "exitedAt": now, "tail": ["FAILED"], "outputLines": 1}),
        );
        let mut out: Vec<VLine> = Vec::new();
        job_card_lines(&mut out, &running, 80, now);
        job_card_lines(&mut out, &failed, 80, now);
        let text = joined(&out);
        assert!(
            text.contains("⋯ running") && text.contains("1m 5s"),
            "{text}"
        );
        assert!(text.contains("running tests"));
        assert!(text.contains("12 lines total"));
        assert!(text.contains("✗ exit 1")); // the outcome survives the exit
                                            // Every card row is a door INTO that job.
        assert!(out.iter().all(|l| {
            l.click
                .as_deref()
                .is_some_and(|c| c.starts_with("job:s1:bg_"))
                || l.text.trim().is_empty()
        }));

        // A JOB THE USER KILLED IS NOT A JOB THAT SUCCEEDED.
        let killed = job(
            "bg_3",
            json!({"startedAt": now - 65_000, "status": "exited", "exitCode": null, "signal": "SIGTERM", "exitedAt": now}),
        );
        let mut o: Vec<VLine> = Vec::new();
        job_card_lines(&mut o, &killed, 80, now);
        let killed_text = joined(&o);
        assert!(killed_text.contains("◼ stopped (SIGTERM)"), "{killed_text}");
        assert!(!killed_text.contains("✓ done"), "{killed_text}");
    }

    #[test]
    fn a_job_whose_name_is_its_command_shows_the_command_once() {
        let now = 1_700_000_000_000i64;
        let j = job(
            "bg_2",
            json!({"name": "ls -1 src", "command": "ls -1 src", "status": "exited",
                   "exitCode": 0, "exitedAt": now, "startedAt": now - 400,
                   "tail": ["cart.py"], "outputLines": 1}),
        );
        let mut out: Vec<VLine> = Vec::new();
        job_card_lines(&mut out, &j, 80, now);
        let text = joined(&out);
        assert_eq!(text.matches("ls -1 src").count(), 1, "{text}");
        // The id still shows: it is what the rail addresses the job by.
        assert!(text.contains("bg_2"), "{text}");

        // A named job with a DIFFERENT command still shows both.
        let named = job(
            "bg_2",
            json!({"name": "dev server", "command": "npm run dev", "status": "exited",
                   "exitCode": 0, "exitedAt": now, "startedAt": now - 400}),
        );
        let mut out2: Vec<VLine> = Vec::new();
        job_card_lines(&mut out2, &named, 80, now);
        let text2 = joined(&out2);
        assert!(
            text2.contains("dev server") && text2.contains("npm run dev"),
            "{text2}"
        );
    }

    #[test]
    fn a_background_wake_note_is_dropped_while_its_job_card_shows_it() {
        let note = "[background] bg_1 finished (exit 0) — command \"make\", 2 lines of output.";
        let thread = vec![msg(
            "n1",
            json!({"role": "system", "parts": [{"type": "text", "text": note}]}),
        )];
        let j = job(
            "bg_1",
            json!({"name": "make", "command": "make", "status": "exited", "exitCode": 0,
                   "startedAt": 1, "exitedAt": 2}),
        );
        let opts = BuildOptions {
            jobs: vec![j],
            now: Some(3),
            ..Default::default()
        };
        let with_card = joined(&build(&thread, 80, &opts));
        assert!(!with_card.contains("[background]"));
        assert!(with_card.contains("bg_1"));
        // Once the job ages out of the registry the note is all that is left.
        assert!(joined(&build(&thread, 80, &BuildOptions::default())).contains("[background]"));
    }

    // ---- the viewport window ----

    #[test]
    fn visible_slice_pinned_to_the_tail_scrolled_up_and_clamped_past_the_top() {
        let lines: Vec<VLine> = (0..100).map(|i| vl(format!("l{i}"))).collect();
        let tail = visible_slice(&lines, 10, 0);
        assert_eq!(tail.rows[0].text, "l90");
        assert_eq!(tail.rows.last().unwrap().text, "l99");
        assert_eq!(tail.more, 0);

        let up = visible_slice(&lines, 10, 20);
        assert_eq!(up.start, 70);
        assert_eq!(up.more, 20);

        // Fully scrolled up reads 0%: the percentage is the viewport TOP's position.
        let top = visible_slice(&lines, 10, 999);
        assert_eq!(top.start, 0);
        assert_eq!(top.more, 90);
        assert_eq!(top.pct, 0);

        // A transcript shorter than the viewport shows everything, nothing below.
        let short = visible_slice(&lines[..3], 10, 5);
        assert_eq!(short.rows.len(), 3);
        assert_eq!(short.more, 0);
        assert_eq!(short.start, 0);
    }

    #[test]
    fn chat_body_height_subtracts_the_reserved_strips() {
        assert_eq!(chat_body_height(20, 0, false), 18);
        assert_eq!(chat_body_height(20, 3, false), 15);
        assert_eq!(chat_body_height(20, 0, true), 17);
        assert_eq!(chat_body_height(20, 2, true), 15);
        // Never zero or negative, however little room there is.
        assert_eq!(chat_body_height(1, 9, true), 1);
    }

    #[test]
    fn line_at_slot_inverts_the_pad_a_short_transcript_hangs_from_the_bottom() {
        let lines = vec![
            VLine {
                text: "a".into(),
                click: Some("ka".into()),
                ..Default::default()
            },
            VLine {
                text: "b".into(),
                click: Some("kb".into()),
                ..Default::default()
            },
        ];
        // Body of 5, two lines: three rows of empty air ABOVE, then the lines.
        assert!(line_at_slot(&lines, 5, 0, 0).is_none());
        assert!(line_at_slot(&lines, 5, 0, 2).is_none());
        assert_eq!(
            line_at_slot(&lines, 5, 0, 3).unwrap().click.as_deref(),
            Some("ka")
        );
        assert_eq!(
            line_at_slot(&lines, 5, 0, 4).unwrap().click.as_deref(),
            Some("kb")
        );
        // Off the bottom of the body.
        assert!(line_at_slot(&lines, 5, 0, 5).is_none());
    }

    #[test]
    fn line_at_slot_follows_the_scroll_offset() {
        let lines: Vec<VLine> = (0..10)
            .map(|i| VLine {
                text: format!("l{i}"),
                click: Some(format!("k{i}")),
                ..Default::default()
            })
            .collect();
        // Pinned to the tail: a body of 3 shows the last three, no pad.
        assert_eq!(
            line_at_slot(&lines, 3, 0, 0).unwrap().click.as_deref(),
            Some("k7")
        );
        assert_eq!(
            line_at_slot(&lines, 3, 0, 2).unwrap().click.as_deref(),
            Some("k9")
        );
        // Scrolled back two: the same slots resolve two lines earlier.
        assert_eq!(
            line_at_slot(&lines, 3, 2, 0).unwrap().click.as_deref(),
            Some("k5")
        );
        assert_eq!(
            line_at_slot(&lines, 3, 2, 2).unwrap().click.as_deref(),
            Some("k7")
        );
    }

    #[test]
    fn a_subagent_card_is_clickable_and_descends_rather_than_folds() {
        let b = Branch {
            id: "sub_9".into(),
            title: "explore the parser".into(),
            status: Some(BranchStatus::Done),
            ..Default::default()
        };
        let opts = BuildOptions {
            branches: vec![b],
            ..Default::default()
        };
        let out = build(&[], 80, &opts);
        // The target exists AND it is the descend form — the `open:` prefix is
        // the contract.
        assert!(out.iter().any(|l| l.click.as_deref() == Some("open:sub_9")));
    }

    #[test]
    fn a_message_that_names_an_installed_skill_says_so_under_the_message() {
        let installed: Vec<String> = ["prewalk", "exa", "shell-use"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            skills_named("/prewalk fix the parser", &installed),
            vec!["prewalk"]
        );
        // Mid-sentence counts.
        assert_eq!(
            skills_named("fix this, use /exa to check", &installed),
            vec!["exa"]
        );
        assert_eq!(
            skills_named("/exa and /shell-use please", &installed),
            vec!["exa", "shell-use"]
        );
        // Names not installed claim nothing.
        assert!(skills_named("/model", &installed).is_empty());
        assert!(skills_named("/prewalkk typo", &installed).is_empty());
        // A path is not a skill reference.
        assert!(skills_named("look at src/exa/mod.ts", &installed).is_empty());
        assert!(skills_named("/prewalk", &[]).is_empty());
        // Repeats collapse.
        assert_eq!(
            skills_named("/exa then /exa again", &installed),
            vec!["exa"]
        );

        let thread = vec![msg(
            "u1",
            json!({"role": "user", "parts": [{"type": "text", "text": "/exa search"}]}),
        )];
        let opts = BuildOptions {
            skills: Some(installed),
            ..Default::default()
        };
        let text = joined(&build(&thread, 80, &opts));
        assert!(text.contains("↳ skill loaded: /exa"), "{text}");
        // With no skills fetched yet, no claim is made.
        let bare = joined(&build(&thread, 80, &BuildOptions::default()));
        assert!(!bare.contains("skill loaded"));
    }

    #[test]
    fn an_exited_job_card_lands_where_the_command_finished_not_at_the_tail() {
        let thread = vec![
            msg(
                "u1",
                json!({"role": "user", "parts": [{"type": "text", "text": "first"}], "createdAt": 10}),
            ),
            msg(
                "u2",
                json!({"role": "user", "parts": [{"type": "text", "text": "second"}], "createdAt": 40}),
            ),
        ];
        let early = job(
            "bg_old",
            json!({"name": "bg_old", "command": "make", "status": "exited", "exitCode": 1,
                   "startedAt": 15, "exitedAt": 20}),
        );
        let opts = BuildOptions {
            jobs: vec![early],
            now: Some(50),
            ..Default::default()
        };
        let rows: Vec<String> = build(&thread, 100, &opts)
            .iter()
            .map(|l| strip_ansi(&l.text))
            .collect();
        let card = rows.iter().position(|r| r.contains("bg_old")).unwrap();
        let second = rows.iter().position(|r| r.contains("second")).unwrap();
        assert!(
            card < second,
            "the failed job renders before the later message"
        );
    }

    // ---- branchesFrom ----

    #[test]
    fn branches_from_pairs_a_spawned_child_with_its_report_note() {
        let note_text = [
            "[subagent finished] \"create mango.py\" (agent-1) — finished.",
            "Changed files: src/mango.py.",
            "Report:",
            "Created src/mango.py with a mango() function.",
            "It worked in THIS session's checkout, so its edits are already here — read them.",
        ]
        .join("\n");
        let thread = vec![
            msg(
                "u1",
                json!({"role": "user", "parts": [{"type": "text", "text": "spawn one"}]}),
            ),
            msg(
                "n1",
                json!({"role": "system", "parts": [{"type": "text", "text": note_text}]}),
            ),
        ];
        let child = ChildRow {
            id: "agent-1".into(),
            title: "subagent · create mango.py".into(),
            kind: SessionKind::Subagent,
            busy: false,
            last_turn_status: Some(TurnStatus::Done),
            outcome_ok: Some(true),
            origin_message_id: Some("u1".into()),
            tokens: None,
            cost_usd: None,
        };
        let branches = branches_from(&thread, &[child]);
        let b = &branches[0];
        assert_eq!(b.id, "agent-1");
        assert!(!b.busy);
        assert_eq!(b.status, Some(BranchStatus::Done));
        assert_eq!(b.ok, Some(true));
        assert_eq!(b.origin_message_id.as_deref(), Some("u1"));
        assert_eq!(b.note.as_ref().unwrap().session_id, "agent-1");
        assert_eq!(b.note.as_ref().unwrap().files, vec!["src/mango.py"]);

        // AND the note it paired is dropped from the raw thread.
        let opts = BuildOptions {
            branches,
            ..Default::default()
        };
        let text = joined(&build(&thread, 100, &opts));
        assert!(text.contains("create mango.py"), "{text}");
        assert!(!text.contains("It worked in THIS session"), "{text}");
        assert!(!text.contains("[subagent finished]"), "{text}");
    }

    #[test]
    fn branches_from_reports_a_running_child_as_running_and_leaves_an_unpaired_note_raw() {
        let note_text = "[subagent finished] \"other\" (agent-9) — finished.\nChanged files: none.";
        let thread = vec![msg(
            "n1",
            json!({"role": "system", "parts": [{"type": "text", "text": note_text}]}),
        )];

        // `running` is not a settled status and must not be reported as one.
        let child = |kind: SessionKind, status: Option<TurnStatus>, busy: bool| ChildRow {
            id: "a".into(),
            title: "t".into(),
            kind,
            busy,
            last_turn_status: status,
            outcome_ok: None,
            origin_message_id: None,
            tokens: None,
            cost_usd: None,
        };
        let live = branches_from(
            &[],
            &[child(
                SessionKind::Subagent,
                Some(TurnStatus::Running),
                true,
            )],
        );
        assert_eq!(live[0].status, None);
        assert!(live[0].busy);
        assert_eq!(live[0].ok, None);

        // A note whose session is not among the children yields no branch.
        assert!(branches_from(&thread, &[]).is_empty());

        // DELEGATED KINDS ONLY.
        for kind in [
            SessionKind::Root,
            SessionKind::Fork,
            SessionKind::Compaction,
        ] {
            assert!(branches_from(&[], &[child(kind, Some(TurnStatus::Done), false)]).is_empty());
        }
        assert_eq!(
            branches_from(
                &[],
                &[child(
                    SessionKind::WorkflowAgent,
                    Some(TurnStatus::Done),
                    false
                )]
            )
            .len(),
            1
        );
        let text = joined(&build(&thread, 100, &BuildOptions::default()));
        assert!(text.contains("[subagent finished]"), "{text}");
    }

    #[test]
    fn several_steps_are_several_rows_one_step_stays_on_the_header() {
        let m = msg(
            "m1",
            json!({"parts": [
                call("c1", "alpha()"), result("c1", "1"),
                call("c2", "beta()"), result("c2", "2"),
            ]}),
        );
        let lines = mlines(&m, closed, closed, 100);
        let head = lines
            .iter()
            .position(|l| strip_ansi(&l.text).contains("2 steps"))
            .unwrap();
        // Two step rows follow, each carrying the fold's click key.
        assert!(strip_ansi(&lines[head + 1].text).contains("alpha()"));
        assert!(strip_ansi(&lines[head + 2].text).contains("beta()"));
        assert_eq!(lines[head + 1].click, lines[head].click);

        // One step rides the header.
        let single = msg(
            "m2",
            json!({"parts": [call("c1", "alpha()"), result("c1", "1")]}),
        );
        let lines2 = mlines(&single, closed, closed, 100);
        let head2 = lines2
            .iter()
            .find(|l| strip_ansi(&l.text).contains("1 step"))
            .unwrap();
        assert!(strip_ansi(&head2.text).contains("alpha()"));
    }

    #[test]
    fn a_collapsed_steps_repeated_summaries_collapse_to_a_count() {
        let m = msg(
            "m1",
            json!({"parts": [
                call("c1", "run()"), result("c1", "1"),
                call("c2", "run()"), result("c2", "2"),
                call("c3", "run()"), result("c3", "3"),
            ]}),
        );
        let text = joined(&mlines(&m, closed, closed, 120));
        assert!(text.contains("run() ×3"), "{text}");
        assert!(!text.contains("run() · run()"), "{text}");
    }

    #[test]
    fn a_host_function_called_as_a_tool_is_named_not_dumped_as_json() {
        let m = msg(
            "m1",
            json!({"parts": [
                {"type": "tool_call", "id": "c1", "name": "patch", "input": {"input": "raw json here"}},
                result_over("c1", "patch is not a tool", json!({"isError": true})),
            ]}),
        );
        let text = joined(&mlines(&m, closed, closed, 120));
        assert!(text.contains("called patch as a tool"), "{text}");
        assert!(!text.contains("raw json here"), "{text}");
    }

    // ---- the memory margin (`#` rows) ----

    #[test]
    fn primed_tags_render_once_dim_as_the_transcripts_first_row() {
        let thread = vec![msg(
            "m1",
            json!({"role": "user", "parts": [{"type": "text", "text": "hi"}]}),
        )];
        let opts = BuildOptions {
            primed_tags: vec!["git:push".into(), "bun:test".into(), "psql:migrate".into()],
            ..Default::default()
        };
        let lines = build_lines(&thread, &open, &open, 100, &opts);
        assert_eq!(
            strip_ansi(&lines[0].text),
            "# this repo remembers: git:push · bun:test · psql:migrate"
        );
        assert_eq!(
            lines[0].copy.as_deref().map(strip_ansi),
            Some(strip_ansi(&lines[0].text))
        );
        // No primed tags — no row, not an empty row.
        let bare = build_lines(&thread, &open, &open, 100, &BuildOptions::default());
        assert!(!strip_ansi(&bare[0].text).starts_with('#'));
    }

    #[test]
    fn a_primed_row_longer_than_the_terminal_truncates_with_an_ellipsis() {
        let opts = BuildOptions {
            primed_tags: vec![
                "git:push".into(),
                "bun:test".into(),
                "psql:migrate".into(),
                "docker:exec".into(),
            ],
            ..Default::default()
        };
        let lines = build_lines(&[], &open, &open, 40, &opts);
        let text = strip_ansi(&lines[0].text);
        assert!(text.ends_with('…'), "{text}");
        assert!(width(&lines[0].text) <= 40, "{text}");
    }

    #[test]
    fn the_injected_agents_md_files_render_as_their_own_row_under_the_tags_one() {
        let thread = vec![msg(
            "m1",
            json!({"role": "user", "parts": [{"type": "text", "text": "hi"}]}),
        )];
        let opts = BuildOptions {
            primed_tags: vec!["git:push".into()],
            project_rules: vec!["AGENTS.md".into(), "packages/api/AGENTS.md".into()],
            ..Default::default()
        };
        let lines = build_lines(&thread, &open, &open, 100, &opts);
        assert_eq!(
            strip_ansi(&lines[0].text),
            "# this repo remembers: git:push"
        );
        assert_eq!(
            strip_ansi(&lines[1].text),
            "# rules: AGENTS.md · packages/api/AGENTS.md · /rules"
        );
        // No AGENTS.md anywhere — no row.
        let bare_opts = BuildOptions {
            primed_tags: vec!["git:push".into()],
            ..Default::default()
        };
        let bare = build_lines(&thread, &open, &open, 100, &bare_opts);
        assert!(!strip_ansi(&bare[1].text).starts_with('#'));
    }

    #[test]
    fn a_rules_row_that_fills_the_terminal_drops_its_hint_rather_than_wrapping() {
        let opts = BuildOptions {
            project_rules: vec![
                "AGENTS.md".into(),
                "packages/api/AGENTS.md".into(),
                "packages/web/AGENTS.md".into(),
            ],
            ..Default::default()
        };
        let lines = build_lines(&[], &open, &open, 40, &opts);
        let text = strip_ansi(&lines[0].text);
        assert!(width(&lines[0].text) <= 40, "{text}");
        // A row that has already been elided has no room for an advertisement.
        assert!(!text.contains("/rules"), "{text}");
    }

    #[test]
    fn history_hints_leave_the_output_block_and_become_marginalia() {
        let out = "ok\n[history] tags previously used in migrations/: psql, alembic — see history.sql() for the commands behind them";
        let m = msg(
            "m1",
            json!({"parts": [call("c1", "await bash('x', 'y')"), result("c1", out)]}),
        );
        let opts = BuildOptions::default();
        let text = joined(&build_lines(&[m], &open, &open, 100, &opts));
        // The hint line is rewritten and outside the │ block…
        assert!(
            text.contains("  # migrations/ also remembers: psql · alembic"),
            "{text}"
        );
        // …and the model-facing raw line is nowhere on screen.
        assert!(!text.contains("[history]"), "{text}");
        assert!(!text.contains("history.sql()"), "{text}");
        // The block keeps the program's real output.
        assert!(text.contains("│ ok"), "{text}");
    }

    #[test]
    fn a_result_that_is_only_hints_renders_no_output_block_at_all() {
        let out = "[history] tags previously used in src/tui/: opentui — see history.sql()";
        let m = msg(
            "m1",
            json!({"parts": [call("c1", "await view('a')"), result("c1", out)]}),
        );
        let text = joined(&build_lines(
            &[m],
            &open,
            &open,
            100,
            &BuildOptions::default(),
        ));
        assert!(!text.contains("↳ output"), "{text}");
        assert!(
            text.contains("# src/tui/ also remembers: opentui"),
            "{text}"
        );
    }

    #[test]
    fn split_margin_notes_takes_trailing_notes_in_any_order() {
        let (body, hints) = split_margin_notes(
            "real output\n[rules] AGENTS.md changed — re-read it\n[history] tags previously used in a/: x, y — see",
        );
        assert_eq!(body, "real output");
        assert_eq!(
            hints,
            vec!["rules: AGENTS.md changed", "a/ also remembers: x · y"]
        );
        let (body2, hints2) = split_margin_notes("just output");
        assert_eq!(body2, "just output");
        assert!(hints2.is_empty());
    }

    #[test]
    fn a_step_is_headlined_by_what_the_program_did_not_by_its_first_line_of_code() {
        // Ported from format.test.ts. The old header was a clipped source line
        // — debug output, not a UI:
        //   ▸ 1 step  run_steps · const out = await bash(`node --input-type=module -e "
        let sum = |code: &str| program_summary(code, 64, false);

        assert_eq!(
            sum(r#"const out = await bash("node -e 1"); console.log(out);"#),
            "ran 1 command"
        );
        assert_eq!(
            sum(r#"console.log(await view("/tmp/x/app.mjs"));"#),
            "read app.mjs"
        );
        // `Bun.file` is where `files.md` sends the model for raw content, so a
        // program using it was following instructions and still got a source
        // line for a header.
        assert_eq!(
            sum(r#"const t = await Bun.file("/w/src/order.py").text();"#),
            "read order.py"
        );
        // And `node:fs`, which models reach for whatever the prompt recommends.
        assert_eq!(
            sum(r#"const fs = require("fs"); const t = fs.readFileSync("/w/src/a.py", "utf8");"#),
            "read a.py"
        );
        assert_eq!(
            sum("const { readFile } = await import(\"node:fs/promises\");\nconst t = await readFile(\"/w/src/b.py\", \"utf8\");"),
            "read b.py"
        );
        assert_eq!(
            sum(r#"await write("src/a.ts", body); await bash("deno test");"#),
            "wrote a.ts · ran 1 command"
        );
        // A write through `node:fs` is still a write.
        assert_eq!(
            sum("const { writeFile } = await import(\"node:fs/promises\");\nawait writeFile(\"/w/lib/slugify.py\", body);"),
            "wrote slugify.py"
        );
        // Several files collapse rather than running off the row.
        assert_eq!(
            sum(r#"await edit("a.ts", x); await edit("b.ts", y); await edit("c.ts", z);"#),
            "wrote a.ts +2 more"
        );
        assert_eq!(
            sum(r#"await Promise.all([agent("one"), agent("two")]);"#),
            "2 subagents"
        );
        // Delegation has four verbs. A fan-out written with `spawn()` matched
        // nothing, so the header fell back to the gist and read `const tasks = [`.
        assert_eq!(
            sum("const r = await Promise.allSettled(tasks.map((t) => spawn(t.prompt)));"),
            "1 subagent"
        );
        assert_eq!(
            sum("const reports = await Promise.all(ids.map((id) => join(id)));"),
            "collected subagent reports"
        );
        // `node:path` exports a bare `join` too. The destructure is the signal —
        // the host verb is shadowed, so it cannot be the one being called.
        assert_eq!(
            sum("const { join } = await import(\"node:path\");\nconst p = join(dir, \"a.py\"); await bash(`ls ${p}`);"),
            "ran 1 command"
        );
        // The STATIC import too — the spelling the model actually used, and the
        // reason the first version of this fix did nothing on screen.
        assert_eq!(
            sum("import { readFileSync } from \"node:fs\";\nimport { join } from \"node:path\";\nconst p = join(ws, \"src/util.py\"); const t = readFileSync(p, \"utf8\");"),
            "read 1 file"
        );
        assert_eq!(sum(r#"await workflow("review", args);"#), "ran a workflow");
        assert_eq!(
            sum(r#"await ask("which one?", ["a", "b"]);"#),
            "asked you a question"
        );
        // A path built from a template is not a name. Seen on a fresh walk:
        // `▸ 1 step · wrote ${cartPath}` — the placeholder printed at the reader.
        assert_eq!(sum("await write(`${cartPath}`, body);"), "wrote 1 file");
        assert_eq!(
            sum("const t = await Bun.file(`${ws}/src/a.py`).text();"),
            "read 1 file"
        );
        // A PATCH whose section tag is built from a variable: named nothing,
        // counted nothing, and the header fell back to a line of source.
        assert_eq!(
            sum("const path = f();\nawait patch(`[${path}#3AF0]\nSWAP 1.=1:\n+x`);"),
            "wrote 1 file"
        );
        // A named tag still NAMES the file rather than counting it.
        assert_eq!(
            sum("await patch(`[src/a.py#3AF0]\nSWAP 1.=1:\n+x`);"),
            "wrote a.py"
        );
        // A real name alongside an interpolated one still names the one it can.
        assert_eq!(
            sum("await write(\"src/a.py\", x); await write(`${p}`, y);"),
            "wrote a.py"
        );
        assert_eq!(
            sum(r#"await bashBg("npm run dev");"#),
            "started 1 background command"
        );
        assert_eq!(
            sum(r#"const out = await bashOutput("bg_1"); console.log(out);"#),
            "read a background command's output"
        );
        assert_eq!(
            sum(r#"await bashWait("bg_1");"#),
            "waited for a background command"
        );
        assert_eq!(
            sum(r#"await bashKill("bg_1");"#),
            "killed a background command"
        );
        // A program that reached for `node:child_process` instead of `bash()`.
        assert_eq!(
            sum(r#"const { execSync } = require("node:child_process"); execSync("ls");"#),
            "ran 1 command"
        );
        assert_eq!(sum("await Bun.$`ls -1`;"), "ran 1 command");
        // But NOT the delegation verbs that happen to share those names.
        assert_eq!(
            sum(r#"const r = await spawn("do the thing");"#),
            "1 subagent"
        );
        // MEMBER CALLS ARE NOT HOST CALLS.
        assert_eq!(
            sum("const body = names.join(\"\\n\"); await artifact(\"summary\", body);"),
            "published an artifact"
        );
        assert_eq!(
            sum(r#"const i = text.search(/x/); await bash("ls");"#),
            "ran 1 command"
        );
        assert_eq!(
            sum(r#"stream.write("x"); await bash("ls");"#),
            "ran 1 command"
        );
        assert_eq!(
            sum(r#"await res.body.view("x"); await bash("ls");"#),
            "ran 1 command"
        );
        // And the real calls still count when they follow a dot-free boundary.
        assert_eq!(sum("await join(id);"), "collected subagent reports");
        // The rest of the host surface.
        assert_eq!(
            sum(r#"await state.set({key: "campaign", value: "x"});"#),
            "wrote session state"
        );
        assert_eq!(
            sum(r#"const v = await state.get({key: "campaign"});"#),
            "read session state"
        );
        assert_eq!(
            sum(r#"await schedule.add({cron: "0 9 * * *", prompt: "x"});"#),
            "changed a schedule"
        );
        assert_eq!(
            sum(r#"const r = await fetch("https://example.com");"#),
            "fetched 1 URL"
        );
        assert_eq!(
            sum(r#"await image("/tmp/a.png", "the chart");"#),
            "attached an image"
        );
        // Command text, not a call shape — MCP is reached through the shell now.
        assert_eq!(
            sum("await bash(`bough mcp call github list_repos '{}'`);"),
            "ran 1 command · 1 MCP call"
        );
        assert_eq!(
            sum(r#"await bash("bough mcp doctor");"#),
            "ran 1 command · checked the MCP servers"
        );
        // Both, and in that order: it IS a command, and the structural search is
        // the part worth naming. A single label would drop one of two true facts.
        assert_eq!(
            sum(r#"await bash("ast-grep -p 'send($$$)' -l ts src/");"#),
            "ran 1 command · searched by structure"
        );
        // Member calls still do not count — `res.fetch(…)` is not the host verb.
        assert_eq!(
            sum(r#"await client.fetch(u); await bash("ls");"#),
            "ran 1 command"
        );
        // patch() takes ONE string — naming it like a path-first call captured
        // the whole template literal, so the most-read line in the UI read
        // `wrote cart.js#8902] SWAP 3.=3: + for (let i = 0; …`.
        assert_eq!(
            sum("await patch(`[src/cart.js#8902]\nSWAP 3.=3:\n+  const x = 1;`);"),
            "wrote cart.js"
        );
        assert_eq!(
            sum("await patch(`[a.ts#]\nDEL 2.=3\n[b/c.ts#F1]\nINS.TAIL:\n+x`);"),
            "wrote a.ts, c.ts"
        );
        // A bracket that is not a patch section tag must not invent a file.
        assert_eq!(
            sum(r#"const rows = data["k#1"]; await bash("ls");"#),
            "ran 1 command"
        );
        // Unrecognized programs yield "", so the caller falls back to the code
        // gist rather than to an empty header.
        assert_eq!(sum("const x = 1 + 1;"), "");
        assert_eq!(sum(""), "");
        // Always bounded — this shares a row with the step count and the chips.
        assert!(sum(&r#"await bash("x");"#.repeat(40)).chars().count() <= 64);
    }

    #[test]
    fn a_running_program_is_named_in_the_present_tense() {
        // "ran 1 command" under a shell that has been blocked for ten seconds is
        // a statement the reader acts on and should not.
        let running = |code: &str| program_summary(code, 64, true);
        assert_eq!(running(r#"await bash("cargo test");"#), "running 1 command");
        assert_eq!(running(r#"await write("a.ts", x);"#), "writing a.ts");
        assert_eq!(running(r#"await view("a.ts");"#), "reading a.ts");
        assert_eq!(
            running(r#"await workflow("review");"#),
            "running a workflow"
        );
    }
}
