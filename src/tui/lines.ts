// The conversation as a flat list of pre-wrapped visual lines — the viewport
// slices these for rendering, scrolling is an index offset, and a mouse click
// maps (row → line → click key). Replaces the old Static-seal architecture so
// any tool group, however old, can be expanded in place.
import wrapAnsi from "wrap-ansi";
import type { Message, Role } from "../schema/parts.ts";
import { clip, COLOR, highlightCode, md, outputText, segmentParts, toolSummary } from "./format.ts";
import { fgParams, palette } from "./theme.ts";

export interface VLine {
  text: string;
  /** Click target: a tool-group key toggles its fold; an "open:<sessionId>" key
   * descends into that subagent's branch. */
  click?: string;
  /** The raw, unstyled, unwrapped text of the section this line belongs to; a
   * right-click copies it. */
  copy?: string;
}

const SGR = (n: number | string, s: string) => (COLOR ? `\x1b[${n}m${s}\x1b[0m` : s);
const bold = (s: string) => SGR(1, s);
const dim = (s: string) => SGR(2, s);
// Hue helpers read the live theme palette (truecolor) — evaluated per call, so
// an applied theme recolors rebuilt lines without a restart.
const green = (s: string) => SGR(fgParams(palette.accent), s);
const yellow = (s: string) => SGR(fgParams(palette.warn), s);
const red = (s: string) => SGR(fgParams(palette.error), s);

// One accent: green is bough's color (identity + affirmative status); the user
// speaks in plain bright text. A function (not a const map) so labels pick up
// the palette active at render time.
const roleLabel = (role: Role): string =>
  role === "user"
    ? bold("you")
    : role === "supervisor"
    ? bold(green("bough"))
    : role === "worker"
    ? dim("worker")
    : bold(yellow("system"));

function wrap(text: string, width: number): string[] {
  return wrapAnsi(text, Math.max(20, width), { hard: true, trim: false }).split("\n");
}

// A subagent's completion note (subagent.ts formatNote) replays as a system
// message so the model can act on it, but the raw bracketed wall is noise to a
// human. Parse it back into fields so we can render a real card.
export interface SubagentNote {
  title: string;
  sessionId: string;
  status: string;
  ok: boolean;
  files: string[];
  report: string | null;
}

export function parseSubagentNote(text: string): SubagentNote | null {
  const head = text.match(/^\[subagent finished\] "(.*)" \(([^)]+)\) — (.+)\.$/m);
  if (!head) return null;
  const [, title, sessionId, status] = head;
  const filesLine = text.match(/^Changed files on its branch: (.+)\.$/m);
  const files = filesLine && filesLine[1] !== "none"
    ? filesLine[1].split(", ").map((f) => f.trim())
    : [];
  const reportMatch = text.match(/^Report:\n([\s\S]*?)\nIts changes stay on its own branch/m);
  const report = reportMatch ? reportMatch[1].trim() : null;
  // Only a "finished…" note is a success; FAILED / STOPPED / ORPHANED are not.
  return { title, sessionId, status, ok: status.startsWith("finished"), files, report };
}

// How many report lines a finished-subagent card shows before "+N more"; the
// full report is one click away (its own toggle key). Keeps a chatty subagent
// from burying the conversation it reported into (the card is a summary, not the
// subagent's transcript — that lives one `open` away).
const REPORT_LINES = 6;

