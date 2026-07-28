/**
 * The transcript as a flat list of pre-wrapped visual lines.
 *
 * THE INVARIANT THIS HOLDS: **the transcript is data before it is a component.**
 * `buildLines(thread, …)` turns messages into `VLine[]` — one entry per PHYSICAL
 * row, already wrapped, already styled, each carrying its click target and its raw
 * copy text. Rendering is then a slice of an array and scrolling is an index
 * offset, which is what lets every folding rule below be asserted with no renderer
 * mounted and no terminal attached (plan §7). The previous tree grew a 3,618-line
 * `App.tsx` because this boundary did not exist.
 *
 * SECOND INVARIANT — **folding is decided by predicates the caller owns.**
 * `isExpanded`/`isFull` are passed in, so "expand all" and "show the rest of this
 * one block" are caller state, not hidden state here. Two consequences the tests
 * pin: a collapsed fold must still carry every fact you would otherwise expand to
 * find (which calls ran, a gist of the program, whether one errored or is still
 * running), and expand-all must NOT lift the per-block line caps — otherwise one
 * keystroke dumps a 200-line program output into the viewport.
 *
 * THIRD — **a running program is visible while it runs.** The live `console.*`
 * lines (`toolLogs`, keyed by call id) render under a call that has no result yet,
 * and are replaced — not duplicated — by the finalized output when the
 * `tool_result` lands. Spec §5: console output streams live to the UI *and*
 * batches into the model's tool result; those are the same lines seen twice, and
 * the transcript must show them once.
 *
 * Ported from `src/tui/lines.ts`. Gone with the rewrite: the `prose` part kind (the
 * union is frozen at six — schema/parts.ts) and the check-passed hue on subagent
 * cards (there is no acceptance gate — spec §17).
 */
import type { BackgroundJob, Message, Role } from "../schema/parts.ts";
import {
  accent,
  bold,
  clip,
  codeGist,
  danger,
  dim,
  highlightCode,
  info,
  linkifyUrls,
  md,
  MIN_WRAP,
  outputText,
  programSummary,
  segmentParts,
  surface,
  toolSummary,
  warn,
  wrapLine,
} from "./format.ts";
import type { TranscriptMark } from "./store.ts";

export interface VLine {
  text: string;
  /**
   * Click target. A tool-group key toggles its fold; `<key>!full` lifts a block's
   * line cap; `open:<sessionId>` descends into a subagent's branch.
   */
  click?: string;
  /** The raw, unstyled, unwrapped text of this line's section — a copy yields it. */
  copy?: string;
  /**
   * The single unwrapped LINE this row was laid out from.
   *
   * Finer than `copy`, which is the whole section: a drag that crosses a wrap
   * should paste the line, not the block it sits in. `selectedCopy` yields one
   * `src` per run of rows that share it, which un-wraps the line and drops the `│`
   * gutter in the same step — neither the window's break points nor the gutter was
   * ever in the source.
   */
  src?: string;
}

const wrap = (text: string, w: number) => wrapLine(text, Math.max(MIN_WRAP, w));

function push(out: VLine[], text: string, w: number, click?: string) {
  // EVERY wrapped row carries its own raw source, not just the ones that had a
  // reason to before. A copy that spans a wrap used to break the line where the
  // WINDOW broke it — "…a very long command line\n that will certainly wrap" — and
  // a paste of that is not the text anyone selected. `src` is deduped by the
  // reader (`selectedCopy`), so a run of rows sharing one source yields it once.
  for (const l of wrap(text, w)) {
    out.push(click ? { text: l, click, src: text } : { text: l, src: text });
  }
}

/**
 * One accent: green is bough's color; the user speaks in plain bright text and a
 * harness-injected note is amber, because a `system` message is neither of them
 * talking (spec §4) and reading it as the agent's own words is the failure.
 */
const roleLabel = (role: Role): string =>
  role === "user"
    ? bold("you")
    : role === "supervisor"
    ? bold(accent("bough"))
    : bold(warn("system"));

