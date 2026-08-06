//! The patch engine — hash-anchored line edits, computed as pure data.
//! Port of `src/hostfn/patch.ts` (spec: hostfn.md §patch, docs.md §patch).
//!
//! WHY THIS EXISTS. Subagents share their spawner's checkout. There are no
//! worktrees, no leases, and no merge step: two agents editing one file is
//! ordinary traffic. Line numbers alone are worthless under that regime — a
//! concurrent insert above shifts every anchor below it — so every patch is
//! bound to a TAG naming the exact version the agent read. That binding is what
//! makes a stale patch *detectable*, and knowing the base text is what makes
//! most stale patches *recoverable*.
//!
//! THE INVARIANT THIS HOLDS: a patch never silently lands on text its author
//! did not see. Three rules, in order of importance:
//!
//!   1. **Rebase or refuse, never guess.** If the file changed since the tag
//!      but none of the patched line ranges were touched, the anchors rebase
//!      onto the new version and both edits land. If a patched range *was*
//!      touched, the patch is refused with the file and the range named. A
//!      silent lost update is the one outcome this module must never produce.
//!   2. **Viewed coordinates.** Every line number is in the coordinates of the
//!      version the agent viewed. Edits are collected against the original and
//!      the result is assembled in ONE pass (`materialize`); nothing is ever
//!      applied sequentially, so an earlier op in the same patch cannot shift a
//!      later op's anchor.
//!   3. **All or none.** A multi-file patch that fails on its third file leaves
//!      the first two untouched. `apply_patch` builds a new map and errors
//!      before returning it, so failure is indistinguishable from never having
//!      been called.
//!
//! The module is pure: strings and vectors in, strings and vectors out. No IO,
//! no clock, no snapshot store. Resolving a TAG to the text it names is the
//! caller's job (`hostfn/files.rs`) — see the `base` argument of `apply_patch`.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::errors::BoughError;

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// The six operations. `Del` is the only one that carries no body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    Swap,
    Del,
    InsPre,
    InsPost,
    InsHead,
    InsTail,
}

/// One operation, already bound to the file section it was written under.
///
/// `parse_patch` returns a flat list rather than a tree so callers can filter
/// and count without walking; `group_by_file` reassembles the per-file view
/// when the caller needs the `(path, tag)` pair to resolve a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchOp {
    /// The path from the enclosing `[path#TAG]` header, verbatim.
    pub path: String,
    /// The four-hex version this op is written against, uppercased, or `""`
    /// for the tagless form (`[path#]` / `[path]`) meaning "whatever I just
    /// viewed". Tagless costs nothing in safety — the caller resolves it from
    /// the same snapshot a rebase would use.
    pub tag: String,
    pub kind: OpKind,
    /// First line of the footprint (1-based). Absent for `InsHead`/`InsTail`.
    pub a: Option<usize>,
    /// Last line of the footprint (1-based); equals `a` for single-line ops.
    pub b: Option<usize>,
    /// Body rows with their `+` prefix stripped. A lone `+` yields `""`.
    pub body: Vec<String>,
    /// 1-based line in the patch input this op was written on, for error text.
    pub at: usize,
}

/// One file's worth of operations, in the order they were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOps {
    pub path: String,
    pub tag: String,
    pub ops: Vec<PatchOp>,
}

// ---------------------------------------------------------------------------
// Tags and text normalization
// ---------------------------------------------------------------------------

/// CRLF and a leading BOM must not change a file's identity.
pub fn normalize(text: &str) -> String {
    let stripped = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    stripped.replace("\r\n", "\n")
}