// The card for a finished subagent: a clickable ◆ header (opens its branch), a
// files line, a capped report preview (expand toggle), and a footer with the
// next action. `full` lifts the report cap (set by clicking its "+N more" line).
function subagentNoteLines(out: VLine[], note: SubagentNote, width: number, full: boolean) {
  const open = `open:${note.sessionId}`;
  const dot = note.ok ? green("◆") : red("◆");
  const statusTag = note.ok ? green(note.status) : red(note.status);
  out.push({ text: `${dot} ${bold(note.title)}  ${statusTag}`, click: open });
  const fileNote = note.files.length
    ? `${note.files.length} file${note.files.length === 1 ? "" : "s"} on its branch · ${
      note.files.join(", ")
    }`
    : "no file changes";
  push(out, dim(`  ${fileNote}`), width, open);
  if (note.report) {
    // The rendered report can be long; cap it like a tool-output block so a
    // finished subagent stays a compact card. Physical (post-wrap) lines are
    // what floods the screen, so cap those, not logical lines.
    const physical = md(note.report).split("\n").flatMap((line) => wrap(line, width - 2));
    const shown = full ? physical : physical.slice(0, REPORT_LINES);
    for (const l of shown) {
      out.push({ text: `${dim("│")} ${l}`, click: `report:${note.sessionId}` });
    }
    if (physical.length > shown.length) {
      out.push({
        text: `${dim("│")} ${dim(`… +${physical.length - shown.length} more · click to show all`)}`,
        click: `report:${note.sessionId}!full`,
      });
    }
  }
  push(
    out,
    dim(`  ↳ enter/click to open · adopt("…") in a turn to merge its changes`),
    width,
    open,
  );
}

function push(out: VLine[], text: string, width: number, click?: string) {
  for (const l of wrap(text, width)) out.push(click ? { text: l, click } : { text: l });
}

// How much of an expanded call shows before "… +N more lines" (logical lines,
// before wrapping; a runaway single line is still tamed by the hard wrap).
const CODE_LINES = 14;
const OUTPUT_LINES = 20;

/**
 * A gutter-framed block: each logical line wraps to the remaining width and
 * every physical line carries a dim `│` (clickable — anywhere in the block
 * collapses it). `style` colors the content; the gutter stays dim. A truncated
 * block ends on a "+N more · click to show all" line whose click target is
 * `fullKey` — toggling it re-renders the block uncapped.
 */
function pushBlock(
  out: VLine[],
  text: string,
  width: number,
  opts: { maxLines: number; style: (l: string) => string; click: string; fullKey?: string },
) {
  const logical = text.split("\n");
  const shown = logical.slice(0, opts.maxLines);
  for (const line of shown) {
    for (const l of wrap(line, width - 2)) {
      out.push({ text: `${dim("│")} ${l ? opts.style(l) : ""}`, click: opts.click });
    }
  }
  if (logical.length > shown.length) {
    out.push({
      text: `${dim("│")} ${
        dim(`… +${logical.length - shown.length} more lines · click to show all`)
      }`,
      click: opts.fullKey ?? opts.click,
    });
  }
}

// Harness verdict lines inside run_steps output get their own colors; everything
// else in an output block reads dim (it's the result, not the intent).
function styleOutputLine(line: string, isError: boolean): string {
  if (isError) return red(line);
  if (line.startsWith("[done] accepted")) return green(line);
  if (line.startsWith("[done] rejected")) return red(line);
  if (line.startsWith("[check]")) return yellow(line);
  return dim(line);
}

// One-line excerpt of a call's input for the collapsed fold header: the first
// meaningful code line (or compact JSON). A bare tool name ("run_steps") tells
// the reader nothing about what ran; the scrollback is the session's record.
function inputGist(call: { input?: unknown }): string {
  const raw = call.input as Record<string, unknown> | null | undefined;
  const code = raw && typeof raw.code === "string" ? raw.code : null;
  const src = code ?? (call.input === undefined ? "" : JSON.stringify(call.input));
  const line = src.trim().split("\n").map((l) => l.trim())
    .find((l) => l.length > 0 && !l.startsWith("//")) ?? "";
  return clip(line, 60);
}