// ---- system notes the UI re-renders as cards --------------------------------

/**
 * A detached subagent's completion note (`agents/notes.ts` `formatSubagentNote`)
 * replays to the model as text, but the bracketed wall is noise to a human. Parse
 * it back into fields so the transcript can draw a real card at the spawn point.
 */
export interface SubagentNote {
  title: string;
  sessionId: string;
  status: string;
  ok: boolean;
  files: string[];
  /** The note could not recover a file list ("not reported", not "none"). */
  filesUnknown: boolean;
  report: string | null;
}

export function parseSubagentNote(text: string): SubagentNote | null {
  const head = text.match(/^\[subagent finished\] "(.*)" \(([^)]+)\) — (.+)\.$/m);
  if (!head) return null;
  const [, title, sessionId, status] = head;
  const filesLine = text.match(/^Changed files: (.+)\.$/m);
  // "not reported" is a fact about the harness's knowledge, not a file named so.
  const filesUnknown = !filesLine || filesLine[1] === "not reported";
  const files = filesLine && filesLine[1] !== "none" && !filesUnknown
    ? filesLine[1].split(", ").map((f) => f.trim())
    : [];
  const report = text.match(/^Report:\n([\s\S]*?)\nIt worked in THIS session's checkout/m);
  // Only "finished" is success. FAILED / STOPPED / ORPHANED each mean something
  // different and the card must not flatten them into one red mark.
  return {
    title,
    sessionId,
    status,
    ok: status.startsWith("finished"),
    files,
    filesUnknown,
    report: report ? report[1].trim() : null,
  };
}

/**
 * The `[background]` wake note (`hostfn/jobs.ts`). It exists to wake the agent,
 * not to inform the user — the job card says the same thing in the user's
 * language, so the raw note is dropped while that card is showing.
 */
// The quoted name is optional because the note carries one only for a job that has
// one — and because a note left in an old transcript predates names entirely. Miss
// this and the raw note renders BESIDE the card that restates it.
const BG_NOTE_RE = /^\[background\] (\S+)(?: "[^"]*")? finished/;
export function parseBgNote(text: string): string | null {
  return BG_NOTE_RE.exec(text.trim())?.[1] ?? null;
}

/**
 * The `[image]` note (`hostfn/image.ts`). The attached part already renders its
 * own placeholder, so the text would repeat an absolute path directly under it —
 * and a program attaching a dozen screenshots would spend three lines each saying
 * so. The note's WORDS are the only part worth keeping.
 */
const IMAGE_NOTE_RE = /^\[image\] (\S+)(?: — (.*))?$/s;
export function parseImageNote(text: string): { path: string; note?: string } | null {
  const m = IMAGE_NOTE_RE.exec(text.trim());
  return m ? { path: m[1], note: m[2]?.trim() || undefined } : null;
}

// ---- blocks -----------------------------------------------------------------

/** Logical lines shown before "+N more" — the program, then its output. */
const CODE_LINES = 14;
const OUTPUT_LINES = 20;
/** Report lines a finished-subagent card shows before "+N more". */
const REPORT_LINES = 6;

/**
 * A gutter-framed block: each logical line wraps to the remaining width and every
 * physical line carries a dim `│` (clickable — anywhere in the block collapses
 * it). A truncated block ends on a "+N more lines" line whose target is `fullKey`,
 * so lifting one block's cap is separate from the fold itself.
 *
 * That line used to end "· click to show all". No click is dispatched anywhere in
 * the TUI and `isFull` has no caller, so the cap cannot be lifted by any means —
 * the row was instructing the reader to do something that does not exist. The
 * count stays, because knowing how much was cut is the useful half; the promise
 * goes.
 */