/// A file's tag: the low 16 bits of FNV-1a over the NORMALIZED text, as four
/// uppercase hex digits.
///
/// The hash runs **over UTF-16 code units** (TS `charCodeAt`) — a port
/// iterating bytes or `char`s would produce different tags. It only ever
/// round-trips inside this process's own snapshot store — it is an identity
/// check, not a checksum anyone else verifies — so 16 bits is the right trade.
/// A collision degrades to a *rejected* patch, never a wrong one, because the
/// rebase re-checks the actual lines rather than trusting the tag.
pub fn tag_of(text: &str) -> String {
    let norm = normalize(text);
    let mut h: u32 = 0x811c9dc5;
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
/// NOTE: a file emptied by a patch comes back as `""` rather than a lone
/// newline — deleting every line should not leave a blank one behind.
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

/// `[path#TAG]` + `NNN:text` — the form `view()` hands the model.
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

// `[path#TAG]`, plus the tagless `[path#]` / `[path]`. The path is lazy so a
// four-hex tag, when present, wins the trailing segment rather than being
// swallowed into the path.
static SECTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(.+?)(?:#([0-9a-fA-F]{4})?)?\]$").unwrap());
// `12:const x = 1;` — the shape of view()'s own listing. Recognised only so it
// can be named: pasting view()'s whole output back into patch() is the most
// natural mistake this format invites.
static NUMBERED_LINE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\d+:").unwrap());
// `SWAP 12.=14:` — also accepts `SWAP 12:` and the `-` / `..` / bare-space
// range spellings, which weaker models reach for constantly.
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

/// Throw-shaped helper: every refusal in this module is a `PatchError` (400)
/// whose message is aimed at the model — say what failed and what to write.
fn bad<T>(message: String) -> Result<T, BoughError> {
    Err(BoughError::patch(message))
}

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
/// Errors with a corrective message on anything malformed — a rejected patch
/// the model can fix beats a partially-understood one it cannot see. Every
/// message names the input line, what was wrong, and what to write.
pub fn parse_patch(input: &str) -> Result<Vec<PatchOp>, BoughError> {
    struct Seen {
        path: String,
        tag: String,
        count: usize,
    }

    let normalized = normalize(input);
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut ops: Vec<PatchOp> = Vec::new();
    // Sections seen, so a header with no operations can be reported.
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
                        "line {at}: body row \"{}\" has no operation above it. A \
                         \"+\" row must follow SWAP, INS.PRE, INS.POST, INS.HEAD or INS.TAIL.",
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
                    "line {at}: expected a section header \"[path#TAG]\" before any \
                     operation — the TAG comes from view(path), and \"[path#]\" with an \
                     empty tag means the version you just viewed."
                ));
            }
        };
        if line.starts_with('-') {
            return bad(format!(
                "line {at}: \"-\" rows are not part of this format. Name the lines to \
                 remove with DEL, or replace them with SWAP; write literal text \
                 starting with \"-\" as a body row (\"+- like this\")."
            ));
        }
        if NUMBERED_LINE_RE.is_match(line) {
            return bad(format!(
                "line {at}: \"{}\" looks like a line from view()'s listing. \
                 Do not pass view()'s output to patch() — the listing is for you to \
                 read. Write only the section header and your operations; the header \
                 may be just \"[{}#]\" to mean the version you viewed.",
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
                "line {at}: \"{}\" is not an operation. Use SWAP A.=B:, \
                 DEL A.=B, INS.PRE A:, INS.POST A:, INS.HEAD: or INS.TAIL:",
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
/// Two sections naming one path are merged, which is how a model that writes
/// the header twice still gets one coherent edit. They must agree on the tag:
/// two different base versions of one file in a single patch is a plan the
/// engine cannot honour, and guessing which one wins is exactly the
/// silent-clobber shape.
pub fn group_by_file(ops: &[PatchOp]) -> Result<Vec<FileOps>, BoughError> {
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
                        "{} appears twice with different tags (\"[{}#{}]\" \
                         and \"[{}#{}]\"). One file has one base version per \
                         patch — use a single section, or an empty tag \"[{}#]\".",
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

/// Validate one file's ops against a file of `count` lines, in the coordinates
/// the ops are written in. Rejects out-of-range anchors, inverted ranges,
/// bodiless SWAPs, overlapping spans, and inserts anchored inside a span
/// another op replaces.
///
/// Two ops rewriting one line is always a mistake in the model's plan, and
/// letting assembly order pick a winner would hide it.
pub fn check_ops(path: &str, ops: &[PatchOp], count: usize) -> Result<(), BoughError> {
    for op in ops {
        if op.kind == OpKind::InsHead || op.kind == OpKind::InsTail {
            continue;
        }
        let a = op.a.expect("line-anchored op has a");
        let b = op.b.expect("line-anchored op has b");
        if a < 1 || a > count {
            return if count == 0 {
                bad(format!(
                    "{path}: line {a} is out of range — {path} is empty. Use \
                     INS.HEAD: or INS.TAIL: to put the first lines in."
                ))
            } else {
                bad(format!(
                    "{path}: line {a} is out of range — {path} has {count} lines. \
                     Line numbers are in the coordinates of the version you viewed."
                ))
            };
        }
        if b < a || b > count {
            return bad(format!(
                "{path}: range {a}.={b} is invalid for a file of {count} lines — \
                 the range runs from the first line to the last, inclusive."
            ));
        }
        if op.kind == OpKind::Swap && op.body.is_empty() {
            return bad(format!(
                "{path}: SWAP {a}.={b} has no body rows. Put the replacement text on \
                 \"+\" rows beneath it, or use DEL {a}.={b} to remove those lines."
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
                "{path}: operations overlap — lines {}.={} \
                 and {}.={} both cover line {}. Cover \
                 each line with at most one operation.",
                spans[i - 1].0,
                spans[i - 1].1,
                spans[i].0,
                spans[i].1,
                spans[i].0
            ));
        }
    }

    // NOTE (from the TS port): an INS anchored inside a SWAP span has no
    // meaning; saying so beats emitting the model's new lines interleaved at
    // random.
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
                    "{path}: {verb} {x} anchors inside lines {sa}.={sb}, which \
                     another operation in this patch replaces. Fold the inserted text \
                     into that operation's body instead."
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
/// This is the mechanical guarantee behind rule 2: every anchor is read
/// against `lines` and nothing is ever spliced into a partially-edited vector,
/// so an earlier op cannot shift a later op's coordinates. Assumes `check_ops`
/// passed.
///
/// Ordering at a single gap is fixed and documented rather than incidental:
/// `INS.HEAD` bodies, then for each line its `INS.PRE` bodies, then the line
/// (or its SWAP body, or nothing for DEL), then its `INS.POST` bodies, and
/// finally `INS.TAIL`. Multiple ops of the same kind at one anchor emit in
/// patch order, and `INS.POST N` precedes `INS.PRE N+1` in the gap they share.
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

// ---------------------------------------------------------------------------
// Rebase
// ---------------------------------------------------------------------------

/// Past this many lines of divergence the LCS is skipped and every line in the
/// diverged middle is reported as changed. Two agents editing one file almost
/// always diverge in a single small region, so the table normally runs over a
/// handful of lines; the cap keeps a pathological diff from becoming an
/// O(n·m) stall. Exceeding it costs a rejected patch, never a wrong one.
const LCS_CAP: usize = 400;

/// Map each base line index to its index in `cur`, or `None` where the line
/// was changed or deleted by whoever wrote the file in the meantime.
///
/// Common prefix and suffix are trimmed first — that is what makes this cheap
/// — and an LCS over the diverged middles supplies the rest. The result is
/// monotonically increasing, which is what lets `rebase_ops` conclude that
/// non-overlapping spans stay non-overlapping after rebasing.
pub fn line_map(base: &[String], cur: &[String]) -> Vec<Option<usize>> {
    let mut map: Vec<Option<usize>> = vec![None; base.len()];
    let mut p = 0;
    while p < base.len() && p < cur.len() && base[p] == cur[p] {
        map[p] = Some(p);
        p += 1;
    }
    let mut s = 0;
    while s < base.len() - p
        && s < cur.len() - p
        && base[base.len() - 1 - s] == cur[cur.len() - 1 - s]
    {
        map[base.len() - 1 - s] = Some(cur.len() - 1 - s);
        s += 1;
    }
    let bm = &base[p..base.len() - s];
    let cm = &cur[p..cur.len() - s];
    if bm.is_empty() || cm.is_empty() {
        return map;
    }
    if bm.len() > LCS_CAP || cm.len() > LCS_CAP {
        return map;
    }

    let (n, m) = (bm.len(), cm.len());
    let mut dp = vec![0usize; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[at(i, j)] = if bm[i] == cm[j] {
                dp[at(i + 1, j + 1)] + 1
            } else {
                dp[at(i + 1, j)].max(dp[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if bm[i] == cm[j] {
            map[p + i] = Some(p + j);
            i += 1;
            j += 1;
        } else if dp[at(i + 1, j)] >= dp[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    map
}

/// One op that could not be moved onto the current text, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseConflict {
    pub op: PatchOp,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseResult {
    Ok { ops: Vec<PatchOp> },
    Conflicts(Vec<RebaseConflict>),
}

/// Move ops from the coordinates of `base` (the text the agent read) onto
/// `cur` (the text as it stands now).
///
/// An op survives when every line it names is still present AND still
/// contiguous. Contiguity is not pedantry: if someone inserted *into* the
/// span, the op's footprint now covers lines the agent never saw, and
/// rewriting them would silently discard that insert. Anything else is a
/// genuine conflict, reported rather than guessed at.
pub fn rebase_ops(ops: &[PatchOp], base: &[String], cur: &[String]) -> RebaseResult {
    let map = line_map(base, cur);
    let mut out: Vec<PatchOp> = Vec::new();
    let mut conflicts: Vec<RebaseConflict> = Vec::new();
    for op in ops {
        if op.kind == OpKind::InsHead || op.kind == OpKind::InsTail {
            out.push(op.clone());
            continue;
        }
        let op_a = op.a.expect("line-anchored op has a");
        let op_b = op.b.expect("line-anchored op has b");
        let a = map.get(op_a - 1).copied().flatten();
        let b = map.get(op_b - 1).copied().flatten();
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                conflicts.push(RebaseConflict {
                    op: op.clone(),
                    reason: format!("lines {op_a}.={op_b} were rewritten"),
                });
                continue;
            }
        };
        if b - a != op_b - op_a {
            conflicts.push(RebaseConflict {
                op: op.clone(),
                reason: format!("lines {op_a}.={op_b} had lines inserted inside them"),
            });
            continue;
        }
        // EVERY line in the span, not just its endpoints. Checking only the
        // ends accepts an op whose interior was rewritten in place — the line
        // count is unchanged, so the contiguity guard above passes, and the op
        // then overwrites an edit the agent never saw. That is the silent lost
        // update this module exists to prevent, and it is the single most
        // common concurrent-edit shape (a renamed identifier, a changed
        // constant) inside a span another agent is replacing. Being stricter
        // here can only turn a silently-wrong apply into a reported conflict.
        let mut interior_moved = false;
        for k in (op_a - 1)..=(op_b - 1) {
            if map.get(k).copied().flatten() != Some(a + (k - (op_a - 1))) {
                interior_moved = true;
                break;
            }
        }
        if interior_moved {
            conflicts.push(RebaseConflict {
                op: op.clone(),
                reason: format!("lines {op_a}.={op_b} were rewritten"),
            });
            continue;
        }
        out.push(PatchOp {
            a: Some(a + 1),
            b: Some(b + 1),
            ..op.clone()
        });
    }
    if conflicts.is_empty() {
        RebaseResult::Ok { ops: out }
    } else {
        RebaseResult::Conflicts(conflicts)
    }
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

/// Apply a parsed patch to a set of files.
///
/// `files` maps path → the text as it stands NOW (what a writer would
/// clobber). `base` (TS `ApplyOptions.base`) maps path → the text the agent
/// VIEWED. Three behaviours, chosen deliberately:
///
///   - **`base` supplied, path present** — that text is the rebase base. If
///     the section carried an explicit tag it must match, or the patch is
///     refused as stale.
///   - **`base` supplied, path absent** — refused. The agent patched a file
///     this session never viewed, so there is no base to rebase from and
///     applying against the current text would be exactly the silent clobber
///     this module exists to prevent.
///   - **`base` omitted entirely** — no snapshot store is in play (unit
///     tests, or a caller that has just read the file itself), so the current
///     text is the base and no rebase is possible. An explicit tag is still
///     verified against the current text.
///
/// The return value is a NEW map: every entry of `files` carried over, with
/// patched paths replaced. Neither argument is mutated.
///
/// All or none: every file is validated, rebased and assembled before any
/// result is handed back, and any failure errors — so a caller that writes
/// only what this returns can never half-apply a patch.
pub fn apply_patch(
    files: &HashMap<String, String>,
    ops: &[PatchOp],
    base: Option<&HashMap<String, String>>,
) -> Result<HashMap<String, String>, BoughError> {
    let groups = group_by_file(ops)?;
    let mut result = files.clone();

    for g in &groups {
        let current = match files.get(&g.path) {
            Some(c) => c,
            None => {
                return bad(format!(
                    "{} is not in this patch's file set — the path in the section \
                     header must be the same path you viewed. Check for a typo or a \
                     wrong-directory prefix.",
                    g.path
                ));
            }
        };

        let base_text = resolve_base(g, current, base)?;
        let base_lines = to_lines(&base_text);

        // Bounds and overlap are judged in the coordinates the ops were
        // WRITTEN in, which is the viewed version — not whatever the file has
        // since become.
        check_ops(&g.path, &g.ops, base_lines.len())?;

        let current_lines = to_lines(current);
        let effective: Vec<PatchOp> = if normalize(&base_text) != normalize(current) {
            match rebase_ops(&g.ops, &base_lines, &current_lines) {
                RebaseResult::Conflicts(conflicts) => {
                    return bad(conflict_message(&g.path, &conflicts));
                }
                RebaseResult::Ok { ops } => ops,
            }
        } else {
            g.ops.clone()
        };

        result.insert(
            g.path.clone(),
            join_lines(&materialize(&current_lines, &effective), current),
        );
    }

    Ok(result)
}

/// Decide which text the ops' line numbers are written against. See
/// `apply_patch`.
fn resolve_base(
    g: &FileOps,
    current: &str,
    base: Option<&HashMap<String, String>>,
) -> Result<String, BoughError> {
    if let Some(base) = base {
        let snapshot = match base.get(&g.path) {
            Some(s) => s,
            None => {
                return bad(format!(
                    "no viewed version of {} is on record — call view(\"{}\") \
                     before patching it, then write \"[{}#]\" with an empty tag to \
                     mean the version you just viewed.",
                    g.path, g.path, g.path
                ));
            }
        };
        if !g.tag.is_empty() && tag_of(snapshot) != g.tag {
            return bad(stale_tag_message(&g.path, &g.tag, current));
        }
        return Ok(snapshot.clone());
    }
    // No snapshot store in play: the current text is the only version there is.
    if !g.tag.is_empty() && tag_of(current) != g.tag {
        return bad(stale_tag_message(&g.path, &g.tag, current));
    }
    Ok(current.to_string())
}

fn stale_tag_message(path: &str, tag: &str, current: &str) -> String {
    format!(
        "stale tag: \"[{path}#{tag}]\" names a version of {path} that is no longer \
         on record — the file has moved on and is now #{}. Re-view \
         {path} and rewrite the operations against its line numbers, or write an \
         empty tag \"[{path}#]\" to mean the version you just viewed.",
        tag_of(current)
    )
}

fn conflict_message(path: &str, conflicts: &[RebaseConflict]) -> String {
    let reasons = conflicts
        .iter()
        .map(|c| c.reason.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "patch conflict in {path}: {reasons}. \
         Someone else changed {path} since the version you viewed. Nothing was \
         written — a patch applies to all its files or none. Re-view {path} and \
         rewrite the operations against the new line numbers."
    )
}

// ---------------------------------------------------------------------------
// Tests — ported from src/hostfn/patch.test.ts. This file is deliberately
// exhaustive: every operation, every rejection, and — the part that actually
// matters — the rebase-vs-conflict decision proved in BOTH directions.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build file text from lines, with a trailing newline like a real file.
    fn doc(lines: &[&str]) -> String {
        format!("{}\n", lines.join("\n"))
    }

    fn fmap(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn try_apply(
        input: &str,
        current: &[(&str, &str)],
        base: Option<&[(&str, &str)]>,
    ) -> Result<HashMap<String, String>, BoughError> {
        let files = fmap(current);
        let ops = parse_patch(input)?;
        let base_map = base.map(fmap);
        apply_patch(&files, &ops, base_map.as_ref())
    }

    fn apply(input: &str, current: &[(&str, &str)]) -> HashMap<String, String> {
        try_apply(input, current, None).expect("patch should apply")
    }

    fn apply_with(
        input: &str,
        current: &[(&str, &str)],
        base: &[(&str, &str)],
    ) -> HashMap<String, String> {
        try_apply(input, current, Some(base)).expect("patch should apply")
    }

    /// Assert the result is a `PatchError` whose message contains `needle`,
    /// and hand the error back so a test can make further claims about its
    /// text. Error text is a product surface, so it is asserted on, not merely
    /// tolerated.
    fn throws_patch<T: std::fmt::Debug>(r: Result<T, BoughError>, needle: &str) -> BoughError {
        let err = match r {
            Err(e) => e,
            Ok(v) => panic!("expected a PatchError, but nothing was thrown (got {v:?})"),
        };
        assert_eq!(err.name(), "PatchError", "expected a PatchError, got {err}");
        if !needle.is_empty() {
            assert!(
                err.to_string().contains(needle),
                "message did not contain {needle:?}:\n  {err}"
            );
        }
        err
    }

    /// A four-hex tag guaranteed not to be `text`'s.
    fn wrong_tag(text: &str) -> &'static str {
        if tag_of(text) == "0000" {
            "FFFF"
        } else {
            "0000"
        }
    }

    fn six() -> String {
        doc(&["one", "two", "three", "four", "five", "six"])
    }

    fn svec(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    // -----------------------------------------------------------------------
    // tags, normalization, joining
    // -----------------------------------------------------------------------

    #[test]
    fn tag_of_crlf_and_bom_do_not_change_a_files_identity() {
        let lf = "a\nb\n";
        assert_eq!(tag_of(lf), tag_of("a\r\nb\r\n"));
        assert_eq!(tag_of(lf), tag_of("\u{FEFF}a\nb\n"));
        assert_ne!(tag_of(lf), tag_of("a\nc\n"));
        let tag = tag_of(lf);
        assert!(
            tag.len() == 4
                && tag
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_lowercase()),
            "{tag}"
        );
    }

    #[test]
    fn tag_of_is_fnv1a_over_utf16_code_units() {
        // Values pinned against the TS implementation (charCodeAt semantics).
        // "héllo 🌍" contains a surrogate PAIR — a port hashing bytes or
        // chars would disagree on all of these.
        assert_eq!(tag_of("a\nb\n"), "6DEE");
        assert_eq!(tag_of(&six()), "65FB");
        assert_eq!(tag_of("héllo 🌍\n"), "2E00");
        assert_eq!(tag_of("你好世界"), "A414");
        assert_eq!(tag_of(""), "9DC5");
    }

    #[test]
    fn normalize_to_lines_a_trailing_newline_is_not_a_line() {
        assert_eq!(normalize("\u{FEFF}a\r\nb\r\n"), "a\nb\n");
        assert_eq!(to_lines("a\nb\n"), svec(&["a", "b"]));
        assert_eq!(to_lines("a\nb"), svec(&["a", "b"]));
        assert_eq!(to_lines(""), Vec::<String>::new());
        assert_eq!(to_lines("\n"), svec(&[""]));
    }

    #[test]
    fn join_lines_line_ending_style_and_trailing_newline_survive_a_patch() {
        assert_eq!(join_lines(&svec(&["a", "b"]), "x\r\ny\r\n"), "a\r\nb\r\n");
        assert_eq!(join_lines(&svec(&["a", "b"]), "x\ny\n"), "a\nb\n");
        assert_eq!(join_lines(&svec(&["a", "b"]), "x\ny"), "a\nb");
        // A file emptied by a patch is empty, not a blank line.
        assert_eq!(join_lines(&[], "x\ny\n"), "");
    }

    #[test]
    fn render_numbered_the_exact_shape_view_hands_the_model() {
        let text = doc(&["alpha", "beta"]);
        assert_eq!(
            render_numbered("a.ts", &text),
            format!("[a.ts#{}]\n1:alpha\n2:beta", tag_of(&text))
        );
        // Numbers are right-aligned once the file needs two digits.
        let lines: Vec<String> = (0..10).map(|i| format!("L{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let wide = render_numbered("a.ts", &doc(&refs));
        assert!(wide.contains("\n 1:L0"), "{wide}");
        assert!(wide.contains("\n10:L9"), "{wide}");
    }

    // -----------------------------------------------------------------------
    // parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_patch_every_operation_with_path_and_tag_attached() {
        let ops = parse_patch(
            &[
                "[src/a.ts#A1B2]",
                "SWAP 74.=76:",
                "+  hello",
                "+",
                "DEL 91.=92",
                "INS.PRE 30:",
                "+// before",
                "INS.POST 30:",
                "+// after",
                "INS.HEAD:",
                "+// top",
                "INS.TAIL:",
                "+// bottom",
            ]
            .join("\n"),
        )
        .unwrap();
        assert_eq!(
            ops.iter().map(|o| o.kind).collect::<Vec<_>>(),
            vec![
                OpKind::Swap,
                OpKind::Del,
                OpKind::InsPre,
                OpKind::InsPost,
                OpKind::InsHead,
                OpKind::InsTail
            ]
        );
        assert!(ops.iter().all(|o| o.path == "src/a.ts" && o.tag == "A1B2"));
        assert_eq!((ops[0].a, ops[0].b), (Some(74), Some(76)));
        // A lone "+" is a blank line, not a terminator.
        assert_eq!(ops[0].body, svec(&["  hello", ""]));
        assert_eq!((ops[1].a, ops[1].b), (Some(91), Some(92)));
        assert_eq!(ops[2].a, Some(30));
        assert_eq!(ops[3].a, Some(30));
        assert_eq!(ops[4].a, None);
        assert_eq!(ops[5].a, None);
        // The input line is retained so parse errors can point at it.
        assert_eq!(ops[0].at, 2);
    }

    #[test]
    fn parse_patch_tagless_headers_and_lowercase_tags() {
        assert_eq!(parse_patch("[a.ts#]\nDEL 1").unwrap()[0].tag, "");
        assert_eq!(parse_patch("[a.ts]\nDEL 1").unwrap()[0].tag, "");
        assert_eq!(parse_patch("[a.ts#a1b2]\nDEL 1").unwrap()[0].tag, "A1B2");
        // A four-hex tag wins the trailing segment rather than joining the path.
        assert_eq!(parse_patch("[a.ts#a1b2]\nDEL 1").unwrap()[0].path, "a.ts");
        // A "#" that is not a tag stays in the path.
        assert_eq!(
            parse_patch("[weird#name.ts]\nDEL 1").unwrap()[0].path,
            "weird#name.ts"
        );
    }

    #[test]
    fn parse_patch_single_line_and_alternate_range_spellings() {
        for spelling in [
            "SWAP 5:",
            "SWAP 5.=5:",
            "SWAP 5..5:",
            "SWAP 5-5:",
            "SWAP 5 5:",
        ] {
            let ops = parse_patch(&format!("[a.ts#]\n{spelling}\n+x")).unwrap();
            assert_eq!((ops[0].a, ops[0].b), (Some(5), Some(5)), "{spelling}");
        }
        assert_eq!(parse_patch("[a.ts#]\nDEL 7").unwrap()[0].b, Some(7));
        assert_eq!(parse_patch("[a.ts#]\nDEL 7..9").unwrap()[0].b, Some(9));
    }

    #[test]
    fn parse_patch_multiple_files_in_one_patch() {
        let ops = parse_patch("[a.ts#]\nDEL 1\n\n[b.ts#C0DE]\nINS.TAIL:\n+z").unwrap();
        let groups = group_by_file(&ops).unwrap();
        let shape: Vec<(String, String, usize)> = groups
            .iter()
            .map(|g| (g.path.clone(), g.tag.clone(), g.ops.len()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("a.ts".into(), "".into(), 1),
                ("b.ts".into(), "C0DE".into(), 1)
            ]
        );
    }

    #[test]
    fn parse_patch_a_blank_line_ends_a_body_without_becoming_content() {
        let ops = parse_patch("[a.ts#]\nINS.HEAD:\n+x\n\nINS.TAIL:\n+y").unwrap();
        assert_eq!(ops[0].body, svec(&["x"]));
        assert_eq!(ops[1].body, svec(&["y"]));
    }

    #[test]
    fn parse_patch_codex_style_envelopes_are_swallowed() {
        assert_eq!(
            parse_patch("*** Begin Patch\n[a.ts#]\nDEL 1\n*** End Patch")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn parse_patch_rejections_name_the_input_line_and_the_fix() {
        let cases: [(&str, &str); 9] = [
            ("", "empty patch"),
            ("DEL 1\n+x", "expected a section header"),
            ("[a.ts#]", "has no operations"),
            ("[a.ts#]\n+x", "has no operation above it"),
            ("[a.ts#]\nDEL 1\n+x", "DEL takes no body rows"),
            (
                "[a.ts#]\n-old line",
                "\"-\" rows are not part of this format",
            ),
            (
                "[a.ts#]\n  12:const x = 1;",
                "looks like a line from view()'s listing",
            ),
            ("[a.ts#]\nREPLACE 1 2", "is not an operation"),
            ("[a.ts#]\nSWAP one:", "is not an operation"),
        ];
        for (input, needle) in cases {
            throws_patch(parse_patch(input), needle);
        }
    }

    #[test]
    fn parse_patch_pasting_view_output_back_is_diagnosed_not_guessed_at() {
        let listing = render_numbered("a.ts", &six());
        let err = throws_patch(parse_patch(&listing), "Do not pass view()'s output");
        assert!(err.to_string().contains("\"[a.ts#]\""), "{err}");
    }

    #[test]
    fn group_by_file_one_path_with_two_different_tags_is_refused() {
        let ops = parse_patch("[a.ts#A1B2]\nDEL 1\n\n[a.ts#C3D4]\nDEL 5").unwrap();
        throws_patch(group_by_file(&ops), "appears twice with different tags");
        // The same tag twice merges into one section.
        let merged =
            group_by_file(&parse_patch("[a.ts#A1B2]\nDEL 1\n\n[a.ts#A1B2]\nDEL 5").unwrap())
                .unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].ops.len(), 2);
    }

    // -----------------------------------------------------------------------
    // every operation, end to end
    // -----------------------------------------------------------------------

    #[test]
    fn apply_swap_single_line_multi_line_range_collapse_and_expand() {
        assert_eq!(
            apply("[a#]\nSWAP 2:\n+TWO", &[("a", &six())])["a"],
            doc(&["one", "TWO", "three", "four", "five", "six"])
        );
        assert_eq!(
            apply("[a#]\nSWAP 2.=4:\n+X", &[("a", &six())])["a"],
            doc(&["one", "X", "five", "six"])
        );
        assert_eq!(
            apply("[a#]\nSWAP 2.=2:\n+X\n+Y\n+Z", &[("a", &six())])["a"],
            doc(&["one", "X", "Y", "Z", "three", "four", "five", "six"])
        );
    }

    #[test]
    fn apply_del_single_line_range_and_the_whole_file() {
        assert_eq!(
            apply("[a#]\nDEL 3", &[("a", &six())])["a"],
            doc(&["one", "two", "four", "five", "six"])
        );
        assert_eq!(
            apply("[a#]\nDEL 2.=5", &[("a", &six())])["a"],
            doc(&["one", "six"])
        );
        assert_eq!(apply("[a#]\nDEL 1.=6", &[("a", &six())])["a"], "");
    }

    #[test]
    fn apply_ins_pre_text_lands_before_the_named_line() {
        assert_eq!(
            apply("[a#]\nINS.PRE 1:\n+ZERO", &[("a", &six())])["a"],
            doc(&["ZERO", "one", "two", "three", "four", "five", "six"])
        );
        assert_eq!(
            apply("[a#]\nINS.PRE 6:\n+FIVE-AND-A-HALF", &[("a", &six())])["a"],
            doc(&[
                "one",
                "two",
                "three",
                "four",
                "five",
                "FIVE-AND-A-HALF",
                "six"
            ])
        );
    }

    #[test]
    fn apply_ins_post_text_lands_after_the_named_line() {
        assert_eq!(
            apply("[a#]\nINS.POST 1:\n+ONE-AND-A-HALF", &[("a", &six())])["a"],
            doc(&[
                "one",
                "ONE-AND-A-HALF",
                "two",
                "three",
                "four",
                "five",
                "six"
            ])
        );
        assert_eq!(
            apply("[a#]\nINS.POST 6:\n+SEVEN", &[("a", &six())])["a"],
            doc(&["one", "two", "three", "four", "five", "six", "SEVEN"])
        );
    }

    #[test]
    fn apply_ins_head_ins_tail() {
        assert_eq!(
            apply(
                "[a#]\nINS.HEAD:\n+top\nINS.TAIL:\n+bottom",
                &[("a", &six())]
            )["a"],
            doc(&["top", "one", "two", "three", "four", "five", "six", "bottom"])
        );
        // They are the only ops that work on an empty file.
        assert_eq!(
            apply("[a#]\nINS.HEAD:\n+first", &[("a", "")])["a"],
            doc(&["first"])
        );
        assert_eq!(
            apply("[a#]\nINS.TAIL:\n+last", &[("a", "")])["a"],
            doc(&["last"])
        );
    }

    #[test]
    fn apply_an_empty_ins_body_is_a_no_op_not_a_corruption() {
        assert_eq!(
            apply("[a#]\nINS.HEAD:\nINS.TAIL:", &[("a", &six())])["a"],
            six()
        );
    }

    #[test]
    fn apply_all_six_operations_at_once() {
        let out = apply(
            &[
                "[a#]",
                "INS.HEAD:",
                "+H",
                "INS.PRE 1:",
                "+P",
                "SWAP 2:",
                "+TWO",
                "INS.POST 3:",
                "+Q",
                "DEL 5",
                "INS.TAIL:",
                "+T",
            ]
            .join("\n"),
            &[("a", &six())],
        );
        assert_eq!(
            out["a"],
            doc(&["H", "P", "one", "TWO", "three", "Q", "four", "six", "T"])
        );
    }

    // -----------------------------------------------------------------------
    // viewed coordinates — the "never apply sequentially" rule
    // -----------------------------------------------------------------------

    #[test]
    fn line_numbers_are_in_the_viewed_versions_coordinates() {
        // DEL 1.=2 removes two lines; applied sequentially, SWAP 5 would then
        // point at "six". It must still mean "five".
        assert_eq!(
            apply("[a#]\nDEL 1.=2\nSWAP 5:\n+FIVE", &[("a", &six())])["a"],
            doc(&["three", "four", "FIVE", "six"])
        );
        // An expanding SWAP above must not shift the anchor below it either.
        assert_eq!(
            apply("[a#]\nSWAP 1:\n+A\n+B\n+C\nDEL 4", &[("a", &six())])["a"],
            doc(&["A", "B", "C", "two", "three", "five", "six"])
        );
    }

    #[test]
    fn op_order_in_the_patch_text_does_not_change_the_result() {
        let forwards = "[a#]\nDEL 1\nINS.POST 3:\n+X\nSWAP 6:\n+SIX";
        let backwards = "[a#]\nSWAP 6:\n+SIX\nINS.POST 3:\n+X\nDEL 1";
        assert_eq!(
            apply(forwards, &[("a", &six())])["a"],
            apply(backwards, &[("a", &six())])["a"]
        );
        assert_eq!(
            apply(forwards, &[("a", &six())])["a"],
            doc(&["two", "three", "X", "four", "five", "SIX"])
        );
    }

    #[test]
    fn materialize_gap_ordering_is_fixed_and_documented() {
        let lines = svec(&["one", "two", "three"]);
        assert_eq!(
            materialize(
                &lines,
                &parse_patch("[a#]\nINS.PRE 2:\n+pre2\nSWAP 2:\n+TWO\nINS.POST 2:\n+post2")
                    .unwrap()
            ),
            svec(&["one", "pre2", "TWO", "post2", "three"])
        );
        // INS.POST N precedes INS.PRE N+1 in the gap they share.
        assert_eq!(
            materialize(
                &lines,
                &parse_patch("[a#]\nINS.PRE 2:\n+B\nINS.POST 1:\n+A").unwrap()
            ),
            svec(&["one", "A", "B", "two", "three"])
        );
        // Two ops of the same kind at one anchor emit in patch order.
        assert_eq!(
            materialize(
                &lines,
                &parse_patch("[a#]\nINS.POST 1:\n+A\nINS.POST 1:\n+B").unwrap()
            ),
            svec(&["one", "A", "B", "two", "three"])
        );
    }

    #[test]
    fn ins_post_at_the_last_line_of_a_del_span_still_lands() {
        assert_eq!(
            apply("[a#]\nDEL 2.=4\nINS.POST 4:\n+X", &[("a", &six())])["a"],
            doc(&["one", "X", "five", "six"])
        );
    }

    // -----------------------------------------------------------------------
    // rejections
    // -----------------------------------------------------------------------

    #[test]
    fn out_of_bounds_anchors_are_rejected() {
        throws_patch(
            try_apply("[a#]\nSWAP 7:\n+x", &[("a", &six())], None),
            "out of range",
        );
        throws_patch(
            try_apply("[a#]\nDEL 0", &[("a", &six())], None),
            "out of range",
        );
        throws_patch(
            try_apply("[a#]\nINS.PRE 9:\n+x", &[("a", &six())], None),
            "out of range",
        );
        throws_patch(
            try_apply("[a#]\nINS.POST 7:\n+x", &[("a", &six())], None),
            "out of range",
        );
        throws_patch(
            try_apply("[a#]\nDEL 3.=99", &[("a", &six())], None),
            "is invalid",
        );
        throws_patch(
            try_apply("[a#]\nSWAP 4.=2:\n+x", &[("a", &six())], None),
            "is invalid",
        );
        // The message names the file and its real length so the model can re-aim.
        throws_patch(
            try_apply("[a#]\nSWAP 7:\n+x", &[("a", &six())], None),
            "a has 6 lines",
        );
    }

    #[test]
    fn an_empty_file_rejects_line_anchored_ops_by_name() {
        throws_patch(
            try_apply("[a#]\nSWAP 1:\n+x", &[("a", "")], None),
            "a is empty",
        );
        throws_patch(try_apply("[a#]\nDEL 1", &[("a", "")], None), "INS.HEAD");
    }

    #[test]
    fn overlapping_ranges_are_rejected_rather_than_silently_ordered() {
        throws_patch(
            try_apply("[a#]\nSWAP 2.=4:\n+x\nDEL 3.=5", &[("a", &six())], None),
            "operations overlap",
        );
        // Identical spans overlap too.
        throws_patch(
            try_apply("[a#]\nDEL 2\nDEL 2", &[("a", &six())], None),
            "operations overlap",
        );
        // A span fully containing another is caught regardless of the order written.
        throws_patch(
            try_apply("[a#]\nDEL 3\nSWAP 2.=5:\n+x", &[("a", &six())], None),
            "operations overlap",
        );
        // Touching-but-disjoint spans are fine.
        assert_eq!(
            apply("[a#]\nSWAP 2.=3:\n+X\nDEL 4.=5", &[("a", &six())])["a"],
            doc(&["one", "X", "six"])
        );
    }

    #[test]
    fn an_ins_anchored_inside_a_replaced_span_is_rejected() {
        throws_patch(
            try_apply(
                "[a#]\nSWAP 2.=4:\n+X\nINS.PRE 3:\n+Y",
                &[("a", &six())],
                None,
            ),
            "anchors inside lines 2.=4",
        );
        throws_patch(
            try_apply("[a#]\nDEL 2.=4\nINS.POST 2:\n+Y", &[("a", &six())], None),
            "anchors inside lines 2.=4",
        );
        // The span boundaries themselves are legal: before it, and after it.
        assert_eq!(
            apply(
                "[a#]\nSWAP 2.=4:\n+X\nINS.PRE 2:\n+B\nINS.POST 4:\n+A",
                &[("a", &six())]
            )["a"],
            doc(&["one", "B", "X", "A", "five", "six"])
        );
        // And so is inserting around a single-line SWAP.
        assert_eq!(
            apply(
                "[a#]\nSWAP 2:\n+X\nINS.PRE 2:\n+B\nINS.POST 2:\n+A",
                &[("a", &six())]
            )["a"],
            doc(&["one", "B", "X", "A", "three", "four", "five", "six"])
        );
    }

    #[test]
    fn swap_with_no_body_is_rejected_del_is_how_you_remove_lines() {
        throws_patch(
            try_apply("[a#]\nSWAP 2.=3:", &[("a", &six())], None),
            "has no body rows",
        );
        throws_patch(
            try_apply("[a#]\nSWAP 2.=3:", &[("a", &six())], None),
            "use DEL 2.=3",
        );
    }

    #[test]
    fn a_path_missing_from_the_file_set_is_named() {
        throws_patch(
            try_apply("[nope.ts#]\nDEL 1", &[("a", &six())], None),
            "nope.ts is not in this patch's file set",
        );
    }

    #[test]
    fn check_ops_is_callable_directly_and_judges_one_files_ops() {
        let ops = parse_patch("[a#]\nDEL 1.=2").unwrap();
        check_ops("a", &ops, 6).unwrap();
        throws_patch(check_ops("a", &ops, 1), "is invalid");
    }

    // -----------------------------------------------------------------------
    // tags: explicit, chained, stale
    // -----------------------------------------------------------------------

    #[test]
    fn an_explicit_tag_matching_the_current_text_applies() {
        assert_eq!(
            apply(&format!("[a#{}]\nDEL 1", tag_of(&six())), &[("a", &six())])["a"],
            doc(&["two", "three", "four", "five", "six"])
        );
    }

    #[test]
    fn a_patch_chains_the_second_is_written_against_the_firsts_echoed_tag() {
        let first = apply("[a#]\nDEL 1", &[("a", &six())])["a"].clone();
        let second = apply(
            &format!("[a#{}]\nSWAP 1:\n+TWO", tag_of(&first)),
            &[("a", &first)],
        )["a"]
            .clone();
        assert_eq!(second, doc(&["TWO", "three", "four", "five", "six"]));
    }

    #[test]
    fn a_stale_tag_is_refused_with_the_empty_tag_escape_hatch_spelled_out() {
        let err = throws_patch(
            try_apply(
                &format!("[a#{}]\nDEL 1", wrong_tag(&six())),
                &[("a", &six())],
                None,
            ),
            "stale tag",
        );
        assert!(
            err.to_string()
                .contains(&format!("is now #{}", tag_of(&six()))),
            "{err}"
        );
        assert!(err.to_string().contains("\"[a#]\""), "{err}");
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn a_tag_that_does_not_match_the_recorded_snapshot_is_stale() {
        let viewed = doc(&["alpha", "beta"]);
        throws_patch(
            try_apply(
                &format!("[a#{}]\nDEL 1", wrong_tag(&viewed)),
                &[("a", &viewed)],
                Some(&[("a", &viewed)]),
            ),
            "stale tag",
        );
    }

    #[test]
    fn patching_a_file_this_session_never_viewed_is_refused() {
        // `base` is present but missing the path: there is no version to
        // rebase from, so applying against the current text would be exactly
        // the silent clobber.
        throws_patch(
            try_apply(
                "[a#]\nDEL 1",
                &[("a", &six()), ("b", &six())],
                Some(&[("b", &six())]),
            ),
            "no viewed version of a is on record",
        );
    }

    // -----------------------------------------------------------------------
    // rebase vs conflict — BOTH directions
    // -----------------------------------------------------------------------

    fn base4() -> String {
        doc(&["alpha", "beta", "gamma", "delta"])
    }

    #[test]
    fn rebase_the_file_moved_but_the_patched_range_is_untouched() {
        // Another agent inserted a line at the top of the version we viewed.
        let current = doc(&["header", "alpha", "beta", "gamma", "delta"]);
        let out = apply_with(
            "[a#]\nSWAP 4:\n+DELTA",
            &[("a", &current)],
            &[("a", &base4())],
        );
        // Both edits survive: the other agent's header AND ours, correctly aimed.
        assert_eq!(
            out["a"],
            doc(&["header", "alpha", "beta", "gamma", "DELTA"])
        );
    }

    #[test]
    fn rebase_an_insert_in_the_middle_shifts_only_the_ops_below_it() {
        let current = doc(&["one", "two", "NEW", "three", "four", "five", "six"]);
        let out = apply_with(
            "[a#]\nSWAP 5.=6:\n+FIVE-SIX",
            &[("a", &current)],
            &[("a", &six())],
        );
        assert_eq!(
            out["a"],
            doc(&["one", "two", "NEW", "three", "four", "FIVE-SIX"])
        );
    }

    #[test]
    fn rebase_a_deletion_above_shifts_the_ops_below_it() {
        let current = doc(&["one", "three", "four", "five", "six"]);
        let out = apply_with("[a#]\nDEL 6", &[("a", &current)], &[("a", &six())]);
        assert_eq!(out["a"], doc(&["one", "three", "four", "five"]));
    }

    #[test]
    fn rebase_an_explicit_tag_naming_a_superseded_but_known_version_still_rebases() {
        let current = doc(&["header", "alpha", "beta", "gamma", "delta"]);
        let out = apply_with(
            &format!("[a#{}]\nSWAP 4:\n+DELTA", tag_of(&base4())),
            &[("a", &current)],
            &[("a", &base4())],
        );
        assert_eq!(
            out["a"],
            doc(&["header", "alpha", "beta", "gamma", "DELTA"])
        );
    }

    #[test]
    fn rebase_unchanged_file_needs_no_rebase_and_is_byte_identical_elsewhere() {
        let out = apply_with(
            "[a#]\nSWAP 4:\n+DELTA",
            &[("a", &base4())],
            &[("a", &base4())],
        );
        assert_eq!(out["a"], doc(&["alpha", "beta", "gamma", "DELTA"]));
    }

    #[test]
    fn conflict_the_patched_line_itself_was_rewritten() {
        let current = doc(&["alpha", "beta", "gamma", "delta -- edited elsewhere"]);
        let err = throws_patch(
            try_apply(
                "[a#]\nSWAP 4:\n+DELTA",
                &[("a", &current)],
                Some(&[("a", &base4())]),
            ),
            "patch conflict in a",
        );
        // Names the file, the range, and the move (error text is a surface).
        assert!(
            err.to_string().contains("lines 4.=4 were rewritten"),
            "{err}"
        );
        assert!(err.to_string().contains("Someone else changed a"), "{err}");
        assert!(err.to_string().contains("Re-view a"), "{err}");
        assert!(err.to_string().contains("Nothing was written"), "{err}");
    }

    #[test]
    fn conflict_lines_were_inserted_inside_the_patched_span() {
        // The op's footprint would now cover a line the agent never saw;
        // rewriting it would silently discard the other agent's insert.
        let current = doc(&["one", "two", "NEW", "three", "four", "five", "six"]);
        throws_patch(
            try_apply(
                "[a#]\nSWAP 2.=4:\n+X",
                &[("a", &current)],
                Some(&[("a", &six())]),
            ),
            "lines 2.=4 had lines inserted inside them",
        );
    }

    #[test]
    fn conflict_the_patched_line_was_deleted_by_the_other_write() {
        let current = doc(&["alpha", "gamma", "delta"]);
        throws_patch(
            try_apply(
                "[a#]\nSWAP 2:\n+BETA",
                &[("a", &current)],
                Some(&[("a", &base4())]),
            ),
            "lines 2.=2 were rewritten",
        );
    }

    #[test]
    fn conflict_every_conflicting_range_is_listed_not_just_the_first() {
        let current = doc(&["alpha!", "beta", "gamma!", "delta"]);
        let err = throws_patch(
            try_apply(
                "[a#]\nSWAP 1:\n+A\nSWAP 3:\n+G",
                &[("a", &current)],
                Some(&[("a", &base4())]),
            ),
            "",
        );
        assert!(err.to_string().contains("lines 1.=1"), "{err}");
        assert!(err.to_string().contains("lines 3.=3"), "{err}");
    }

    #[test]
    fn conflict_one_touched_range_refuses_the_whole_files_other_clean_ops() {
        let current = doc(&["alpha", "beta!", "gamma", "delta"]);
        throws_patch(
            try_apply(
                "[a#]\nSWAP 2:\n+B\nSWAP 4:\n+D",
                &[("a", &current)],
                Some(&[("a", &base4())]),
            ),
            "patch conflict in a",
        );
        // The clean op alone would have landed — the refusal is the conflict rule.
        assert_eq!(
            apply_with("[a#]\nSWAP 4:\n+D", &[("a", &current)], &[("a", &base4())])["a"],
            doc(&["alpha", "beta!", "gamma", "D"])
        );
    }

    #[test]
    fn ins_head_ins_tail_never_conflict_they_name_no_line() {
        let current = doc(&["totally", "different", "content"]);
        let out = apply_with(
            "[a#]\nINS.TAIL:\n+z",
            &[("a", &current)],
            &[("a", &base4())],
        );
        assert_eq!(out["a"], doc(&["totally", "different", "content", "z"]));
    }

    #[test]
    fn bounds_are_judged_in_viewed_coordinates_not_the_current_files() {
        // The other write truncated the file; our op named line 4 of what we
        // viewed, which no longer exists. That is a conflict, not an
        // out-of-range parse error.
        let current = doc(&["alpha", "beta"]);
        throws_patch(
            try_apply(
                "[a#]\nSWAP 4:\n+D",
                &[("a", &current)],
                Some(&[("a", &base4())]),
            ),
            "patch conflict in a",
        );
    }

    #[test]
    fn rebase_ops_and_line_map_are_usable_directly() {
        let base = svec(&["a", "b", "c"]);
        assert_eq!(
            line_map(&base, &svec(&["x", "a", "b", "c"])),
            vec![Some(1), Some(2), Some(3)]
        );
        assert_eq!(
            line_map(&base, &svec(&["a", "B", "c"])),
            vec![Some(0), None, Some(2)]
        );

        let ops = parse_patch("[f#]\nDEL 2").unwrap();
        match rebase_ops(&ops, &base, &svec(&["x", "a", "b", "c"])) {
            RebaseResult::Ok { ops } => assert_eq!((ops[0].a, ops[0].b), (Some(3), Some(3))),
            RebaseResult::Conflicts(c) => panic!("expected ok, got conflicts {c:?}"),
        }
        match rebase_ops(&ops, &base, &svec(&["a", "B", "c"])) {
            RebaseResult::Ok { .. } => panic!("expected conflicts"),
            RebaseResult::Conflicts(conflicts) => assert_eq!(conflicts.len(), 1),
        }
    }

    // -----------------------------------------------------------------------
    // multi-file atomicity
    // -----------------------------------------------------------------------

    #[test]
    fn multi_file_all_files_change_together_on_success() {
        let out = apply(
            "[a#]\nDEL 1\n\n[b#]\nINS.TAIL:\n+z",
            &[
                ("a", &six()),
                ("b", &doc(&["x", "y"])),
                ("untouched", &doc(&["keep"])),
            ],
        );
        assert_eq!(out["a"], doc(&["two", "three", "four", "five", "six"]));
        assert_eq!(out["b"], doc(&["x", "y", "z"]));
        // Files the patch never mentions come through verbatim.
        assert_eq!(out["untouched"], doc(&["keep"]));
    }

    #[test]
    fn multi_file_all_or_none_one_conflict_discards_the_whole_patch() {
        let b_current = doc(&["x", "CHANGED ELSEWHERE"]);
        let current: [(&str, &str); 2] = [("a", &six()), ("b", &b_current)];
        let b_base = doc(&["x", "y"]);
        let base: [(&str, &str); 2] = [("a", &six()), ("b", &b_base)];
        let files = fmap(&current);
        let ops = parse_patch("[a#]\nDEL 1\n\n[b#]\nSWAP 2:\n+Y").unwrap();

        throws_patch(
            apply_patch(&files, &ops, Some(&fmap(&base))),
            "patch conflict in b",
        );
        // The input map is untouched — a caller that writes only the return
        // value cannot have half-applied this patch.
        assert_eq!(files["a"], six());
        assert_eq!(files["b"], b_current);

        // The first file alone would have applied, proving the refusal is the
        // atomicity rule and not some unrelated failure.
        assert_eq!(
            apply_with("[a#]\nDEL 1", &current, &base)["a"],
            doc(&["two", "three", "four", "five", "six"])
        );
    }

    #[test]
    fn multi_file_a_later_out_of_range_op_discards_the_earlier_valid_file() {
        let b = doc(&["x"]);
        let files = fmap(&[("a", &six()), ("b", &b)]);
        throws_patch(
            apply_patch(
                &files,
                &parse_patch("[a#]\nDEL 1\n\n[b#]\nDEL 9").unwrap(),
                None,
            ),
            "out of range",
        );
        assert_eq!(files["a"], six());
    }

    #[test]
    fn multi_file_a_stale_tag_on_the_second_file_discards_the_first() {
        let b = doc(&["x"]);
        let files = fmap(&[("a", &six()), ("b", &b)]);
        let input = format!("[a#]\nDEL 1\n\n[b#{}]\nDEL 1", wrong_tag(&b));
        throws_patch(
            apply_patch(&files, &parse_patch(&input).unwrap(), None),
            "stale tag",
        );
        assert_eq!(files["a"], six());
    }

    // -----------------------------------------------------------------------
    // purity
    // -----------------------------------------------------------------------

    #[test]
    fn apply_patch_mutates_neither_argument_and_is_repeatable() {
        let files = fmap(&[("a", &six())]);
        let base = fmap(&[("a", &six())]);
        let ops = parse_patch("[a#]\nSWAP 1:\n+ONE").unwrap();
        let snapshot = ops.clone();

        let out = apply_patch(&files, &ops, Some(&base)).unwrap();
        assert_eq!(files["a"], six());
        assert_eq!(base["a"], six());
        assert_eq!(
            out["a"],
            doc(&["ONE", "two", "three", "four", "five", "six"])
        );
        // The rebase does not rewrite ops in place.
        assert_eq!(ops, snapshot);
        // Same inputs, same answer.
        assert_eq!(
            apply_patch(&files, &ops, Some(&base)).unwrap()["a"],
            out["a"]
        );
    }

    #[test]
    fn crlf_files_keep_their_line_endings_through_a_patch() {
        assert_eq!(
            apply("[a#]\nDEL 2", &[("a", "one\r\ntwo\r\nthree\r\n")])["a"],
            "one\r\nthree\r\n"
        );
        // …including when the patch body itself is plain LF.
        assert_eq!(
            apply("[a#]\nSWAP 2:\n+TWO", &[("a", "one\r\ntwo\r\n")])["a"],
            "one\r\nTWO\r\n"
        );
    }

    #[test]
    fn a_file_with_no_trailing_newline_keeps_having_none() {
        assert_eq!(
            apply("[a#]\nSWAP 1:\n+ONE", &[("a", "one\ntwo")])["a"],
            "ONE\ntwo"
        );
    }

    // -----------------------------------------------------------------------
    // Regression: multi-line spans must check their INTERIOR, not just
    // endpoints. Found by adversarial review. The endpoint-only check was
    // inherited verbatim from src/tools/hashedit.ts and silently discarded a
    // concurrent in-place edit whenever the line count was preserved — the
    // exact lost update this module exists to prevent.
    // -----------------------------------------------------------------------

    #[test]
    fn conflict_a_multi_line_swap_whose_interior_was_rewritten_in_place() {
        let base = doc(&["X", "Y", "Z"]);
        // another writer changed line 2; count unchanged
        let cur = doc(&["X", "Y-EDITED", "Z"]);
        let err = throws_patch(
            try_apply(
                "[a#]\nSWAP 1.=3:\n+N",
                &[("a", &cur)],
                Some(&[("a", &base)]),
            ),
            "1.=3",
        );
        assert!(err.to_string().contains('a'), "names the file");
    }

    #[test]
    fn conflict_a_multi_line_del_whose_interior_was_rewritten_in_place() {
        let base = doc(&["a", "b", "c", "d", "e"]);
        let cur = doc(&["a", "B!", "C!", "d", "e"]);
        throws_patch(
            try_apply("[f#]\nDEL 1.=5", &[("f", &cur)], Some(&[("f", &base)])),
            "1.=5",
        );
    }

    #[test]
    fn rebase_still_succeeds_when_the_span_is_untouched_and_merely_shifts() {
        let base = doc(&["a", "b", "c"]);
        let cur = doc(&["header", "a", "b", "c"]); // inserted ABOVE the span
        let out = apply_with("[g#]\nSWAP 1.=3:\n+N", &[("g", &cur)], &[("g", &base)]);
        assert_eq!(out["g"], doc(&["header", "N"]));
    }
}
