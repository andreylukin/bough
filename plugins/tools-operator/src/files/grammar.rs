//! Invariant: line numbers are in the coordinates of the version the model VIEWED. Earlier
//! operations do not shift later numbers, and a file this session never saw is refused rather
//! than patched blind. A port of `git show main:crates/bough-core/src/hostfn/patch.rs`, with
//! `BoughError` retargeted onto the local [`PatchError`].
//!
//! The module is pure: strings and vectors in, strings and vectors out. Resolving a TAG to the
//! text it names is [`super::apply`]'s job.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

/// The six operations of main's patch grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpKind {
    /// `SWAP A.=B:` — replaces lines A..B.
    Swap,
    /// `DEL A.=B` — removes them.
    Del,
    /// `INS.PRE A:` — before line A.
    InsPre,
    /// `INS.POST A:` — after line A.
    InsPost,
    /// `INS.HEAD:` — at the file's start.
    InsHead,
    /// `INS.TAIL:` — at its end.
    InsTail,
}

/// One operation against one file, in VIEWED coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchOp {
    /// The path from the enclosing `[path#TAG]` header, verbatim.
    pub path: String,
    /// The hash anchor, uppercased, or empty for `[path#]` ("the version you just saw").
    pub tag: String,
    pub kind: OpKind,
    /// First line of the footprint (1-based). Absent for `InsHead`/`InsTail`.
    pub a: Option<usize>,
    /// Last line of the footprint (1-based); equals `a` for single-line ops.
    pub b: Option<usize>,
    /// `+`-prefixed NEW text only, prefix stripped. There are no `-` rows.
    pub body: Vec<String>,
    /// 1-based line of the patch input this op was written on, for error text.
    pub at: usize,
}

/// One file's operations, in the order they were written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOps {
    pub path: String,
    pub tag: String,
    pub ops: Vec<PatchOp>,
}

/// Why a patch was refused. Every message is aimed at the MODEL: what failed, and what to write
/// instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchError {
    /// A malformed patch, an out-of-plan op, or any other refusal whose text is the whole story.
    Grammar(String),
    /// A section naming a file this agent never viewed.
    Unseen { path: String },
    /// An explicit tag that no longer names the version on record.
    StaleTag {
        path: String,
        saw: String,
        now: String,
    },
    /// An anchor outside the viewed file.
    OutOfRange {
        path: String,
        line: usize,
        count: usize,
    },
    /// A range that was touched since the view.
    Conflict {
        path: String,
        from: usize,
        to: usize,
        detail: String,
    },
    /// A path that resolved outside `ctx.workspace`.
    Denied { path: String, detail: String },
    /// The filesystem said no.
    Io(String),
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchError::Grammar(m) => write!(f, "{m}"),
            PatchError::Unseen { path } => write!(
                f,
                "no viewed version of {path} is on record — call view(\"{path}\") before patching \
                 it, then write \"[{path}#]\" with an empty tag to mean the version you just viewed."
            ),
            PatchError::StaleTag { path, saw, now } => write!(
                f,
                "stale tag: \"[{path}#{saw}]\" names a version of {path} that is no longer on \
                 record — the file has moved on and is now #{now}. Re-view {path} and rewrite the \
                 operations against its line numbers, or write an empty tag \"[{path}#]\" to mean \
                 the version you just viewed."
            ),
            PatchError::OutOfRange { path, line, count } if *count == 0 => write!(
                f,
                "{path}: line {line} is out of range — {path} is empty. Use INS.HEAD: or INS.TAIL: \
                 to put the first lines in."
            ),
            PatchError::OutOfRange { path, line, count } => write!(
                f,
                "{path}: line {line} is out of range — {path} has {count} lines. Line numbers are \
                 in the coordinates of the version you viewed."
            ),
            PatchError::Conflict {
                path,
                from,
                to,
                detail,
            } => write!(
                f,
                "patch conflict in {path}: lines {from}.={to} — {detail}. Someone else changed \
                 {path} since the version you viewed. Nothing was written — a patch applies to all \
                 its files or none. Re-view {path} and rewrite the operations against the new line \
                 numbers."
            ),
            PatchError::Denied { path, detail } => write!(
                f,
                "cannot patch {path}: {detail}. Nothing was written; a patch applies to all its \
                 files or none."
            ),
            PatchError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for PatchError {}