function pushBlock(
  out: VLine[],
  text: string,
  w: number,
  opts: {
    maxLines: number;
    style: (l: string) => string;
    click: string;
    fullKey?: string;
    raised?: boolean;
  },
) {
  const finish = (l: string) => (opts.raised ? surface(l, w) : l);
  const logical = text.split("\n");
  const shown = logical.slice(0, opts.maxLines);
  for (const line of shown) {
    for (const l of wrap(line, w - 2)) {
      out.push({
        text: finish(`${dim("│")} ${l ? opts.style(l) : ""}`),
        click: opts.click,
        // The block's own line, so a copy across a wrapped one rejoins it and
        // leaves the gutter behind.
        src: line,
      });
    }
  }
  if (logical.length > shown.length) {
    out.push({
      text: finish(
        `${dim("│")} ${dim(`… +${logical.length - shown.length} more lines`)}`,
      ),
      click: opts.fullKey ?? opts.click,
    });
  }
}

/**
 * Program output reads dim — it is the result, not the intent — except the line
 * that says the program died, which is the one line the reader is looking for.
 */
function styleOutputLine(line: string, isError: boolean): string {
  // URLs first: output is where served links land (`artifact()`), and a printed
  // link must open on click like one in prose.
  const l = linkifyUrls(line);
  if (isError || line.startsWith("[program error]")) return danger(l);
  return dim(l);
}

/**
 * One folded tool step. Collapsed, the header carries everything you would expand
 * to learn: how many calls, their names, a gist of the program that ran, an error
 * mark, and a live ⚙ for the call still running. Expanded, each call shows what
 * ran (bright) over what came back (dim) — the brightness IS the boundary.
 */
function toolGroupLines(
  out: VLine[],
  parts: Message["parts"],
  key: string,
  expanded: boolean,
  full: boolean,
  w: number,
  toolLogs?: Record<string, string[]>,
) {
  const capCode = full ? Infinity : CODE_LINES;
  const capOut = full ? Infinity : OUTPUT_LINES;
  const { calls, results, running, hasError, interrupted } = toolSummary(parts);
  if (calls.length === 0) return;
  // THE STATUS COMES FIRST, where nothing can clip it. It used to be appended —
  // `⚙ run_steps…` after the summary — and on a 100-column screen it never once
  // reached the glass, so the only sign a step was in flight was the spinner line
  // shared by the whole turn.
  const state = running
    ? warn("⚙ ")
    : hasError
    ? danger("✗ error  ")
    : interrupted
    ? warn("⏹ interrupted  ")
    : "";
  // Names only when they DIFFER. bough has one tool, so `calls.map(name)` rendered
  // a four-call turn as `run_steps · run_steps · run_steps · run_steps` — four
  // copies of an internal identifier where the prose summary should be.
  const names = [...new Set(calls.map((c) => c.name))];
  const count = `${calls.length} ${calls.length === 1 ? "step" : "steps"}`;
  // The fold glyph stays at text weight — it is the affordance that says
  // "expandable"; an all-dim header reads as inert.
  let head = `${expanded ? "▾" : "▸"} ` + state +
    dim(names.length > 1 ? `${count}  ${names.join(" · ")}` : count);
  // A collapsed group carries WHAT IT DID — every call's gist, not just the first
  // one's. Expanded shows the real thing, so the summary would only repeat it.
  if (!expanded) {
    // WHAT IT DID first, and the code only when nothing was recognized. A clipped
    // line of source as the headline reads as debug output and answers none of the
    // questions a reader actually has (`programSummary`).
    const gists = calls.map((call) => {
      const input = call.input as { code?: unknown } | null | undefined;
      const code = input && typeof input.code === "string" ? input.code : "";
      // Present tense while the call is still open: "ran 1 command" under a shell
      // that has been blocked for ten seconds is a lie the reader acts on.
      const live = !results.get(call.id);
      return programSummary(code, 64, live) || codeGist(call.input);
    }).filter(Boolean);
    // One budget for the whole tail, so a four-step turn clips once at the end
    // rather than losing its last step silently off the edge of the row.
    if (gists.length) head += dim(` · ${clip(gists.join(" · "), Math.max(20, w - 16))}`);
  }
  // Never wrapped: the whole visual row stays one click target.
  out.push({ text: head, click: key });
  if (!expanded) return;
  for (const call of calls) {
    const res = results.get(call.id);
    const status = !res
      ? warn("⚙ running")
      : res.isError
      ? danger("✗ error")
      : res.interrupted
      ? warn("⏹ interrupted")
      : accent("✓ done");
    // The ◇ marker takes the call's status color — accent green next to red error
    // text misreads as success.
    const mark = res?.isError ? danger("◇") : res?.interrupted ? warn("◇") : accent("◇");
    push(out, `${mark} ${call.name} ${status}`, w, key);
    const input = callInput(call.input);
    if (input) {
      // `run_steps` code is JavaScript; a JSON input highlights fine as C-family.
      pushBlock(out, input, w, {
        maxLines: capCode,
        style: (l) => highlightCode(l, "js"),
        click: key,
        fullKey: `${key}!full`,
        raised: true,
      });
    }
    if (res && outputText(res) !== "") {
      out.push({ text: dim("↳ output"), click: key });
      pushBlock(out, outputText(res), w, {
        maxLines: capOut,
        style: (l) => styleOutputLine(l, res.isError),
        click: key,
        fullKey: `${key}!full`,
        raised: true,
      });
    } else {
      // Still running: the program's console lines as they stream in. The
      // finalized `tool_result` replaces them with the same lines joined — which
      // is why this arm is an `else` and not an addition.
      const live = toolLogs?.[call.id];
      if (live?.length) {
        out.push({ text: dim("↳ output (live)"), click: key });
        pushBlock(out, live.join("\n"), w, {
          maxLines: capOut,
          style: (l) => styleOutputLine(l, false),
          click: key,
          fullKey: `${key}!full`,
          raised: true,
        });
      }
    }
  }
}

