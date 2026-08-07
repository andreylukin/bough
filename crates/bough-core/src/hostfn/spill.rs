//! Oversized command output goes to a file, and the turn is told where.
//! Port of `src/hostfn/spill.ts` (spec: hostfn.md §spill).
//!
//! THE PROBLEM. A shell command's output is the one thing in a turn whose size
//! the model does not choose. Returning it all lets one noisy command consume
//! half a context window; truncating it throws away the middle permanently —
//! and the failing assertion in a test run is almost never in the first or
//! last 5,000 characters. The third option: keep the output, on disk, and
//! spend a few inline characters saying where.
//!
//! NO SCRATCHPAD MEANS NO SPILL, AND THEN NOTHING IS DROPPED THAT WOULD NOT
//! HAVE BEEN. A unit test and any caller without a session have nowhere to
//! write, and the aggressive inline budget only makes sense as a trade AGAINST
//! a file that holds the rest. Without one, this falls back to the old
//! generous head and tail.
//!
//! PURE CORE, INJECTED EDGES. `plan_spill` decides; `spill` writes. The
//! filesystem arrives as a trait object so a test never touches a real
//! directory.
//!
//! "Chars" throughout are Unicode scalar values (`char`s) — TS counted UTF-16
//! code units. The two agree on ASCII, which shell output overwhelmingly is;
//! what matters is self-consistency, and that a slice never lands mid-glyph.

use std::path::Path;

// ---------------------------------------------------------------------------
// Deterministic truncation — the fallback, and what retention itself uses
// ---------------------------------------------------------------------------

/// Verbatim head retained per shell. Smaller than the tail on purpose: the
/// head of a build log is the invocation and the first failure, the tail is
/// where it ended up, and the middle is the part nobody reads.
pub const MAX_HEAD_CHARS: usize = 100_000;
/// Verbatim tail retained per shell.
pub const MAX_TAIL_CHARS: usize = 300_000;

/// How much output a single shell retains before the middle starts being
/// omitted.
pub const MAX_BUF: usize = MAX_HEAD_CHARS + MAX_TAIL_CHARS;

/// Head/tail budget for one retained buffer. Injected so tests can use small
/// ones.
#[derive(Debug, Clone, Copy, Default)]
pub struct TruncateLimits {
    pub head: Option<usize>,
    pub tail: Option<usize>,
}

/// The omission marker.
///
/// NOTE: spec §6 asks for head + tail verbatim, which means the marker has to
/// name how much went missing from the *middle* — and, because error text is a
/// product surface, name the move that avoids it next time (filter at the
/// source).
pub fn omission_marker(omitted: usize, total: usize) -> String {
    format!(
        "\n[… {omitted} chars omitted from the middle of {total} — head and tail are \
         verbatim. Filter at the source (rg, head, tail, targeted reads) instead of dumping \
         output …]\n"
    )
}

/// Keep the first `head` and last `tail` characters verbatim, with an explicit
/// marker where the middle used to be. Pure and deterministic: the same input
/// always yields the same output, and nothing summarizes anything.
pub fn truncate_middle(text: &str, limits: TruncateLimits) -> String {
    let head = limits.head.unwrap_or(MAX_HEAD_CHARS);
    let tail = limits.tail.unwrap_or(MAX_TAIL_CHARS);
    let len = char_len(text);
    if len <= head + tail {
        return text.to_string();
    }
    let omitted = len - head - tail;
    format!(
        "{}{}{}",
        take_chars(text, head),
        omission_marker(omitted, len),
        last_chars(text, tail)
    )
}

/// Output longer than this is written to a file.
///
/// 20,000 characters is roughly 5,000 tokens — already a large thing to read,
/// and far above what an ordinary command produces. `git status`, a targeted
/// `rg`, a passing test run: all comfortably under, and all completely
/// unaffected by any of this. What clears the bar is the category this exists
/// for — a failing suite, a full build, an unfiltered log.
pub const SPILL_OVER_CHARS: usize = 20_000;

/// Verbatim head kept inline when output spills.
pub const SPILL_HEAD_CHARS: usize = 5_000;

/// Verbatim tail kept inline when output spills.
///
/// Equal to the head, unlike the retention buffer's 1:3 split. That asymmetry
/// is right when the tail is ALL you keep, because a command's verdict is at
/// the end. Here the whole output is on disk either way, so the inline extract
/// is a preview whose only job is to let the model recognize what it is
/// looking at and decide where to grep.
pub const SPILL_TAIL_CHARS: usize = 5_000;

/// The filesystem, injected. Failures are values (`io::Result`), never panics
/// — a full disk must not kill a running command.
pub trait SpillDeps {
    fn exists(&self, path: &str) -> bool;
    fn mkdirp(&self, dir: &str) -> std::io::Result<()>;
    fn write(&self, path: &str, text: &str) -> std::io::Result<()>;
    /// Append to a file, creating it if absent. Used by the streaming sink.
    fn append(&self, path: &str, text: &str) -> std::io::Result<()>;
    /// Read a file back. Used only to build the digest from the COMPLETE
    /// output, since the caller's copy came out of the capped retention buffer.
    fn read(&self, path: &str) -> std::io::Result<String>;
}

/// The real filesystem.
pub struct RealSpillDeps;