/// Throw-shaped helper: every grammar refusal is a `PatchError::Grammar` whose message is aimed
/// at the model.
pub(crate) fn bad<T>(message: String) -> Result<T, PatchError> {
    Err(PatchError::Grammar(message))
}

// ---------------------------------------------------------------------------
// Tags and text normalization
// ---------------------------------------------------------------------------

/// CRLF and a leading BOM must not change a file's identity.
pub fn normalize(text: &str) -> String {
    let stripped = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    stripped.replace("\r\n", "\n")
}

/// A file's tag: the low 16 bits of FNV-1a over the NORMALIZED text, as four uppercase hex digits.
///
/// The hash runs over UTF-16 code units (main hashed TS `charCodeAt`) — iterating bytes or `char`s
/// would produce different tags. It never leaves this process, and a collision degrades to a
/// REJECTED patch, never a wrong one, because the rebase re-checks the actual lines.
pub fn tag_of(text: &str) -> String {
    let norm = normalize(text);
    let mut h: u32 = 0x811c_9dc5;
    for unit in norm.encode_utf16() {
        h ^= unit as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{:04X}", h & 0xffff)
}

/// Split normalized text into lines, dropping the trailing empty element.
pub fn to_lines(text: &str) -> Vec<String> {
    let norm = normalize(text);
    let mut lines: Vec<String> = norm.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Re-attach the original line-ending style and trailing newline.
///
/// A file emptied by a patch comes back as `""` rather than a lone newline.
pub fn join_lines(lines: &[String], original: &str) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let eol = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let trailing = original.is_empty() || normalize(original).ends_with('\n');
    let mut out = lines.join(eol);
    if trailing {
        out.push_str(eol);
    }
    out
}

/// `[path#TAG]` + `NNN:text` — the form `view` hands the model.
pub fn render_numbered(path: &str, text: &str) -> String {
    let lines = to_lines(text);
    let width = lines.len().to_string().len();
    let body = lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>width$}:{}", i + 1, l))
        .collect::<Vec<_>>()
        .join("\n");
    format!("[{}#{}]\n{}", path, tag_of(text), body)
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

// `[path#TAG]`, plus the tagless `[path#]` / `[path]`. The path is lazy so a four-hex tag, when
// present, wins the trailing segment rather than being swallowed into the path.
static SECTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(.+?)(?:#([0-9a-fA-F]{4})?)?\]$").unwrap());
// `12:const x = 1;` — the shape of view's own listing, recognised only so it can be named.
static NUMBERED_LINE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\d+:").unwrap());
// `SWAP 12.=14:` — also the `-` / `..` / bare-space range spellings weaker models reach for.
static SWAP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^SWAP\s+(\d+)(?:\s*(?:\.=|\.\.|-|\s)\s*(\d+))?\s*:?\s*$").unwrap()
});
static DEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^DEL\s+(\d+)(?:\s*(?:\.=|\.\.|-|\s)\s*(\d+))?\s*$").unwrap());
static INS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^INS\.(PRE|POST)\s+(\d+)\s*:?\s*$").unwrap());
static INS_END_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^INS\.(HEAD|TAIL)\s*:?\s*$").unwrap());
// Codex-style envelopes are common muscle memory; swallow them silently.
static ENVELOPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\*\*\* (Begin|End) Patch\s*$").unwrap());