/** The program as the renderer derives it: `code` verbatim, anything else JSON. */
function callInput(input: unknown): string {
  const raw = input as Record<string, unknown> | null | undefined;
  const code = raw && typeof raw.code === "string" ? raw.code : null;
  return code ?? (input === undefined ? "" : JSON.stringify(input, null, 2));
}

/** The whole group as plain text, for a right-click copy. */
function toolGroupCopy(parts: Message["parts"]): string {
  const { calls, results } = toolSummary(parts);
  return calls.map((call) => {
    let block = `◇ ${call.name}\n${callInput(call.input)}`;
    const res = results.get(call.id);
    if (res && outputText(res) !== "") block += `\n↳ output\n${outputText(res)}`;
    return block;
  }).join("\n\n");
}

// ---- messages ---------------------------------------------------------------

export function messageLines(
  msg: Message,
  isExpanded: (key: string) => boolean,
  isFull: (key: string) => boolean,
  w: number,
  streaming?: string,
  toolLogs?: Record<string, string[]>,
): VLine[] {
  const out: VLine[] = [];
  const body: VLine[] = [];
  const inner = w - 2;
  // An image note collapses to ONE line with no role label: the note text repeats
  // the path the placeholder below it already names.
  if (msg.role === "system") {
    const texts = msg.parts.filter((p) => p.type === "text");
    const imgs = msg.parts.filter((p) => p.type === "image");
    const note = texts.length === 1 && imgs.length === 1
      ? parseImageNote((texts[0] as { text: string }).text)
      : null;
    if (note) {
      const img = imgs[0] as { name: string; size: number; path: string };
      const kb = Math.max(1, Math.round(img.size / 1024));
      const name = (img.name.split("/").pop() || img.name).trim();
      out.push({ text: "" });
      out.push({
        text: "  " + dim(`🖼 ${name}${note.note ? ` — ${note.note}` : ""} · ${kb} KB`),
        copy: note.path,
      });
      return out;
    }
  }
  out.push({ text: "" });
  out.push({ text: roleLabel(msg.role) });
  // Bodies hang two columns under the role label so turns read as blocks.
  segmentParts(msg.parts).forEach((s, i) => {
    const key = `${msg.id}:${i}`;
    const seg: VLine[] = [];
    let copy: string;
    if (s.kind === "text") {
      push(seg, md(s.text, inner), inner);
      copy = s.text;
    } else if (s.kind === "reasoning") {
      // Thinking folds like a tool step: a wall of reasoning is process, not
      // answer. Collapsed = one clickable gist line; expanded = a gutter block,
      // capped like an output. Empty reasoning renders nothing at all — the
      // provider reported thinking without text, and a "▸ thinking ·" line with
      // nothing after it is worse than silence.
      if (!s.text.trim()) return;
      const logical = s.text.split("\n");
      if (isExpanded(key)) {
        seg.push({ text: "▾ " + dim(`thinking (${logical.length} lines)`), click: key });
        pushBlock(seg, s.text, inner, {
          maxLines: isFull(key) ? Infinity : OUTPUT_LINES,
          style: dim,
          click: key,
          fullKey: `${key}!full`,
        });
      } else {
        const gist = logical.map((l) => l.trim()).find((l) => l.length > 0) ?? "";
        seg.push({ text: "▸ " + dim(`thinking · ${clip(gist, 60)}`), click: key });
      }
      copy = s.text;
    } else if (s.kind === "image") {
      // Terminals do not do pixels: a compact placeholder, with the bytes on disk
      // under ~/.bough/attachments and already sent to the model.
      const kb = Math.max(1, Math.round(s.part.size / 1024));
      seg.push({ text: dim(`🖼 ${s.part.name} (${kb} KB)`) });
      copy = s.part.path;
    } else if (s.kind === "ask") {
      // A settled `ask()` — one always-visible line: the question, then how it
      // ended. Never folded: the user's own answer is not plumbing.
      const a = s.part;
      const outcome = a.status === "answered"
        ? bold(a.answer ?? "")
        : dim(a.status === "declined" ? "declined" : "interrupted");
      push(seg, `${warn("?")} ${a.question} ${dim("→")} ${outcome}`, inner);
      copy = `${a.question} → ${a.answer ?? a.status}`;
    } else {
      toolGroupLines(seg, s.parts, key, isExpanded(key), isFull(key), inner, toolLogs);
      copy = toolGroupCopy(s.parts);
    }
    for (const l of seg) body.push({ ...l, copy });
  });
  if (streaming) {
    const seg: VLine[] = [];
    push(seg, md(streaming) + "▌", inner);
    for (const l of seg) body.push({ ...l, copy: streaming });
  }
  out.push(...body.map((l) => (l.text ? { ...l, text: "  " + l.text } : l)));
  return out;
}

