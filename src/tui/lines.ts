// The conversation as a flat list of pre-wrapped visual lines — the viewport
// slices these for rendering, scrolling is an index offset, and a mouse click
// maps (row → line → click key). Replaces the old Static-seal architecture so
// any tool group, however old, can be expanded in place.
import wrapAnsi from "wrap-ansi";
import type { Message, Role } from "../schema/parts.ts";
import { COLOR, highlightCode, md, outputText, segmentParts, toolSummary } from "./format.ts";

export interface VLine {
  text: string;
  /** Click target: a tool-group key toggles its fold; an "open:<sessionId>" key
   * descends into that subagent's branch. */
  click?: string;
}

const SGR = (n: number | string, s: string) => (COLOR ? `\x1b[${n}m${s}\x1b[0m` : s);
const bold = (s: string) => SGR(1, s);
const dim = (s: string) => SGR(2, s);
const cyan = (s: string) => SGR(36, s);
const green = (s: string) => SGR(32, s);
const yellow = (s: string) => SGR(33, s);
const red = (s: string) => SGR(31, s);

// One accent: green is bough's color (identity + affirmative status); the user
// speaks in plain bright text, cyan is reserved for code.
const ROLE_LABEL: Record<Role, string> = {
  user: bold("you"),
  supervisor: bold(green("bough")),
  worker: dim("worker"),
  system: bold(yellow("system")),
};

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
  return { title, sessionId, status, ok: !status.startsWith("FAILED"), files, report };
}

// The card for a finished subagent: a clickable ◆ header (opens its branch),
// a files line, the report as markdown, and a dim footer with the next action.
function subagentNoteLines(out: VLine[], note: SubagentNote, width: number) {
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
    for (const line of md(note.report).split("\n")) {
      for (const l of wrap(line, width - 2)) out.push({ text: `${dim("│")} ${l}` });
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

function toolGroupLines(
  out: VLine[],
  parts: Message["parts"],
  key: string,
  expanded: boolean,
  full: boolean,
  width: number,
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
    }
  }
}

export function messageLines(
  msg: Message,
  isExpanded: (key: string) => boolean,
  isFull: (key: string) => boolean,
  width: number,
  streaming?: string,
): VLine[] {
  const out: VLine[] = [];
  const body: VLine[] = [];
  const w = width - 2;
  out.push({ text: "" });
  out.push({ text: ROLE_LABEL[msg.role] });
  // Bodies hang 2 columns under the role label so turns read as blocks.
  segmentParts(msg.parts).forEach((s, i) => {
    const key = `${msg.id}:${i}`;
    if (s.kind === "text") push(body, md(s.text), w);
    else if (s.kind === "reasoning") push(body, dim(s.text), w);
    else toolGroupLines(body, s.parts, key, isExpanded(key), isFull(key), w);
  });
  if (streaming) push(body, md(streaming) + "▌", w);
  out.push(...body.map((l) => (l.text ? { ...l, text: "  " + l.text } : l)));
  return out;
}

/** A subagent branch, anchored to the turn that spawned it. */
export interface Branch {
  id: string;
  title: string;
  busy: boolean;
  /** The assistant message whose turn called spawn — where the card is drawn. */
  originMessageId?: string | null;
  /** Parsed completion note once the subagent finished (report/files/status). */
  note?: SubagentNote | null;
}

// One branch's card: a live ⋯/✓ line, or the full finished card (header, files,
// report as markdown, footer). Indented under the spawning turn.
function branchCardLines(out: VLine[], b: Branch, width: number) {
  const w = width - 2;
  const body: VLine[] = [];
  if (b.note) subagentNoteLines(body, b.note, w);
  else {
    const dot = b.busy ? yellow("◆") : green("◆");
    const tail = b.busy ? yellow(" ⋯ working") : green(" ✓ done");
    body.push({
      text: `${dot} ${b.title.replace(/^subagent · /, "")}${dim(tail)}`,
      click: `open:${b.id}`,
    });
  }
  out.push({ text: "" });
  out.push(...body.map((l) => (l.text ? { ...l, text: "  " + l.text } : l)));
}

export function buildLines(
  thread: Message[],
  streaming: Record<string, string>,
  isExpanded: (key: string) => boolean,
  isFull: (key: string) => boolean,
  width: number,
  branches: Branch[] = [],
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
    out.push(...messageLines(m, isExpanded, isFull, width, streaming[m.id]));
    for (const b of byOrigin.get(m.id) ?? []) branchCardLines(out, b, width);
  }
  // Branches whose spawn point isn't in the current thread fall to the tail.
  for (const b of orphans) branchCardLines(out, b, width);
  return out;
}