/// Keep quoted input short enough not to flood the model's next round.
fn trunc(s: &str) -> String {
    let mut it = s.chars();
    let prefix: String = it.by_ref().take(48).collect();
    if it.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// Parse one or more file sections into a flat op list.
///
/// Errors with a corrective message on anything malformed — a rejected patch the model can fix
/// beats a partially-understood one it cannot see.
pub fn parse_patch(input: &str) -> Result<Vec<PatchOp>, PatchError> {
    struct Seen {
        path: String,
        tag: String,
        count: usize,
    }

    let normalized = normalize(input);
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut ops: Vec<PatchOp> = Vec::new();
    let mut seen: Vec<Seen> = Vec::new();
    let mut cur: Option<usize> = None;
    // The op currently accepting `+` body rows, if any.
    let mut open: Option<usize> = None;
    // Set when the previous op was a DEL, so a stray body row says why.
    let mut last_was_del = false;

    for (i, line) in lines.iter().enumerate() {
        let at = i + 1;
        if ENVELOPE_RE.is_match(line) {
            continue;
        }

        if let Some(rest) = line.strip_prefix('+') {
            match open {
                None => {
                    if last_was_del {
                        return bad(format!(
                            "line {at}: DEL takes no body rows — it only removes lines. To \
                             replace lines, use SWAP A.=B: with the new text below it."
                        ));
                    }
                    return bad(format!(
                        "line {at}: body row \"{}\" has no operation above it. A \"+\" row must \
                         follow SWAP, INS.PRE, INS.POST, INS.HEAD or INS.TAIL.",
                        trunc(line)
                    ));
                }
                Some(idx) => {
                    ops[idx].body.push(rest.to_string());
                    continue;
                }
            }
        }
        if line.trim().is_empty() {
            open = None; // a blank line ends a body; it never becomes content
            continue;
        }

        let trimmed = line.trim();
        if let Some(sec) = SECTION_RE.captures(trimmed) {
            let path = sec.get(1).map_or("", |m| m.as_str()).trim().to_string();
            if path.is_empty() {
                return bad(format!(
                    "line {at}: section header \"{trimmed}\" has no path"
                ));
            }
            let tag = sec
                .get(2)
                .map_or(String::new(), |m| m.as_str().to_uppercase());
            seen.push(Seen {
                path,
                tag,
                count: 0,
            });
            cur = Some(seen.len() - 1);
            open = None;
            last_was_del = false;
            continue;
        }
        let cur_idx = match cur {
            Some(c) => c,
            None => {
                return bad(format!(
                    "line {at}: expected a section header \"[path#TAG]\" before any operation — \
                     the TAG comes from view(path), and \"[path#]\" with an empty tag means the \
                     version you just viewed."
                ));
            }
        };
        if line.starts_with('-') {
            return bad(format!(
                "line {at}: \"-\" rows are not part of this format. Name the lines to remove with \
                 DEL, or replace them with SWAP; write literal text starting with \"-\" as a body \
                 row (\"+- like this\")."
            ));
        }
        if NUMBERED_LINE_RE.is_match(line) {
            return bad(format!(
                "line {at}: \"{}\" looks like a line from view's listing. Do not pass view's \
                 output to patch — the listing is for you to read. Write only the section header \
                 and your operations; the header may be just \"[{}#]\" to mean the version you \
                 viewed.",
                trunc(line),
                seen[cur_idx].path
            ));
        }

        let path = seen[cur_idx].path.clone();
        let tag = seen[cur_idx].tag.clone();
        let op = if let Some(m) = SWAP_RE.captures(trimmed) {
            let a = num(&m, 1);
            let b = m.get(2).map_or(a, |v| parse_num(v.as_str()));
            PatchOp {
                path,
                tag,
                kind: OpKind::Swap,
                a: Some(a),
                b: Some(b),
                body: vec![],
                at,
            }
        } else if let Some(m) = DEL_RE.captures(trimmed) {
            let a = num(&m, 1);
            let b = m.get(2).map_or(a, |v| parse_num(v.as_str()));
            PatchOp {
                path,
                tag,
                kind: OpKind::Del,
                a: Some(a),
                b: Some(b),
                body: vec![],
                at,
            }
        } else if let Some(m) = INS_RE.captures(trimmed) {
            let a = num(&m, 2);
            let kind = if m.get(1).is_some_and(|v| v.as_str() == "PRE") {
                OpKind::InsPre
            } else {
                OpKind::InsPost
            };
            PatchOp {
                path,
                tag,
                kind,
                a: Some(a),
                b: Some(a),
                body: vec![],
                at,
            }
        } else if let Some(m) = INS_END_RE.captures(trimmed) {
            let kind = if m.get(1).is_some_and(|v| v.as_str() == "HEAD") {
                OpKind::InsHead
            } else {
                OpKind::InsTail
            };
            PatchOp {
                path,
                tag,
                kind,
                a: None,
                b: None,
                body: vec![],
                at,
            }
        } else {
            return bad(format!(
                "line {at}: \"{}\" is not an operation. Use SWAP A.=B:, DEL A.=B, INS.PRE A:, \
                 INS.POST A:, INS.HEAD: or INS.TAIL:",
                trunc(line)
            ));
        };
        // DEL takes no body, so a stray `+` row after it is an error, not silent text.
        last_was_del = op.kind == OpKind::Del;
        ops.push(op);
        seen[cur_idx].count += 1;
        open = if last_was_del {
            None
        } else {
            Some(ops.len() - 1)
        };
    }

    if seen.is_empty() {
        return bad("empty patch — expected at least one \"[path#TAG]\" section".to_string());
    }
    for s in &seen {
        if s.count == 0 {
            return bad(format!("section [{}#{}] has no operations", s.path, s.tag));
        }
    }
    Ok(ops)
}

fn parse_num(s: &str) -> usize {
    s.parse().unwrap_or(usize::MAX)
}

fn num(m: &regex::Captures<'_>, i: usize) -> usize {
    parse_num(m.get(i).map_or("", |v| v.as_str()))
}

/// Regroup a flat op list by file, preserving first-appearance order.
///
/// Two sections naming one path are merged. They must agree on the tag: two different base
/// versions of one file in a single patch is a plan the engine cannot honour, and guessing which
/// one wins is exactly the silent-clobber shape.
pub fn group_by_file(ops: &[PatchOp]) -> Result<Vec<FileOps>, PatchError> {
    let mut order: Vec<String> = Vec::new();
    let mut by_path: HashMap<String, FileOps> = HashMap::new();
    for op in ops {
        match by_path.get_mut(&op.path) {
            None => {
                by_path.insert(
                    op.path.clone(),
                    FileOps {
                        path: op.path.clone(),
                        tag: op.tag.clone(),
                        ops: vec![op.clone()],
                    },
                );
                order.push(op.path.clone());
            }
            Some(g) => {
                if g.tag != op.tag {
                    return bad(format!(
                        "{} appears twice with different tags (\"[{}#{}]\" and \"[{}#{}]\"). One \
                         file has one base version per patch — use a single section, or an empty \
                         tag \"[{}#]\".",
                        op.path, op.path, g.tag, op.path, op.tag, op.path
                    ));
                }
                g.ops.push(op.clone());
            }
        }
    }
    Ok(order
        .into_iter()
        .map(|p| by_path.remove(&p).expect("path was inserted"))
        .collect())
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate one file's ops against a file of `count` lines, in the coordinates the ops are written
/// in. Rejects out-of-range anchors, inverted ranges, bodiless SWAPs, overlapping spans, and
/// inserts anchored inside a span another op replaces.
pub fn check_ops(path: &str, ops: &[PatchOp], count: usize) -> Result<(), PatchError> {
    for op in ops {
        if op.kind == OpKind::InsHead || op.kind == OpKind::InsTail {
            continue;
        }
        let a = op.a.expect("line-anchored op has a");
        let b = op.b.expect("line-anchored op has b");
        if a < 1 || a > count {
            return Err(PatchError::OutOfRange {
                path: path.to_string(),
                line: a,
                count,
            });
        }
        if b < a || b > count {
            return bad(format!(
                "{path}: range {a}.={b} is invalid for a file of {count} lines — the range runs \
                 from the first line to the last, inclusive."
            ));
        }
        if op.kind == OpKind::Swap && op.body.is_empty() {
            return bad(format!(
                "{path}: SWAP {a}.={b} has no body rows. Put the replacement text on \"+\" rows \
                 beneath it, or use DEL {a}.={b} to remove those lines."
            ));
        }
    }

    let mut spans: Vec<(usize, usize)> = ops
        .iter()
        .filter(|o| o.kind == OpKind::Swap || o.kind == OpKind::Del)
        .map(|o| (o.a.expect("span has a"), o.b.expect("span has b")))
        .collect();
    spans.sort_by_key(|s| s.0);
    for i in 1..spans.len() {
        if spans[i].0 <= spans[i - 1].1 {
            return bad(format!(
                "{path}: operations overlap — lines {}.={} and {}.={} both cover line {}. Cover \
                 each line with at most one operation.",
                spans[i - 1].0,
                spans[i - 1].1,
                spans[i].0,
                spans[i].1,
                spans[i].0
            ));
        }
    }

    // An INS anchored inside a SWAP span has no meaning; saying so beats emitting the model's new
    // lines interleaved at random.
    for op in ops {
        if op.kind != OpKind::InsPre && op.kind != OpKind::InsPost {
            continue;
        }
        let x = op.a.expect("ins op has a");
        for &(sa, sb) in &spans {
            let inside = if op.kind == OpKind::InsPre {
                x > sa && x <= sb
            } else {
                x >= sa && x < sb
            };
            if inside {
                let verb = if op.kind == OpKind::InsPre {
                    "INS.PRE"
                } else {
                    "INS.POST"
                };
                return bad(format!(
                    "{path}: {verb} {x} anchors inside lines {sa}.={sb}, which another operation \
                     in this patch replaces. Fold the inserted text into that operation's body \
                     instead."
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// Assemble the result in ONE pass over the original lines.
///
/// This is the mechanical guarantee that an earlier op cannot shift a later op's coordinates:
/// every anchor is read against `lines` and nothing is spliced into a partially-edited vector.
/// Assumes [`check_ops`] passed.
///
/// Ordering at a single gap is fixed: `INS.HEAD` bodies, then for each line its `INS.PRE` bodies,
/// then the line (or its SWAP body, or nothing for DEL), then its `INS.POST` bodies, and finally
/// `INS.TAIL`.
pub fn materialize(lines: &[String], ops: &[PatchOp]) -> Vec<String> {
    let n = lines.len();
    let mut pre: Vec<Vec<String>> = vec![Vec::new(); n];
    let mut post: Vec<Vec<String>> = vec![Vec::new(); n];
    let mut span_at: HashMap<usize, &PatchOp> = HashMap::new();
    let mut head: Vec<String> = Vec::new();
    let mut tail: Vec<String> = Vec::new();

    for op in ops {
        match op.kind {
            OpKind::InsHead => head.extend(op.body.iter().cloned()),
            OpKind::InsTail => tail.extend(op.body.iter().cloned()),
            OpKind::InsPre => pre[op.a.expect("ins_pre has a") - 1].extend(op.body.iter().cloned()),
            OpKind::InsPost => {
                post[op.a.expect("ins_post has a") - 1].extend(op.body.iter().cloned())
            }
            OpKind::Swap | OpKind::Del => {
                span_at.insert(op.a.expect("span has a") - 1, op);
            }
        }
    }

    let mut out: Vec<String> = head;
    let mut i = 0;
    while i < n {
        out.extend(pre[i].iter().cloned());
        if let Some(span) = span_at.get(&i) {
            if span.kind == OpKind::Swap {
                out.extend(span.body.iter().cloned());
            }
            let last = span.b.expect("span has b") - 1;
            out.extend(post[last].iter().cloned());
            i = last + 1;
        } else {
            out.push(lines[i].clone());
            out.extend(post[i].iter().cloned());
            i += 1;
        }
    }
    out.extend(tail);
    out
}