// ---- subagent branches ------------------------------------------------------

/** A subagent branch, anchored to the turn that spawned it. */
export interface Branch {
  id: string;
  title: string;
  busy: boolean;
  /** The branch's last turn status, so a finished blocking subagent with no note
   * still reads failed/interrupted rather than a blanket "✓ done". */
  status?: "done" | "error" | "interrupted" | "orphaned";
  /** The persisted delegation outcome — whether its TURN completed (spec §17). */
  ok?: boolean | null;
  /** The message whose turn spawned it — where the card is drawn. */
  originMessageId?: string | null;
  /** Parsed completion note once it finished. */
  note?: SubagentNote | null;
}

/** The card for a finished subagent: header, files, capped report, next action. */
function subagentNoteLines(out: VLine[], note: SubagentNote, w: number, full: boolean) {
  const open = `open:${note.sessionId}`;
  // Amber = stopped or orphaned (an infra restart, not the agent's fault); red is
  // reserved for a genuine failure. Four outcomes, four readings (plan T4.4).
  const halted = note.status.startsWith("ORPHANED") || note.status.startsWith("STOPPED");
  const dot = note.ok ? accent("◆") : halted ? warn("◆") : danger("◆");
  const statusTag = note.ok
    ? accent(note.status)
    : halted
    ? warn(note.status)
    : danger(note.status);
  const title = note.title.replace(/^subagent · /, "");
  out.push({ text: `${dot} ${bold(title)}  ${clip(statusTag, 200)}`, click: open });
  const fileNote = note.filesUnknown
    ? "changed files not reported"
    : note.files.length
    ? `${note.files.length} file${note.files.length === 1 ? "" : "s"} · ${note.files.join(", ")}`
    : "no file changes";
  push(out, dim(`  ${fileNote}`), w, open);
  if (note.report) {
    // Physical (post-wrap) lines are what floods the screen, so cap those.
    const physical = md(note.report).split("\n").flatMap((line) => wrap(line, w - 2));
    const shown = full ? physical : physical.slice(0, REPORT_LINES);
    for (const l of shown) {
      out.push({ text: `${dim("│")} ${l}`, click: `report:${note.sessionId}` });
    }
    if (physical.length > shown.length) {
      out.push({
        text: `${dim("│")} ${dim(`… +${physical.length - shown.length} more`)}`,
        click: `report:${note.sessionId}!full`,
      });
    }
  }
  // It shared this checkout, so there is nothing to merge — the single most
  // common wrong move after a delegated report is looking for the merge step.
  //
  // It used to say "click to open". Nothing in the TUI dispatches a click (App's
  // mouse handler is wheel-only), so the one instruction the transcript gave the
  // reader was an instruction to do something impossible. The `click` targets on
  // these lines are kept — they are what a wired pointer would use — but the row
  // names the key that works today.
  push(out, dim("  ↳ ^s opens it · its edits are already in this checkout"), w, open);
}