impl SpillDeps for RealSpillDeps {
    fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }
    fn mkdirp(&self, dir: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)
    }
    fn write(&self, path: &str, text: &str) -> std::io::Result<()> {
        std::fs::write(path, text)
    }
    fn append(&self, path: &str, text: &str) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        f.write_all(text.as_bytes())
    }
    fn read(&self, path: &str) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
}

// ---------------------------------------------------------------------------
// The digest — what the spilled output was actually made of
// ---------------------------------------------------------------------------

/// Patterns rendered inline. Six is what fits alongside a head and a tail
/// without the extract stopping being an extract.
const DIGEST_TOP: usize = 6;

/// Hard ceiling on the digest, so the inline cost stays bounded by a constant
/// the way the head and tail are. A pathological log with six enormous
/// templates gets cut here rather than blowing the budget.
const DIGEST_MAX_CHARS: usize = 4_000;

/// Below this, there is nothing to compress — the head and tail already show
/// most of it, and a "3 lines → 3 patterns" header is pure overhead.
const DIGEST_MIN_LINES: usize = 40;

/// One line in every `DIGEST_MIN_RATIO` must be a repeat of another for the
/// summary to be worth its characters. Output where nearly every line is
/// structurally distinct — prose, a diff, source code, a single-line blob —
/// does not compress, and a digest listing it back is noise that displaces the
/// verbatim tail.
const DIGEST_MIN_RATIO: usize = 4;

/// Output above this is pointed at rather than analyzed. The pipeline is one
/// bounded-memory pass and runs at roughly 30,000 lines a second, but a command
/// that printed 100MB should not add seconds of latency to its own result —
/// and at that size the file needs targeted grepping anyway.
const DIGEST_MAX_ANALYZE_CHARS: usize = 8_000_000;

/// Compress `text` into the handful of statements it is made of, or `None` when
/// that is not worth doing.
///
/// WHY THIS EXISTS AT ALL. A spilled command's output is exactly the case the
/// log pipeline was built for — a failing suite, a full build, an unfiltered
/// log — and until now the only thing the model got was a path and a suggestion
/// to grep it. That suggestion is a second round trip, and it only works if the
/// model already knows what to grep FOR. The digest answers that question in
/// the same result: which statements this output consists of, how often each
/// fired, and which of them were errors.
///
/// PURE. It reads no file and writes none; the caller decides what text to
/// hand it.
pub fn digest(text: &str) -> Option<String> {
    use crate::logs::analyze::{AnalyzeOptions, Analyzer};
    use crate::logs::format::to_llm;

    if char_len(text) > DIGEST_MAX_ANALYZE_CHARS {
        return None;
    }
    let mut analyzer = Analyzer::new(AnalyzeOptions {
        top: DIGEST_TOP,
        ..Default::default()
    });
    for line in text.lines() {
        analyzer.push(line);
    }
    let analysis = analyzer.finish();
    // `lines` counts only non-blank lines, which is the right denominator: a
    // log padded with blank lines has not compressed just because they were
    // dropped.
    let lines = analysis.lines as usize;
    if lines < DIGEST_MIN_LINES || analysis.pattern_count * DIGEST_MIN_RATIO > lines {
        return None;
    }
    Some(clip_to_pattern(&to_llm(&analysis)))
}

/// A file that a shell's output is streamed into as it arrives.
///
/// WHY STREAMING RATHER THAN WRITING THE BUFFER AT THE END, which is what the
/// TS did first and what looked correct until it was driven: the retention
/// buffer caps at 400,000 characters and drops the middle of anything larger.
/// Writing it out afterwards therefore saved a file that had ALREADY lost the
/// middle — complete with the omission marker embedded in it — under a banner
/// reading "FULL OUTPUT SAVED". `seq 1 200000` produced 1.29MB, the file held
/// 400KB, and the marker claimed it was everything. A tool that says it kept
/// your output and did not is worse than one that admits it truncated.
///
/// So the sink opens on the first chunk past the threshold and every
/// subsequent chunk goes to disk. The in-memory buffer keeps doing its own job
/// for the inline extract; this is a second, complete copy.
///
/// OPENED LAZILY, because most commands never reach the threshold and a file
/// per `git status` would litter the scratchpad with empty logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillSink {
    pub path: String,
    /// Everything written so far, to report the true total rather than the
    /// retained one.
    pub chars: usize,
    /// Newlines seen, counted as we stream — the file is never re-read to
    /// find out.
    pub lines: usize,
}

/// What a caller knows about where this output came from.
#[derive(Debug, Clone, Default)]
pub struct SpillCtx {
    /// The session scratchpad. Absent disables spilling entirely.
    pub scratch: Option<String>,
    /// Base name for the file — `bash`, `sh`, `bg_3`. Defaults to `output`.
    pub label: Option<String>,
}