function toolGroupLines(
  out: VLine[],
  parts: Message["parts"],
  key: string,
  expanded: boolean,
  full: boolean,
  width: number,
  toolLogs?: Record<string, string[]>,
) {
  // `full` lifts the per-block line caps (set by clicking a "+N more" line; its
  // toggle key is `${key}!full`, kept separate from the fold state so ^e
  // expand-all doesn't dump every 200-line output into the viewport).
  const capCode = full ? Infinity : CODE_LINES;
  const capOut = full ? Infinity : OUTPUT_LINES;
  const { calls, results, running, verdict, hasError } = toolSummary(parts);
  if (calls.length === 0) return;
  let head = dim(
    `${expanded ? "▾" : "▸"} ${calls.length} tool ${calls.length === 1 ? "call" : "calls"}  ${
      calls.map((c) => c.name).join(" · ")
    }`,
  );
  // Collapsed single-call groups carry a gist of what ran (expanded shows the
  // real thing, multi-call headers are already crowded with names).
  if (!expanded && calls.length === 1) {
    const gist = inputGist(calls[0]);
    if (gist) head += dim(` · ${gist}`);
  }
  if (verdict) head += "  " + (verdict.ok ? green(verdict.text) : yellow(verdict.text));
  else if (hasError) head += "  " + red("✗ error");
  if (running) head += "  " + yellow(`⚙ ${running.name}…`);
  // The header is one clickable line — click toggles the fold (never wrapped so the
  // whole visual row stays one target; the terminal truncates overflow).
  out.push({ text: head, click: key });
  if (!expanded) return;
  for (const call of calls) {
    const res = results.get(call.id);
    const status = res ? (res.isError ? red("✗ error") : green("✓ done")) : yellow("⚙ running");
    push(out, `${green("◇")} ${call.name} ${status}`, width, key);
    // What ran, bright; what came back, dim — the brightness IS the boundary,
    // with an ↳ seam between the two.
    const raw = call.input as Record<string, unknown> | null | undefined;
    const code = raw && typeof raw.code === "string" ? raw.code : null;
    const input = code ?? (call.input === undefined ? "" : JSON.stringify(call.input, null, 2));
    if (input) {
      // run_steps code is harness JS; JSON inputs highlight fine as C-family.
      pushBlock(out, input, width, {
        maxLines: capCode,
        style: (l) => highlightCode(l, "js"),
        click: key,
        fullKey: `${key}!full`,
      });
    }
    if (res && outputText(res) !== "") {
      out.push({ text: dim("↳ output"), click: key });
      pushBlock(out, outputText(res), width, {
        maxLines: capOut,
        style: (l) => styleOutputLine(l, res.isError),
        click: key,
        fullKey: `${key}!full`,
      });
    } else {
      // Still running: show the program's console lines as they stream in
      // (tool.log events); the finalized tool_result replaces them with the
      // same lines joined into its output.
      const live = toolLogs?.[call.id];
      if (live?.length) {
        out.push({ text: dim("↳ output (live)"), click: key });
        pushBlock(out, live.join("\n"), width, {
          maxLines: capOut,
          style: (l) => styleOutputLine(l, false),
          click: key,
          fullKey: `${key}!full`,
        });
      }
    }
  }
}

// The whole tool group as plain text for right-click-to-copy: per call, a
// `◇ <name>` header over its raw input (the same string the renderer derives),
// then `↳ output` over the result when there is one. Calls join with a blank
// line. Computed once per group and stamped on every line the group renders.
function toolGroupCopy(parts: Message["parts"]): string {
  const { calls, results } = toolSummary(parts);
  return calls.map((call) => {
    const raw = call.input as Record<string, unknown> | null | undefined;
    const code = raw && typeof raw.code === "string" ? raw.code : null;
    const input = code ?? (call.input === undefined ? "" : JSON.stringify(call.input, null, 2));
    let block = `◇ ${call.name}\n${input}`;
    const res = results.get(call.id);
    if (res && outputText(res) !== "") block += `\n↳ output\n${outputText(res)}`;
    return block;
  }).join("\n\n");
}