function branchCardLines(out: VLine[], b: Branch, w: number, isFull: (key: string) => boolean) {
  const inner = w - 2;
  const body: VLine[] = [];
  let copy: string;
  if (b.note) {
    subagentNoteLines(body, b.note, inner, isFull(`report:${b.note.sessionId}`));
    copy = b.note.report ?? b.note.title;
  } else {
    // A blocking subagent reports in-band and leaves no note, so its card reads
    // the session's own status. Blue = in flight, amber = stopped or orphaned,
    // red = failed.
    const { dot, tail } = b.busy
      ? { dot: info("◆"), tail: info(" ⋯ working") }
      : b.status === "orphaned"
      ? { dot: warn("◆"), tail: warn(" ◼ interrupted — the server restarted") }
      : b.status === "interrupted"
      ? { dot: warn("◆"), tail: warn(" ◼ interrupted") }
      : b.status === "error" || b.ok === false
      ? { dot: danger("◆"), tail: danger(" ✗ failed") }
      : { dot: accent("◆"), tail: accent(" ✓ done") };
    body.push({
      text: `${dot} ${b.title.replace(/^subagent · /, "")}${dim(tail)}`,
      click: `open:${b.id}`,
    });
    copy = b.title;
  }
  out.push({ text: "" });
  out.push(...body.map((l) => (l.text ? { ...l, copy, text: "  " + l.text } : { ...l, copy })));
}

// ---- background shells ------------------------------------------------------

/**
 * A background shell as the transcript shows it: the wire row (schema/parts.ts)
 * plus what only the UI fetched — a tail of its buffer and the total line count.
 */
export interface JobView extends BackgroundJob {
  tail?: string[];
  outputLines?: number;
}

/** Two most significant units only; nobody needs seconds on a two-day run. */
function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
}

function jobStatusText(job: JobView): string {
  if (job.status === "running") return warn("⋯ running");
  return (job.exitCode ?? 0) === 0 ? accent("✓ done") : danger(`✗ exit ${job.exitCode}`);
}

/**
 * A background shell's card, kept after the exit. It used to erase itself and
 * leave only a note written for the model, so a build that failed while you were
 * reading something else left no user-visible trace of having failed at all.
 */