/// Give this shell a sink if it has earned one, and write `text` to it.
///
/// `pending` is the output produced BEFORE the threshold was crossed — it
/// lives only in the retention buffer at that moment, and without it the file
/// would start mid-stream and miss the very beginning, which is where a build
/// log says what it was building.
///
/// A write failure returns the sink unchanged (or `None` if it never opened):
/// a full disk must not kill a running command; the inline extract then falls
/// back to plain truncation.
pub fn stream_spill(
    sink: Option<SpillSink>,
    text: &str,
    ctx: &SpillCtx,
    total_so_far: usize,
    pending: impl FnOnce() -> String,
    deps: &dyn SpillDeps,
) -> Option<SpillSink> {
    let scratch = ctx.scratch.as_deref()?;
    match sink {
        None => {
            if total_so_far <= SPILL_OVER_CHARS {
                return None;
            }
            let open = move || -> std::io::Result<SpillSink> {
                deps.mkdirp(scratch)?;
                let path = next_path(scratch, ctx.label.as_deref().unwrap_or("output"), deps);
                let head = pending();
                deps.write(&path, &head)?;
                Ok(SpillSink {
                    path,
                    chars: char_len(&head),
                    lines: count_lines(&head),
                })
            };
            // Give up on the file and let the inline extract fall back to
            // plain truncation.
            open().ok()
        }
        Some(s) => match deps.append(&s.path, text) {
            Ok(()) => Some(SpillSink {
                path: s.path.clone(),
                chars: s.chars + char_len(text),
                // -1 because `count_lines` counts a trailing partial line that
                // the previous chunk already counted; concatenating two chunks
                // must not invent a line.
                lines: s.lines + count_lines(text) - 1,
            }),
            Err(_) => Some(s),
        },
    }
}

/// The first free `<label>-NNN.log` in `dir`.
///
/// A counter would be shorter and wrong across restarts: it resets to 1 while
/// the scratchpad still holds a `bash-001.log` from before, and the next spill
/// silently overwrites output some earlier turn may still be about to read.
/// Probing costs a handful of `exists` calls on a directory that holds a
/// handful of files.
fn next_path(dir: &str, label: &str, deps: &dyn SpillDeps) -> String {
    for i in 1..=999 {
        let p = Path::new(dir).join(format!("{label}-{i:03}.log"));
        let p = p.to_string_lossy().into_owned();
        if !deps.exists(&p) {
            return p;
        }
    }
    // 999 spills in one session is not a real scenario, but silently
    // overwriting would be, so the last slot is reused explicitly rather than
    // by accident.
    Path::new(dir)
        .join(format!("{label}-999.log"))
        .to_string_lossy()
        .into_owned()
}

/// What the inline extract should say. Pure — no filesystem, no decision to
/// write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpillPlan {
    /// True when the text is over the threshold AND there is somewhere to put
    /// it.
    pub spilled: bool,
    pub head: String,
    pub tail: String,
    pub omitted: usize,
    pub lines: usize,
}

/// Decide whether and how to split. Pure.
pub fn plan_spill(text: &str, can_write: bool) -> SpillPlan {
    let len = char_len(text);
    if !can_write || len <= SPILL_OVER_CHARS {
        return SpillPlan::default();
    }
    let head = take_chars(text, SPILL_HEAD_CHARS).to_string();
    let tail = last_chars(text, SPILL_TAIL_CHARS).to_string();
    SpillPlan {
        spilled: true,
        omitted: len - char_len(&head) - char_len(&tail),
        lines: count_lines(text),
        head,
        tail,
    }
}

/// Enforce `DIGEST_MAX_CHARS` at a pattern boundary rather than mid-glyph.
///
/// A hard cut lands in the middle of a template or a slot's value list, and the
/// result reads as though the pattern itself were malformed — a summary whose
/// last entry looks corrupted invites exactly the re-run it exists to prevent.
/// Dropping whole patterns and saying how many is honest and shorter.
fn clip_to_pattern(rendered: &str) -> String {
    if char_len(rendered) <= DIGEST_MAX_CHARS {
        return rendered.to_string();
    }
    let cut = take_chars(rendered, DIGEST_MAX_CHARS);
    let kept = match cut.rfind("\n### ") {
        Some(i) => &cut[..i],
        // No boundary at all: one enormous pattern. Nothing to do but cut it.
        None => cut,
    };
    let shown = kept.matches("\n### ").count();
    let total = rendered.matches("\n### ").count();
    format!(
        "{kept}\n\n[… {} more pattern(s) not shown here — the summary is capped; \
         run `bough patterns --llm` on the file for all {total} …]\n",
        total - shown
    )
}

