/**
 * Oversized command output goes to a file, and the turn is told where.
 *
 * THE PROBLEM. A shell command's output is the one thing in a turn whose size the
 * model does not choose. `npm test` on a failing suite is 800,000 characters; a
 * verbose build is more. Two ways of handling that were already in the tree and
 * both lose:
 *
 *   - RETURN IT ALL. The retention buffer allowed 400,000 characters — around
 *     100,000 tokens — from a single `bash()`. One noisy command could consume half
 *     a context window, and everything the turn learned before it is what gets
 *     compacted away to make room.
 *   - TRUNCATE IT. Cheap, and it throws away the middle permanently. The failing
 *     assertion in a test run is almost never in the first or last 5,000
 *     characters, so the agent's next move is to re-run the command with a filter
 *     it is now guessing at — paying for the whole thing twice and often missing.
 *
 * The third option is the obvious one and it needed somewhere to put the file: keep
 * the output, on disk, and spend a few inline characters saying where. Nothing is
 * lost, the context cost is bounded and small, and the follow-up — `rg` for the
 * failure, `bough patterns` if it is log-shaped — is a targeted read of a local
 * file rather than a second execution of an expensive command.
 *
 * WHY THE SCRATCHPAD AND NOT `/tmp`. Both were argued in `scratch.ts` and the
 * conclusion holds here: `/tmp` is emptied on reboot and reaped after ten days, and
 * it is shared, so a `find /tmp` in a later turn matches another conversation's
 * debris and reads it as this task's own. The session scratchpad is per-session,
 * swept on our schedule, and already exported to every command as `$BOUGH_SCRATCH`.
 *
 * NO SCRATCHPAD MEANS NO SPILL, AND THEN NOTHING IS DROPPED THAT WOULD NOT HAVE
 * BEEN. A unit test and any caller without a session have nowhere to write, and the
 * aggressive inline budget only makes sense as a trade AGAINST a file that holds the
 * rest. Without one, this falls back to the old generous head and tail — dropping
 * 99% of a build log to save context is right when the log is one command away, and
 * simply destructive when it is gone forever.
 *
 * PURE CORE, INJECTED EDGES. `planSpill` decides; `spill` writes. The filesystem
 * arrives as three functions so a test never touches a real directory.
 */