export function jobCardLines(out: VLine[], job: JobView, w: number, now: number) {
  const inner = w - 2;
  const body: VLine[] = [];
  const glyph = job.status === "running"
    ? warn("⚙")
    : (job.exitCode ?? 0) === 0
    ? accent("⚙")
    : danger("⚙");
  const took = fmtElapsed((job.exitedAt ?? now) - job.startedAt);
  body.push({
    text: `${glyph} ${bold(job.name || job.id)} ${jobStatusText(job)}  ${
      dim(`${job.name ? `${job.id} · ` : ""}${clip(job.command, 60)} · ${took}`)
    }`,
  });
  for (const line of job.tail ?? []) {
    for (const l of wrap(line, inner - 2)) body.push({ text: `${dim("│")} ${dim(l)}` });
  }
  const total = job.outputLines ?? 0;
  if (total > (job.tail?.length ?? 0)) {
    body.push({ text: dim(`  ${total} lines total`) });
  }
  const copy = [`${job.id} · ${job.command}`, ...(job.tail ?? [])].join("\n");
  out.push({ text: "" });
  // Clicking the card OPENS the job. The target used to be the bare word `jobs`,
  // which no handler knew, so it fell through to the fold toggle and a click on a
  // finished build did nothing at all — and an exited job is off the rail, so the
  // card is the only door left to its output.
  const click = `job:${job.sessionId}:${job.id}`;
  out.push(...body.map((l) => ({ ...l, copy, click, text: "  " + l.text })));
}

// ---- the whole transcript ---------------------------------------------------

export interface BuildOptions {
  streaming?: Record<string, string>;
  branches?: Branch[];
  toolLogs?: Record<string, string[]>;
  jobs?: JobView[];
  /**
   * The session's permanent ledger (`store.ts`), oldest first.
   *
   * Two things the transcript used to forget the moment they happened: what a turn
   * cost (the spinner's numbers vanished with the spinner) and what was destroyed (a
   * revert printed a notice that expired ten seconds later, leaving no record
   * anywhere that a file had been thrown away). Both are written as marks and both
   * are interleaved HERE rather than pushed into `thread`, because `mergeThread`
   * appends local-only messages at the end and a mark would drift off its position
   * as soon as the next turn's messages landed.
   */
  marks?: TranscriptMark[];
  /** Injected clock — elapsed times are the only thing here that needs one. */
  now?: number;
}

/**
 * One mark as one row: two columns in, like a message body, so it reads as part of
 * the conversation rather than as chrome. A destructive mark is amber — it is the
 * only row in the transcript that reports something the user cannot get back.
 */
function markLine(mark: TranscriptMark): VLine {
  return {
    text: "  " + (mark.kind === "destructive" ? warn(mark.text) : dim(mark.text)),
    copy: mark.text,
  };
}