fn count_lines(text: &str) -> usize {
    1 + text.bytes().filter(|&b| b == b'\n').count()
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// The first `n` chars of `s`, never splitting a scalar value.
fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The last `n` chars of `s`.
fn last_chars(s: &str, n: usize) -> &str {
    let len = char_len(s);
    if len <= n {
        return s;
    }
    match s.char_indices().nth(len - n) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

/// The marker that replaces the middle.
///
/// Every clause earns its characters. The PATH is the point. The SIZE tells
/// the reader whether grepping is worth it. And the three suggested moves are
/// spelled out as runnable commands rather than described, because an agent
/// that has to compose the incantation from a description will sometimes
/// compose the wrong one and conclude the file is empty — and `bough patterns`
/// in particular is a thing it would not otherwise think to reach for on a
/// 9,000-line log.
///
/// WHEN A DIGEST IS PRESENT the `bough patterns` hint is dropped rather than
/// kept alongside it: the hint's entire job was to get the model to run that
/// analysis, and it has already been run. Leaving both in would spend
/// characters inviting a round trip whose answer is directly above it.
pub fn spill_marker(
    path: &str,
    total: usize,
    lines: usize,
    omitted: usize,
    digest: Option<&str>,
) -> String {
    let lines_clause = if lines > 0 {
        format!(", {} lines", commafy(lines))
    } else {
        String::new()
    };
    let patterns_hint = match digest {
        Some(_) => String::new(),
        None => format!(
            "   bough patterns --llm {}   — if it is log-shaped, this summarizes it\n",
            shell_quote(path)
        ),
    };
    let digest_block = match digest {
        Some(d) => format!("\nWHAT THE FULL OUTPUT IS MADE OF:\n{d}\n"),
        None => String::new(),
    };
    format!(
        "\n[… {} chars omitted from the middle. \
         FULL OUTPUT SAVED — {} chars{}:\n   {}\n   \
         rg -n 'error|fail' {}   — find the part you need\n\
         {}   \
         view({})   — read it directly\n\
         {}\
         Head and tail below are verbatim. Do not re-run the command to see the middle …]\n",
        commafy(omitted),
        commafy(total),
        lines_clause,
        path,
        shell_quote(path),
        patterns_hint,
        serde_json::to_string(path).unwrap_or_else(|_| format!("\"{path}\"")),
        digest_block,
    )
}

/// `toLocaleString("en-US")` — thousands separators.
fn commafy(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Single-quote a path for a shell word, so a space or a `$` cannot break the
/// hint.
fn shell_quote(p: &str) -> String {
    format!("'{}'", p.replace('\'', "'\\''"))
}

/// Bound `text` for a tool result, writing the full copy to the scratchpad
/// when it is large and there is a scratchpad to write it to.
///
/// Returns the text to show. A write failure is swallowed deliberately: a full
/// disk or a read-only scratchpad must degrade to the old truncation, not turn
/// a successful command into a failed host call. The model then sees the
/// ordinary omission marker and is no worse off than before this existed.
pub fn spill(text: &str, ctx: &SpillCtx, sink: Option<&SpillSink>, deps: &dyn SpillDeps) -> String {
    // Already streamed to disk: use THAT file and THAT total. The `text` here
    // came out of the retention buffer, so its length is the retained size,
    // not the real one — reporting it would understate a 10MB command as
    // 400KB.
    if let Some(sink) = sink {
        let plan = plan_spill(text, true);
        let (head, tail) = if plan.spilled {
            (plan.head.as_str(), plan.tail.as_str())
        } else {
            (text, "")
        };
        let omitted = sink.chars.saturating_sub(char_len(head) + char_len(tail));
        // From the FILE, not from `text`: `text` is the retained buffer and has
        // already lost its middle, so digesting it would describe a sample and
        // present it as the whole. An unreadable file simply means no digest.
        let full = deps.read(&sink.path).ok();
        let digest = full.as_deref().and_then(digest);
        return format!(
            "{head}{}{tail}",
            spill_marker(
                &sink.path,
                sink.chars,
                sink.lines,
                omitted,
                digest.as_deref()
            )
        );
    }
    let plan = plan_spill(text, ctx.scratch.is_some());
    if !plan.spilled {
        // Under the threshold, or nowhere to write. The generous head/tail is
        // the right fallback in the second case — see the module note.
        return truncate_middle(
            text,
            TruncateLimits {
                head: Some(MAX_HEAD_CHARS),
                tail: Some(MAX_TAIL_CHARS),
            },
        );
    }
    let dir = ctx
        .scratch
        .as_deref()
        .expect("plan.spilled implies a scratchpad");
    let attempt = || -> std::io::Result<String> {
        deps.mkdirp(dir)?;
        let path = next_path(dir, ctx.label.as_deref().unwrap_or("output"), deps);
        deps.write(&path, text)?;
        // `text` IS the complete output on this path — nothing streamed, so
        // nothing was capped — and it is already in memory.
        let digest = digest(text);
        Ok(format!(
            "{}{}{}",
            plan.head,
            spill_marker(
                &path,
                char_len(text),
                plan.lines,
                plan.omitted,
                digest.as_deref()
            ),
            plan.tail
        ))
    };
    attempt().unwrap_or_else(|_| {
        truncate_middle(
            text,
            TruncateLimits {
                head: Some(MAX_HEAD_CHARS),
                tail: Some(MAX_TAIL_CHARS),
            },
        )
    })
}

// ---------------------------------------------------------------------------
// Tests — ported from src/hostfn/spill.test.ts. The filesystem is injected,
// so nothing here touches a real directory.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// A fake filesystem recording every write.
    #[derive(Default)]
    struct FakeFs {
        files: RefCell<BTreeMap<String, String>>,
        dirs: RefCell<Vec<String>>,
    }

    impl SpillDeps for FakeFs {
        fn exists(&self, path: &str) -> bool {
            self.files.borrow().contains_key(path)
        }
        fn mkdirp(&self, dir: &str) -> std::io::Result<()> {
            self.dirs.borrow_mut().push(dir.to_string());
            Ok(())
        }
        fn write(&self, path: &str, text: &str) -> std::io::Result<()> {
            self.files
                .borrow_mut()
                .insert(path.to_string(), text.to_string());
            Ok(())
        }
        fn append(&self, path: &str, text: &str) -> std::io::Result<()> {
            let mut files = self.files.borrow_mut();
            let entry = files.entry(path.to_string()).or_default();
            entry.push_str(text);
            Ok(())
        }
        fn read(&self, path: &str) -> std::io::Result<String> {
            self.files
                .borrow()
                .get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::other("ENOENT"))
        }
    }

    /// `n` characters of recognizable filler.
    fn big(n: usize, ch: char) -> String {
        ch.to_string().repeat(n)
    }

    fn ctx(scratch: &str, label: &str) -> SpillCtx {
        SpillCtx {
            scratch: Some(scratch.to_string()),
            label: Some(label.to_string()),
        }
    }

    fn no_label(scratch: &str) -> SpillCtx {
        SpillCtx {
            scratch: Some(scratch.to_string()),
            label: None,
        }
    }

    // -----------------------------------------------------------------------
    // plan_spill — pure
    // -----------------------------------------------------------------------

    #[test]
    fn output_at_or_under_the_threshold_does_not_spill() {
        // The common case by a wide margin: git status, a targeted rg, a
        // passing test run. None of them should be affected by any of this.
        assert!(!plan_spill(&big(SPILL_OVER_CHARS, 'x'), true).spilled);
        assert!(!plan_spill("", true).spilled);
    }

    #[test]
    fn output_over_the_threshold_spills_keeping_head_and_tail_verbatim() {
        let text = format!("HEAD{}TAIL", big(SPILL_OVER_CHARS * 2, 'x'));
        let plan = plan_spill(&text, true);
        assert!(plan.spilled);
        assert_eq!(plan.head.len(), SPILL_HEAD_CHARS);
        assert_eq!(plan.tail.len(), SPILL_TAIL_CHARS);
        assert!(plan.head.starts_with("HEAD"));
        assert!(plan.tail.ends_with("TAIL"));
        assert_eq!(
            plan.omitted,
            text.len() - SPILL_HEAD_CHARS - SPILL_TAIL_CHARS
        );
    }

    #[test]
    fn nowhere_to_write_means_no_spill_whatever_the_size() {
        // The aggressive inline budget is only defensible as a trade against
        // a file holding the rest. Without one it would just be destruction.
        assert!(!plan_spill(&big(SPILL_OVER_CHARS * 10, 'x'), false).spilled);
    }

    #[test]
    fn plan_spill_counts_lines_for_the_marker() {
        let text: String = "line\n"
            .repeat(SPILL_OVER_CHARS / 2)
            .chars()
            .take(SPILL_OVER_CHARS * 2)
            .collect();
        let plan = plan_spill(&text, true);
        assert!(plan.spilled);
        assert_eq!(plan.lines, text.split('\n').count());
    }

    // -----------------------------------------------------------------------
    // spill — the write
    // -----------------------------------------------------------------------

    #[test]
    fn the_full_output_reaches_the_file_not_the_truncated_version() {
        // The entire point. If the file held the extract too, nothing was
        // gained over plain truncation.
        let f = FakeFs::default();
        let text = format!("START{}END", big(SPILL_OVER_CHARS * 3, 'x'));
        let shown = spill(&text, &ctx("/scratch/s1", "bash"), None, &f);
        assert_eq!(f.files.borrow().len(), 1);
        let (path, contents) = {
            let files = f.files.borrow();
            let (p, c) = files.iter().next().unwrap();
            (p.clone(), c.clone())
        };
        assert_eq!(
            contents, text,
            "the file did not receive the complete output"
        );
        // The invariant that matters is a CEILING, not a ratio: the inline
        // cost is the budget plus one marker no matter how vast the command's
        // output was.
        assert!(
            shown.len() <= SPILL_HEAD_CHARS + SPILL_TAIL_CHARS + 1_000,
            "inline extract was {} chars, above the fixed budget",
            shown.len()
        );
        assert!(
            shown.contains(&path),
            "the extract does not name the file it wrote"
        );
    }

    #[test]
    fn the_marker_names_the_size_the_path_and_the_follow_up_moves() {
        // Each clause earns its characters; an agent that cannot compose the
        // follow-up will conclude the file is empty and re-run the command.
        let f = FakeFs::default();
        let text = format!("A{}Z", big(SPILL_OVER_CHARS * 2, 'x'));
        let shown = spill(&text, &ctx("/s", "bash"), None, &f);
        assert!(shown.contains("FULL OUTPUT SAVED"));
        assert!(shown.contains("chars"));
        assert!(shown.contains("lines"));
        assert!(shown.contains("rg "));
        assert!(shown.contains("bough patterns"));
        assert!(shown.contains("view("));
        assert!(shown.contains("Do not re-run the command"));
        // Head and tail survive around the marker.
        assert!(shown.starts_with('A'));
        assert!(shown.ends_with('Z'));
    }

    #[test]
    fn the_directory_is_created_before_the_write() {
        let f = FakeFs::default();
        spill(
            &big(SPILL_OVER_CHARS * 2, 'x'),
            &no_label("/scratch/s9"),
            None,
            &f,
        );
        assert_eq!(*f.dirs.borrow(), vec!["/scratch/s9".to_string()]);
    }

    #[test]
    fn successive_spills_never_overwrite_each_other() {
        // A counter would reset across restarts and clobber a file an earlier
        // turn is still about to read.
        let f = FakeFs::default();
        let c = ctx("/s", "bash");
        spill(
            &format!("one{}", big(SPILL_OVER_CHARS * 2, 'x')),
            &c,
            None,
            &f,
        );
        spill(
            &format!("two{}", big(SPILL_OVER_CHARS * 2, 'x')),
            &c,
            None,
            &f,
        );
        spill(
            &format!("three{}", big(SPILL_OVER_CHARS * 2, 'x')),
            &c,
            None,
            &f,
        );
        let files = f.files.borrow();
        assert_eq!(files.len(), 3);
        let names: Vec<&String> = files.keys().collect();
        assert_eq!(
            names,
            vec!["/s/bash-001.log", "/s/bash-002.log", "/s/bash-003.log"]
        );
        assert!(files["/s/bash-001.log"].starts_with("one"));
        assert!(files["/s/bash-003.log"].starts_with("three"));
    }

    #[test]
    fn the_label_separates_one_verbs_spills_from_anothers() {
        let f = FakeFs::default();
        spill(
            &big(SPILL_OVER_CHARS * 2, 'x'),
            &ctx("/s", "bash"),
            None,
            &f,
        );
        spill(&big(SPILL_OVER_CHARS * 2, 'x'), &ctx("/s", "sh"), None, &f);
        let files = f.files.borrow();
        let names: Vec<&String> = files.keys().collect();
        assert_eq!(names, vec!["/s/bash-001.log", "/s/sh-001.log"]);
    }

    #[test]
    fn a_path_with_a_space_or_a_quote_survives_into_the_suggested_commands() {
        let f = FakeFs::default();
        let shown = spill(
            &big(SPILL_OVER_CHARS * 2, 'x'),
            &ctx("/tmp/my logs", "bash"),
            None,
            &f,
        );
        // Unquoted, the rg hint would silently search two different paths.
        assert!(
            shown.contains("rg -n 'error|fail' '/tmp/my logs/bash-001.log'"),
            "quoted rg hint missing:\n{shown}"
        );
    }

    // -----------------------------------------------------------------------
    // Fallback
    // -----------------------------------------------------------------------

    #[test]
    fn without_a_scratchpad_it_falls_back_to_the_generous_head_and_tail() {
        // Nothing is dropped that would not have been dropped before this
        // existed.
        let f = FakeFs::default();
        let text = big(SPILL_OVER_CHARS * 5, 'x');
        let shown = spill(&text, &SpillCtx::default(), None, &f);
        assert_eq!(f.files.borrow().len(), 0);
        assert_eq!(
            shown, text,
            "a 100k text fits the old budget and must be untouched"
        );
    }

    /// A filesystem that refuses every mutation.
    struct BrokenFs;
    impl SpillDeps for BrokenFs {
        fn exists(&self, _: &str) -> bool {
            false
        }
        fn mkdirp(&self, _: &str) -> std::io::Result<()> {
            Err(std::io::Error::other("EROFS: read-only file system"))
        }
        fn write(&self, _: &str, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn append(&self, _: &str, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn read(&self, _: &str) -> std::io::Result<String> {
            Err(std::io::Error::other("EROFS"))
        }
    }

    #[test]
    fn a_failed_write_degrades_to_truncation_rather_than_failing_the_command() {
        // A full disk must not turn a successful command into a failed host
        // call.
        let text = big(SPILL_OVER_CHARS * 2, 'x');
        let shown = spill(&text, &no_label("/s"), None, &BrokenFs);
        assert!(!shown.contains("FULL OUTPUT SAVED"));
        assert_eq!(
            shown, text,
            "within the fallback budget, so it should be intact"
        );
    }

    #[test]
    fn small_output_is_returned_completely_unchanged() {
        let f = FakeFs::default();
        let text = "ok\n";
        assert_eq!(spill(text, &no_label("/s"), None, &f), text);
        assert_eq!(f.files.borrow().len(), 0);
    }

    #[test]
    fn the_inline_cost_is_bounded_no_matter_how_vast_the_output_is() {
        // The whole promise of the feature, stated as one assertion: a
        // command that prints ten megabytes costs the same context as one
        // that prints thirty kilobytes.
        let f = FakeFs::default();
        let small = spill(&big(SPILL_OVER_CHARS * 2, 'x'), &ctx("/s", "a"), None, &f);
        let huge = spill(&big(10_000_000, 'x'), &ctx("/s", "b"), None, &f);
        let ceiling = SPILL_HEAD_CHARS + SPILL_TAIL_CHARS + 1_000;
        assert!(small.len() <= ceiling);
        assert!(
            huge.len() <= ceiling,
            "10MB of output produced {} inline chars",
            huge.len()
        );
        // And the 10MB is genuinely on disk, not discarded.
        assert_eq!(f.files.borrow()["/s/b-001.log"].len(), 10_000_000);
    }

    // -----------------------------------------------------------------------
    // Streaming sink
    // -----------------------------------------------------------------------

    /// Feed `chunks` through the sink the way `append` does, returning the
    /// file.
    fn stream(chunks: &[String], f: &FakeFs) -> (Option<SpillSink>, Option<String>) {
        let mut sink: Option<SpillSink> = None;
        let mut seen = String::new();
        let c = ctx("/s", "bash");
        for chunk in chunks {
            seen.push_str(chunk);
            let snapshot = seen.clone();
            let total = snapshot.chars().count();
            sink = stream_spill(sink, chunk, &c, total, move || snapshot, f);
        }
        let contents = sink
            .as_ref()
            .and_then(|s| f.files.borrow().get(&s.path).cloned());
        (sink, contents)
    }

    #[test]
    fn the_streamed_file_holds_every_byte_including_the_chunk_that_opened_it() {
        // The regression: opening the sink before writing the triggering
        // chunk dropped exactly one chunk — 262,144 chars of a 1.29MB command
        // — from a file whose banner claimed it held everything.
        let f = FakeFs::default();
        let chunks = vec![
            big(9_000, 'a'),
            big(9_000, 'b'),
            big(9_000, 'c'),
            big(9_000, 'd'),
        ];
        let (sink, contents) = stream(&chunks, &f);
        let sink = sink.expect("the sink should have opened");
        let joined = chunks.concat();
        assert_eq!(
            contents.as_deref(),
            Some(joined.as_str()),
            "the file is not byte-identical to the stream"
        );
        assert_eq!(sink.chars, joined.len());
    }

    #[test]
    fn the_sink_stays_closed_while_output_is_under_the_threshold() {
        // Otherwise every `git status` litters the scratchpad with an empty
        // log.
        let f = FakeFs::default();
        let (sink, _) = stream(&[big(5_000, 'x'), big(5_000, 'x')], &f);
        assert_eq!(sink, None);
        assert_eq!(f.files.borrow().len(), 0);
    }

    #[test]
    fn the_sink_survives_past_the_retention_cap_without_losing_the_middle() {
        // The reason it streams at all: the in-memory buffer caps at 400k, so
        // anything written from it afterwards would be missing precisely the
        // part the marker promises is on disk.
        let f = FakeFs::default();
        let chunks: Vec<String> = (0..12)
            .map(|i| big(50_000, char::from(b'a' + i as u8)))
            .collect();
        let (_, contents) = stream(&chunks, &f);
        let joined = chunks.concat();
        let contents = contents.expect("the sink should have opened");
        assert_eq!(contents.len(), joined.len());
        assert_eq!(contents, joined);
        assert!(
            !contents.contains("omitted from the middle"),
            "an omission marker got baked into the saved file"
        );
    }

    #[test]
    fn the_marker_reports_the_true_total_not_the_retained_size() {
        // `spill` is handed text out of the capped buffer; reporting its
        // length would understate a 1.29MB command as 400KB.
        let f = FakeFs::default();
        let chunks = vec![big(30_000, 'a'), big(30_000, 'b')];
        let (sink, _) = stream(&chunks, &f);
        let sink = sink.expect("the sink should have opened");
        let retained = format!("{}{}", big(1_000, 'a'), big(1_000, 'b'));
        let shown = spill(&retained, &ctx("/s", "bash"), Some(&sink), &f);
        assert!(
            shown.contains("FULL OUTPUT SAVED — 60,000 chars"),
            "true total missing:\n{shown}"
        );
    }

    /// Append always fails; everything else succeeds.
    struct FlakyFs;
    impl SpillDeps for FlakyFs {
        fn exists(&self, _: &str) -> bool {
            false
        }
        fn mkdirp(&self, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn write(&self, _: &str, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn append(&self, _: &str, _: &str) -> std::io::Result<()> {
            Err(std::io::Error::other("ENOSPC"))
        }
        fn read(&self, _: &str) -> std::io::Result<String> {
            Err(std::io::Error::other("ENOENT"))
        }
    }

    #[test]
    fn a_write_failure_mid_stream_does_not_throw_at_the_caller() {
        let c = no_label("/s");
        let mut sink: Option<SpillSink> = None;
        for chunk in [big(30_000, 'x'), big(30_000, 'x')] {
            let pending = chunk.clone();
            sink = stream_spill(sink, &chunk, &c, 60_000, move || pending, &FlakyFs);
        }
        // The failed append left the sink as it was, rather than killing the
        // command.
        let sink = sink.expect("the sink should have opened");
        assert_eq!(sink.chars, 30_000);
    }

    // -----------------------------------------------------------------------
    // The digest
    // -----------------------------------------------------------------------

    /// A log-shaped output: two statements repeated with varying values, which
    /// is what a build, a test run or a server log actually looks like.
    fn log_shaped(n: usize) -> String {
        let mut out = String::new();
        for i in 0..n {
            out.push_str(&format!(
                "INFO  handled GET /api/items/{i} in {}ms\n",
                i % 97
            ));
            out.push_str(&format!("WARN  cache miss for key item-{i}\n"));
        }
        out
    }

    #[test]
    fn a_log_shaped_spill_comes_back_with_the_statements_it_is_made_of() {
        // The point of the whole feature: the model learns what the output
        // consists of in the same result, instead of being handed a path and
        // told to guess what to grep for.
        let f = FakeFs::default();
        let shown = spill(&log_shaped(800), &ctx("/s", "bash"), None, &f);
        assert!(shown.contains("WHAT THE FULL OUTPUT IS MADE OF"));
        assert!(
            shown.contains("handled GET /api/items/"),
            "the digest does not name the dominant statement:\n{shown}"
        );
        assert!(
            shown.contains("cache miss for key item-"),
            "the digest lost the second statement:\n{shown}"
        );
        // Having run the analysis, the marker no longer invites the model to
        // run it.
        assert!(
            !shown.contains("bough patterns"),
            "the digest and the hint to produce it are both present"
        );
    }

    #[test]
    fn output_that_does_not_compress_gets_no_digest_and_keeps_the_hint() {
        // Prose, a diff, a source file: every line structurally distinct.
        // Listing them back is noise that would displace the verbatim tail.
        let f = FakeFs::default();
        // This module's own source, which is the honest fixture: a synthetic
        // one built from a small vocabulary compresses no matter how the lines
        // are shuffled, because clustering folds on structure and structure is
        // what a generator has little of. Real prose and real code do not
        // compress, and if that ever stops being true here, this failing is
        // the right way to find out.
        let text = include_str!("spill.rs");
        assert_eq!(digest(text), None, "source code should not compress");
        // Asserted through the hint rather than through the absence of the
        // digest banner: this fixture is this file, so the banner's own string
        // literal is in the fixture and would match either way.
        let shown = spill(text, &ctx("/s", "bash"), None, &f);
        assert!(
            shown.contains("bough patterns --llm"),
            "with no digest the hint that produces one must survive"
        );
    }

    #[test]
    fn a_streamed_digest_describes_the_file_not_the_retained_buffer() {
        // THE REGRESSION THIS EXISTS TO PIN. `spill` is handed text out of the
        // capped retention buffer, whose middle is already gone. Digesting
        // THAT would describe a sample of the output while the banner above it
        // says "FULL OUTPUT" — a summary that is confidently wrong about what
        // ran. The statement below appears ONLY in the middle.
        let f = FakeFs::default();
        let c = ctx("/s", "bash");
        let chunks = vec![
            log_shaped(400),
            (0..200)
                .map(|i| format!("ERROR failed to open /var/data/shard-{i}.db\n"))
                .collect::<String>(),
            log_shaped(400),
        ];
        let mut sink: Option<SpillSink> = None;
        let mut seen = String::new();
        for chunk in &chunks {
            seen.push_str(chunk);
            let snapshot = seen.clone();
            let total = snapshot.chars().count();
            sink = stream_spill(sink, chunk, &c, total, move || snapshot, &f);
        }
        let sink = sink.expect("the sink should have opened");

        // What the retention buffer would hand `spill`: head and tail only.
        let retained = format!(
            "{}{}",
            take_chars(&chunks[0], 2_000),
            last_chars(&chunks[2], 2_000)
        );
        assert!(
            !retained.contains("failed to open"),
            "the fixture must actually lose the middle for this test to mean anything"
        );

        let shown = spill(&retained, &c, Some(&sink), &f);
        assert!(
            shown.contains("failed to open /var/data/shard-"),
            "the digest missed a statement that only the file holds:\n{shown}"
        );
    }

    #[test]
    fn an_unreadable_spill_file_costs_the_digest_and_nothing_else() {
        // Same rule as every other failure here: degrade, never fail the
        // command.
        let f = FakeFs::default();
        let c = ctx("/s", "bash");
        let sink = SpillSink {
            path: "/s/vanished-001.log".to_string(),
            chars: 900_000,
            lines: 9_000,
        };
        let shown = spill(&log_shaped(100), &c, Some(&sink), &f);
        assert!(!shown.contains("WHAT THE FULL OUTPUT IS MADE OF"));
        assert!(shown.contains("FULL OUTPUT SAVED — 900,000 chars"));
    }

    #[test]
    fn the_digest_does_not_break_the_bounded_inline_cost() {
        // The feature's original promise still holds, with the digest's own
        // ceiling added to it and nothing else.
        let f = FakeFs::default();
        let shown = spill(&log_shaped(50_000), &ctx("/s", "bash"), None, &f);
        assert!(shown.contains("WHAT THE FULL OUTPUT IS MADE OF"));
        let ceiling = SPILL_HEAD_CHARS + SPILL_TAIL_CHARS + DIGEST_MAX_CHARS + 1_000;
        assert!(
            char_len(&shown) <= ceiling,
            "a 2.6MB log produced {} inline chars, above the {ceiling} ceiling",
            char_len(&shown)
        );
    }

    #[test]
    fn an_oversized_digest_is_cut_between_patterns_and_says_what_it_dropped() {
        let body = big(1_500, 'x');
        let rendered = format!("# header\n### #1 a\n{body}\n### #2 b\n{body}\n### #3 c\n{body}\n");
        let clipped = clip_to_pattern(&rendered);
        assert!(char_len(&clipped) <= DIGEST_MAX_CHARS + 200);
        assert!(clipped.contains("### #1 a"));
        assert!(
            !clipped.contains("### #3 c"),
            "the cut kept a pattern it had no room for"
        );
        assert!(
            clipped.contains("1 more pattern(s) not shown"),
            "the cut is silent about what it dropped:\n{clipped}"
        );
    }

    #[test]
    fn digest_declines_on_output_too_short_to_have_a_shape() {
        assert_eq!(digest("one\ntwo\nthree\n"), None);
        assert_eq!(digest(""), None);
    }
}
