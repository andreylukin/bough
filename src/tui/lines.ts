// The conversation as a flat list of pre-wrapped visual lines — the viewport
// slices these for rendering, scrolling is an index offset, and a mouse click
// maps (row → line → click key). Replaces the old Static-seal architecture so
// any tool group, however old, can be expanded in place.
import wrapAnsi from "wrap-ansi";
import type { Message, Role } from "../schema/parts.ts";
import { md, outputText, segmentParts, toolSummary } from "./format.ts";

export interface VLine {
  text: string;
  /** Set on expandable lines: clicking (or ^e-ing) toggles this key. */
  click?: string;
}

const SGR = (n: number | string, s: string) => `\x1b[${n}m${s}\x1b[0m`;
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
 * collapses it). `style` colors the content; the gutter stays dim.
 */
function pushBlock(
  out: VLine[],
  text: string,
  width: number,
  opts: { maxLines: number; style: (l: string) => string; click: string },
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
      text: `${dim("│")} ${dim(`… +${logical.length - shown.length} more lines`)}`,
      click: opts.click,
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
  width: number,
) {
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
      pushBlock(out, input, width, { maxLines: CODE_LINES, style: (l) => l, click: key });
    }
    if (res && outputText(res) !== "") {
      out.push({ text: dim("↳ output"), click: key });
      pushBlock(out, outputText(res), width, {
        maxLines: OUTPUT_LINES,
        style: (l) => styleOutputLine(l, res.isError),
        click: key,
      });
    }
  }
}

export function messageLines(
  msg: Message,
  isExpanded: (key: string) => boolean,
  width: number,
  streaming?: string,
): VLine[] {
  const out: VLine[] = [];
  out.push({ text: "" });
  out.push({ text: ROLE_LABEL[msg.role] });
  // Bodies hang 2 columns under the role label so turns read as blocks.
  const body: VLine[] = [];
  const w = width - 2;
  segmentParts(msg.parts).forEach((s, i) => {
    if (s.kind === "text") push(body, md(s.text), w);
    else if (s.kind === "reasoning") push(body, dim(s.text), w);
    else toolGroupLines(body, s.parts, `${msg.id}:${i}`, isExpanded(`${msg.id}:${i}`), w);
  });
  if (streaming) push(body, md(streaming) + "▌", w);
  out.push(...body.map((l) => (l.text ? { ...l, text: "  " + l.text } : l)));
  return out;
}

export function buildLines(
  thread: Message[],
  streaming: Record<string, string>,
  isExpanded: (key: string) => boolean,
  width: number,
): VLine[] {
  const out: VLine[] = [];
  for (const m of thread) out.push(...messageLines(m, isExpanded, width, streaming[m.id]));
  return out;
}