export function buildLines(
  thread: Message[],
  isExpanded: (key: string) => boolean,
  isFull: (key: string) => boolean,
  w: number,
  opts: BuildOptions = {},
): VLine[] {
  const streaming = opts.streaming ?? {};
  const branches = opts.branches ?? [];
  const jobs = opts.jobs ?? [];
  const now = opts.now ?? Date.now();
  // A note that already renders as a card is dropped from the raw thread, and so
  // is a job wake note while its card is showing. Once a job ages out of the
  // registry the note is all that is left — keep it then.
  const notedIds = new Set(branches.map((b) => b.note?.sessionId).filter(Boolean));
  const jobIds = new Set(jobs.map((j) => j.id));
  const byOrigin = new Map<string, Branch[]>();
  const orphans: Branch[] = [];
  for (const b of branches) {
    // A running subagent lives in the pinned rail, not in a card that scrolls out
    // of view; the transcript keeps its finished report.
    if (b.busy && !b.note) continue;
    if (b.originMessageId) {
      byOrigin.set(b.originMessageId, [...(byOrigin.get(b.originMessageId) ?? []), b]);
    } else orphans.push(b);
  }
  const out: VLine[] = [];
  // The ledger, drained in step with the thread: every mark older than the message
  // about to be pushed is flushed before it, so a settled-turn line lands under the
  // turn it measured and a revert lands where the conversation was when it happened.
  const marks = [...(opts.marks ?? [])].sort((a, b) => a.at - b.at);
  let markAt = 0;
  const flushMarks = (until: number) => {
    while (markAt < marks.length && marks[markAt].at <= until) out.push(markLine(marks[markAt++]));
  };
  // True once a pending reply has rendered: any user message after it was posted
  // into a running turn and is only QUEUED server-side (spec §5).
  let midTurn = false;
  for (const m of thread) {
    if (m.role === "system") {
      const t = m.parts.filter((p) => p.type === "text")
        .map((p) => (p as { text: string }).text).join("\n");
      const parsed = parseSubagentNote(t);
      if (parsed && notedIds.has(parsed.sessionId)) continue;
      const bg = parseBgNote(t);
      if (bg && jobIds.has(bg)) continue;
    }
    flushMarks(m.createdAt);
    out.push(...messageLines(m, isExpanded, isFull, w, streaming[m.id], opts.toolLogs));
    // An honest ack under a steered message: the turn drains it at the next round
    // boundary, and a blocking host call can hold that off for minutes — silence
    // reads as being ignored.
    if (midTurn && m.role === "user") {
      out.push({ text: "  " + dim("⧖ queued — the agent sees this after the current step") });
    }
    if (m.pending) midTurn = true;
    for (const b of byOrigin.get(m.id) ?? []) branchCardLines(out, b, w, isFull);
    byOrigin.delete(m.id);
  }
  // Everything that happened after the last message — the turn that just settled,
  // the file reverted while nothing was being said.
  flushMarks(Infinity);
  // Anything left is anchored to a message not in this thread (a fork or
  // compaction dropped the spawn turn). Those cards would render nowhere at all.
  const tail = [...orphans, ...[...byOrigin.values()].flat()];
  if (tail.length) {
    out.push({ text: "" });
    out.push({ text: "  " + dim("subagents with no spawn point in this thread") });
  }
  for (const b of tail) branchCardLines(out, b, w, isFull);
  // Background shells at the tail — including the finished ones: the outcome is
  // the whole point, and dropping exited jobs put a failure nowhere at all.
  for (const job of jobs) jobCardLines(out, job, w, now);
  return out;
}

/**
 * The slice a viewport of `height` rows shows, `scrollOff` lines up from the live
 * tail. `more` is what remains below — the indicator that keeps a scrolled-up
 * reader from mistaking an old frame for the current one.
 */
/**
 * Rows the transcript body occupies inside the chat's TOTAL height.
 *
 * Lives here rather than in `Chat` because two callers need the same number and
 * they must not each carry their own copy: `Chat` lays the rows out, and the
 * composition root hit-tests a mouse click against them. A click that resolves one
 * row off is worse than a click that does nothing, so the arithmetic is defined
 * once and tested directly.
 */
export function chatBodyHeight(height: number, queued: number, hasNotice: boolean): number {
  return Math.max(1, height - (queued + 2 + (hasNotice ? 1 : 0)));
}

/**
 * The transcript line under a screen slot, counting from the top of the chat body.
 *
 * The exact inverse of `Chat`'s row loop, including the pad: a short conversation
 * hangs from the BOTTOM, so the first `body - rows.length` slots are empty air and
 * resolve to null rather than to line zero.
 */
export function lineAtSlot(
  lines: VLine[],
  body: number,
  scrollOff: number,
  slot: number,
): VLine | null {
  const { start, rows } = visibleSlice(lines, body, scrollOff);
  const pad = Math.max(0, Math.max(1, body) - rows.length);
  const i = slot - pad;
  return i >= 0 && i < rows.length ? rows[i] ?? null : null;
}

export function visibleSlice(
  lines: VLine[],
  height: number,
  scrollOff: number,
): { start: number; rows: VLine[]; more: number; pct: number } {
  const h = Math.max(1, height);
  const maxOff = Math.max(0, lines.length - h);
  const off = Math.max(0, Math.min(scrollOff, maxOff));
  const start = Math.max(0, lines.length - h - off);
  return {
    start,
    rows: lines.slice(start, start + h),
    more: off,
    pct: maxOff === 0 ? 100 : Math.round((start / maxOff) * 100),
  };
}