import { appendFileSync, existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

// ---------------------------------------------------------------------------
// Deterministic truncation — the fallback, and what retention itself uses
// ---------------------------------------------------------------------------

/**
 * Verbatim head retained per shell. Smaller than the tail on purpose: the head of
 * a build log is the invocation and the first failure, the tail is where it ended
 * up, and the middle is the part nobody reads.
 */
export const MAX_HEAD_CHARS = 100_000;
/** Verbatim tail retained per shell. */
export const MAX_TAIL_CHARS = 300_000;

/** How much output a single shell retains before the middle starts being omitted. */
export const MAX_BUF = MAX_HEAD_CHARS + MAX_TAIL_CHARS;

/** Head/tail budget for one retained buffer. Injected so tests can use small ones. */
export interface TruncateLimits {
  head?: number;
  tail?: number;
}

/**
 * The omission marker.
 *
 * NOTE: the port dropped the OLDEST chars and said so in one line. Spec §6 asks
 * for head + tail verbatim instead, which means the marker has to name how much
 * went missing from the *middle* — and, because error text is a product surface,
 * name the move that avoids it next time (filter at the source).
 */
export function omissionMarker(omitted: number, total: number): string {
  return `\n[… ${omitted} chars omitted from the middle of ${total} — head and tail are ` +
    `verbatim. Filter at the source (rg, head, tail, targeted reads) instead of dumping ` +
    `output …]\n`;
}

/**
 * Keep the first `head` and last `tail` characters verbatim, with an explicit
 * marker where the middle used to be. Pure and deterministic: the same input
 * always yields the same output, and nothing summarizes anything.
 */
export function truncateMiddle(text: string, limits: TruncateLimits = {}): string {
  const head = limits.head ?? MAX_HEAD_CHARS;
  const tail = limits.tail ?? MAX_TAIL_CHARS;
  if (text.length <= head + tail) return text;
  const omitted = text.length - head - tail;
  return text.slice(0, head) + omissionMarker(omitted, text.length) +
    text.slice(text.length - tail);
}

/**
 * Output longer than this is written to a file.
 *
 * 20,000 characters is roughly 5,000 tokens — already a large thing to read, and
 * far above what an ordinary command produces. `git status`, a targeted `rg`, a
 * passing test run: all comfortably under, and all completely unaffected by any of
 * this. What clears the bar is the category this exists for — a failing suite, a
 * full build, an unfiltered log.
 */
export const SPILL_OVER_CHARS = 20_000;

/** Verbatim head kept inline when output spills. */
export const SPILL_HEAD_CHARS = 5_000;

/**
 * Verbatim tail kept inline when output spills.
 *
 * Equal to the head, unlike the retention buffer's 1:3 split. That asymmetry is
 * right when the tail is ALL you keep, because a command's verdict is at the end.
 * Here the whole output is on disk either way, so the inline extract is a preview
 * whose only job is to let the model recognize what it is looking at and decide
 * where to grep — and for that, the beginning (what ran, with what arguments) is
 * worth exactly as much as the end (how it finished).
 */
export const SPILL_TAIL_CHARS = 5_000;

/** The filesystem, injected. */
export interface SpillDeps {
  exists: (path: string) => boolean;
  mkdirp: (dir: string) => void;
  write: (path: string, text: string) => void;
  /** Append to a file, creating it if absent. Used by the streaming sink. */
  append: (path: string, text: string) => void;
}

const realDeps: SpillDeps = {
  exists: (p) => existsSync(p),
  mkdirp: (d) => {
    mkdirSync(d, { recursive: true });
  },
  write: (p, t) => writeFileSync(p, t, "utf8"),
  append: (p, t) => appendFileSync(p, t, "utf8"),
};

/**
 * A file that a shell's output is streamed into as it arrives.
 *
 * WHY STREAMING RATHER THAN WRITING THE BUFFER AT THE END, which is what this did
 * first and what looked correct until it was driven: the retention buffer caps at
 * 400,000 characters and drops the middle of anything larger. Writing it out
 * afterwards therefore saved a file that had ALREADY lost the middle — complete
 * with the omission marker embedded in it — under a banner reading "FULL OUTPUT
 * SAVED". `seq 1 200000` produced 1.29MB, the file held 400KB, and the marker
 * claimed it was everything. A tool that says it kept your output and did not is
 * worse than one that admits it truncated.
 *
 * So the sink opens on the first chunk past the threshold and every subsequent
 * chunk goes to disk. The in-memory buffer keeps doing its own job for the inline
 * extract; this is a second, complete copy.
 *
 * OPENED LAZILY, because most commands never reach the threshold and a file per
 * `git status` would litter the scratchpad with empty logs.
 */
export interface SpillSink {
  path: string;
  /** Everything written so far, to report the true total rather than the retained one. */
  chars: number;
  /** Newlines seen, counted as we stream — the file is never re-read to find out. */
  lines: number;
}

/**
 * Give this shell a sink if it has earned one, and write `text` to it.
 *
 * `pending` is the output produced BEFORE the threshold was crossed — it lives only
 * in the retention buffer at that moment, and without it the file would start
 * mid-stream and miss the very beginning, which is where a build log says what it
 * was building.
 */
export function streamSpill(
  sink: SpillSink | undefined,
  text: string,
  ctx: SpillCtx & { totalSoFar: number; pending: () => string },
  deps: SpillDeps = realDeps,
): SpillSink | undefined {
  if (!ctx.scratch) return undefined;
  try {
    if (!sink) {
      if (ctx.totalSoFar <= SPILL_OVER_CHARS) return undefined;
      deps.mkdirp(ctx.scratch);
      const path = nextPath(ctx.scratch, ctx.label ?? "output", deps);
      const head = ctx.pending();
      deps.write(path, head);
      return { path, chars: head.length, lines: countLines(head) };
    }
    deps.append(sink.path, text);
    return {
      path: sink.path,
      chars: sink.chars + text.length,
      // -1 because `countLines` counts a trailing partial line that the previous
      // chunk already counted; concatenating two chunks must not invent a line.
      lines: sink.lines + countLines(text) - 1,
    };
  } catch {
    // A full disk must not kill a running command. Give up on the file and let the
    // inline extract fall back to plain truncation.
    return sink;
  }
}

/** What a caller knows about where this output came from. */
export interface SpillCtx {
  /** The session scratchpad. Absent disables spilling entirely. */
  scratch?: string;
  /** Base name for the file — `bash`, `sh`, `bg_3`. Defaults to `output`. */
  label?: string;
}

/**
 * The first free `<label>-NNN.log` in `dir`.
 *
 * A counter would be shorter and wrong across restarts: it resets to 1 while the
 * scratchpad still holds a `bash-001.log` from before, and the next spill silently
 * overwrites output some earlier turn may still be about to read. Probing costs a
 * handful of `existsSync` calls on a directory that holds a handful of files.
 */
function nextPath(dir: string, label: string, deps: SpillDeps): string {
  for (let i = 1; i <= 999; i++) {
    const p = join(dir, `${label}-${String(i).padStart(3, "0")}.log`);
    if (!deps.exists(p)) return p;
  }
  // 999 spills in one session is not a real scenario, but silently overwriting
  // would be, so the last slot is reused explicitly rather than by accident.
  return join(dir, `${label}-999.log`);
}

/** What the inline extract should say. Pure — no filesystem, no decision to write. */
export interface SpillPlan {
  /** True when the text is over the threshold AND there is somewhere to put it. */
  spilled: boolean;
  head: string;
  tail: string;
  omitted: number;
  lines: number;
}

/** Decide whether and how to split. Pure. */
export function planSpill(text: string, canWrite: boolean): SpillPlan {
  if (!canWrite || text.length <= SPILL_OVER_CHARS) {
    return { spilled: false, head: "", tail: "", omitted: 0, lines: 0 };
  }
  const head = text.slice(0, SPILL_HEAD_CHARS);
  const tail = text.slice(text.length - SPILL_TAIL_CHARS);
  return {
    spilled: true,
    head,
    tail,
    omitted: text.length - head.length - tail.length,
    lines: countLines(text),
  };
}

function countLines(text: string): number {
  let n = 1;
  for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) === 10) n++;
  return n;
}