export function messageLines(
  msg: Message,
  isExpanded: (key: string) => boolean,
  isFull: (key: string) => boolean,
  width: number,
  streaming?: string,
  toolLogs?: Record<string, string[]>,
): VLine[] {
  const out: VLine[] = [];
  const body: VLine[] = [];
  const w = width - 2;
  out.push({ text: "" });
  out.push({ text: roleLabel(msg.role) });
  // Bodies hang 2 columns under the role label so turns read as blocks. Each
  // segment's fresh lines are stamped with the section's raw text for copy.
  segmentParts(msg.parts).forEach((s, i) => {
    const key = `${msg.id}:${i}`;
    const seg: VLine[] = [];
    let copy: string;
    if (s.kind === "text") {
      push(seg, md(s.text), w);
      copy = s.text;
    } else if (s.kind === "reasoning") {
      // Thinking folds like a tool group: a long reasoning wall is process, not
      // answer. Collapsed = one clickable gist line; expanded = a gutter block
      // (capped like outputs, "+N more" lifts the cap via the !full key).
      // Empty reasoning (thinking happened, text not captured) renders nothing.
      if (!s.text.trim()) return;
      const logical = s.text.split("\n");
      if (isExpanded(key)) {
        seg.push({ text: dim(`▾ thinking (${logical.length} lines)`), click: key });
        pushBlock(seg, s.text, w, {
          maxLines: isFull(key) ? Infinity : OUTPUT_LINES,
          style: dim,
          click: key,
          fullKey: `${key}!full`,
        });
      } else {
        const gist = logical.map((l) => l.trim()).find((l) => l.length > 0) ?? "";
        seg.push({ text: dim(`▸ thinking · ${clip(gist, 60)}`), click: key });
      }
      copy = s.text;
    } else if (s.kind === "image") {
      // An attached image renders as a compact placeholder (terminals don't do
      // pixels); the bytes live in ~/.bough/attachments and went to the model.
      const kb = Math.max(1, Math.round(s.part.size / 1024));
      seg.push({ text: dim(`🖼 ${s.part.name} (${kb} KB)`) });
      copy = s.part.path;
    } else if (s.kind === "ask") {
      // A settled ask() Q/A — one always-visible line: the question, then how it
      // ended (chosen/typed answer, declined, or interrupted).
      const a = s.part;
      const outcome = a.status === "answered"
        ? bold(a.answer ?? "")
        : dim(a.status === "declined" ? "declined" : "interrupted");
      push(seg, `${yellow("?")} ${a.question} ${dim("→")} ${outcome}`, w);
      copy = `${a.question} → ${a.answer ?? a.status}`;
    } else {
      toolGroupLines(seg, s.parts, key, isExpanded(key), isFull(key), w, toolLogs);
      copy = toolGroupCopy(s.parts);
    }
    for (const l of seg) body.push({ ...l, copy });
  });
  if (streaming) {
    const seg: VLine[] = [];
    push(seg, md(streaming) + "▌", w);
    for (const l of seg) body.push({ ...l, copy: streaming });
  }
  out.push(...body.map((l) => (l.text ? { ...l, text: "  " + l.text } : l)));
  return out;
}

/** A subagent branch, anchored to the turn that spawned it. */
export interface Branch {
  id: string;
  title: string;
  busy: boolean;
  /** The subagent's last turn status, so a finished blocking subagent (no note)
   * still shows failed/interrupted rather than a blanket "✓ done". */
  status?: "done" | "error" | "interrupted" | "orphaned";
  /** The assistant message whose turn called spawn — where the card is drawn. */
  originMessageId?: string | null;
  /** Parsed completion note once the subagent finished (report/files/status). */
  note?: SubagentNote | null;
}