/**
 * The marker that replaces the middle.
 *
 * Every clause earns its characters. The PATH is the point. The SIZE tells the
 * reader whether grepping is worth it. And the three suggested moves are spelled
 * out as runnable commands rather than described, because an agent that has to
 * compose the incantation from a description will sometimes compose the wrong one
 * and conclude the file is empty — and `bough patterns` in particular is a thing it
 * would not otherwise think to reach for on a 9,000-line log.
 */
export function spillMarker(path: string, total: number, lines: number, omitted: number): string {
  return (
    `\n[… ${omitted.toLocaleString("en-US")} chars omitted from the middle. ` +
    `FULL OUTPUT SAVED — ${total.toLocaleString("en-US")} chars` +
    `${lines > 0 ? `, ${lines.toLocaleString("en-US")} lines` : ""}:\n` +
    `   ${path}\n` +
    `   rg -n 'error|fail' ${shellQuote(path)}   — find the part you need\n` +
    `   bough patterns --llm ${shellQuote(path)}   — if it is log-shaped, this summarizes it\n` +
    `   view(${JSON.stringify(path)})   — read it directly\n` +
    `Head and tail below are verbatim. Do not re-run the command to see the middle …]\n`
  );
}

/** Single-quote a path for a shell word, so a space or a `$` cannot break the hint. */
function shellQuote(p: string): string {
  return `'${p.replace(/'/g, `'\\''`)}'`;
}

/**
 * Bound `text` for a tool result, writing the full copy to the scratchpad when it
 * is large and there is a scratchpad to write it to.
 *
 * Returns the text to show. A write failure is swallowed deliberately: a full disk
 * or a read-only scratchpad must degrade to the old truncation, not turn a
 * successful command into a failed host call. The model then sees the ordinary
 * omission marker and is no worse off than before this existed.
 */
export function spill(
  text: string,
  ctx: SpillCtx & { sink?: SpillSink },
  deps: SpillDeps = realDeps,
): string {
  // Already streamed to disk: use THAT file and THAT total. The `text` here came
  // out of the retention buffer, so its length is the retained size, not the real
  // one — reporting it would understate a 10MB command as 400KB.
  if (ctx.sink) {
    const plan = planSpill(text, true);
    const head = plan.spilled ? plan.head : text;
    const tail = plan.spilled ? plan.tail : "";
    const omitted = Math.max(0, ctx.sink.chars - head.length - tail.length);
    return head + spillMarker(ctx.sink.path, ctx.sink.chars, ctx.sink.lines, omitted) + tail;
  }
  const plan = planSpill(text, Boolean(ctx.scratch));
  if (!plan.spilled) {
    // Under the threshold, or nowhere to write. The generous head/tail is the right
    // fallback in the second case — see the module note.
    return truncateMiddle(text, { head: MAX_HEAD_CHARS, tail: MAX_TAIL_CHARS });
  }
  const dir = ctx.scratch as string;
  try {
    deps.mkdirp(dir);
    const path = nextPath(dir, ctx.label ?? "output", deps);
    deps.write(path, text);
    return plan.head + spillMarker(path, text.length, plan.lines, plan.omitted) + plan.tail;
  } catch {
    return truncateMiddle(text, { head: MAX_HEAD_CHARS, tail: MAX_TAIL_CHARS });
  }
}