// One branch's card: a live ⋯/✓ line, or the finished card (header, files,
// capped report, footer). Indented under the spawning turn. `isFull` lifts the
// report cap when its "+N more" line was clicked.
function branchCardLines(
  out: VLine[],
  b: Branch,
  width: number,
  isFull: (key: string) => boolean,
) {
  const w = width - 2;
  const body: VLine[] = [];
  let copy: string;
  if (b.note) {
    subagentNoteLines(body, b.note, w, isFull(`report:${b.note.sessionId}`));
    copy = b.note.report ?? b.note.title;
  } else {
    // A finished blocking subagent has no completion note — reflect its real
    // outcome from the session status instead of always showing "✓ done".
    const { dot, tail } = b.busy
      ? { dot: yellow("◆"), tail: yellow(" ⋯ working") }
      : b.status === "error" || b.status === "orphaned"
      ? { dot: red("◆"), tail: red(" ✗ failed") }
      : b.status === "interrupted"
      ? { dot: yellow("◆"), tail: yellow(" ◼ interrupted") }
      : { dot: green("◆"), tail: green(" ✓ done") };
    body.push({
      text: `${dot} ${b.title.replace(/^subagent · /, "")}${dim(tail)}`,
      click: `open:${b.id}`,
    });
    copy = b.title;
  }
  out.push({ text: "" });
  out.push(
    ...body.map((l) => (l.text ? { ...l, copy, text: "  " + l.text } : { ...l, copy })),
  );
}

/** A background shell of the open session (GET /sessions/:id/jobs row). */
export interface BgJob {
  id: string;
  command: string;
  startedAt: number;
  status: "running" | "exited" | "killed";
  tailLines: string[];
}

function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

// A background shell's card: alive while it runs (⋯ marker + output tail,
// refreshed as the jobs poll lands), an honest ✗ once killed. Natural exits get
// no card — their completion note already lands in the transcript.
function jobCardLines(out: VLine[], job: BgJob, width: number) {
  const w = width - 2;
  const body: VLine[] = [];
  if (job.status === "running") {
    body.push({
      text: `${yellow("⚙")} ${bold(job.id)} ${yellow("⋯ running")}  ${
        dim(`${clip(job.command, 60)} · ${fmtElapsed(Date.now() - job.startedAt)}`)
      }`,
    });
    for (const line of job.tailLines) {
      for (const l of wrap(line, w - 2)) body.push({ text: `${dim("│")} ${dim(l)}` });
    }
  } else {
    body.push({
      text: `${red("⚙")} ${bold(job.id)} ${red("✗ killed")}  ${dim(clip(job.command, 60))}`,
    });
  }
  const copy = [`${job.id} · ${job.command}`, ...job.tailLines].join("\n");
  out.push({ text: "" });
  out.push(...body.map((l) => ({ ...l, copy, text: "  " + l.text })));
}

export function buildLines(
  thread: Message[],
  streaming: Record<string, string>,
  isExpanded: (key: string) => boolean,
  isFull: (key: string) => boolean,
  width: number,
  branches: Branch[] = [],
  toolLogs?: Record<string, string[]>,
  jobs: BgJob[] = [],
): VLine[] {
  // Branches draw under the turn that spawned them; a completion note that already
  // renders as a card is dropped from the raw thread (it's a system message).
  const notedIds = new Set(branches.map((b) => b.note?.sessionId).filter(Boolean));
  const byOrigin = new Map<string, Branch[]>();
  const orphans: Branch[] = [];
  for (const b of branches) {
    if (b.originMessageId) {
      byOrigin.set(b.originMessageId, [...(byOrigin.get(b.originMessageId) ?? []), b]);
    } else orphans.push(b);
  }
  const out: VLine[] = [];
  for (const m of thread) {
    // Skip the raw [subagent finished] system message — its card renders at the
    // spawn point instead.
    if (m.role === "system") {
      const t = m.parts.filter((p) => p.type === "text").map((p) => (p as { text: string }).text)
        .join("\n");
      const parsed = parseSubagentNote(t);
      if (parsed && notedIds.has(parsed.sessionId)) continue;
    }
    out.push(...messageLines(m, isExpanded, isFull, width, streaming[m.id], toolLogs));
    for (const b of byOrigin.get(m.id) ?? []) branchCardLines(out, b, width, isFull);
  }
  // Branches whose spawn point isn't in the current thread fall to the tail.
  for (const b of orphans) branchCardLines(out, b, width, isFull);
  // Background shells at the tail: running jobs look alive, killed ones honest.
  for (const job of jobs) {
    if (job.status !== "exited") jobCardLines(out, job, width);
  }
  return out;
}
