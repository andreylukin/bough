/**
 * The TUI's string layer: part folding, markdown-lite, ANSI-safe measurement.
 *
 * THE INVARIANT THIS HOLDS: **every function here is a pure function of strings and
 * data, correct with no terminal attached.** Nothing in this file reads the
 * terminal, mounts a component, or talks to the server — which is what lets the
 * folding rules, the wrap math and the ANSI truncation be tested directly on
 * strings (plan §7: "wrapping, ANSI width and selection math have direct unit
 * tests"). A helper that needed `process.stdout.columns` would have broken it.
 *
 * SECOND INVARIANT — **display width is never `String.length`.** Every measurement
 * goes through `string-width` and every slice through `slice-ansi`, because the
 * strings here carry SGR escapes and OSC 8 hyperlinks that occupy zero columns.
 * Counting characters would make a styled line "too long" and a wide-CJK line "too
 * short"; both bugs land as a garbled viewport rather than as an exception.
 *
 * THIRD — **color is a display setting, not a parameter of the data.** `colors` and
 * the NO_COLOR gate are module state on purpose: they change how a line is painted
 * and never what it says. Everything that decides *content* (which segment, which
 * fold, how many lines) is independent of them, so the folding tests below assert
 * on structure with color switched off and on the same way.
 *
 * Ported from `src/tui/format.ts`, minus two things that no longer exist: the
 * `prose` part kind (the union is frozen at six kinds — schema/parts.ts) and the
 * `[done] accepted/rejected` harness verdict (there is no acceptance gate — spec
 * §17). The theme palette is not imported: `theme.ts` belongs to a different task,
 * so the SGR parameters live in `colors` below and a theme installs itself with
 * `setColors` rather than this file reaching for one.
 */
import process from "node:process";
import stringWidth from "string-width";
import sliceAnsi from "slice-ansi";
import stripAnsi from "strip-ansi";
import wrapAnsi from "wrap-ansi";
import type { Part } from "../schema/parts.ts";
// Type-only, so this stays a leaf: `store.ts` imports the formatters below at
// runtime, and a value import back would be a cycle.
import type { LiveUnit } from "./store.ts";

export type ToolCall = Extract<Part, { type: "tool_call" }>;
export type ToolResult = Extract<Part, { type: "tool_result" }>;
export type ImagePart = Extract<Part, { type: "image" }>;
export type AskPart = Extract<Part, { type: "ask" }>;
export type WorkflowPart = Extract<Part, { type: "workflow" }>;

// ---- color state ------------------------------------------------------------

/**
 * Honor the NO_COLOR convention (https://no-color.org) for the hand-rolled SGR
 * paths — Ink's own `<Text color>` respects it via chalk, but these raw escapes
 * would otherwise leak styling into a colorless terminal. Read once; a test flips
 * it with `setColorEnabled` rather than mutating the environment.
 */
let COLOR = (process.env["NO_COLOR"] ?? "") === "";

export function colorEnabled(): boolean {
  return COLOR;
}

/** Returns the previous value, so a test can restore it in a `finally`. */
export function setColorEnabled(on: boolean): boolean {
  const was = COLOR;
  COLOR = on;
  return was;
}

/**
 * SGR *parameter bodies* (what goes between `\x1b[` and `m`), not whole escapes,
 * so a theme can swap 256-color indices for truecolor triples without this file
 * knowing which it got. De-emphasis is an explicit muted foreground rather than
 * the faint attribute: `\x1b[2m` is emulator-dependent and fails contrast on light
 * profiles.
 */
export interface ColorParams {
  muted: string;
  accent: string;
  warn: string;
  error: string;
  info: string;
  code: string;
  str: string;
  keyword: string;
  number: string;
  surfaceBg: string;
}

export const colors: ColorParams = {
  muted: "38;5;245",
  accent: "38;5;35",
  warn: "38;5;179",
  error: "38;5;167",
  info: "38;5;74",
  code: "38;5;80",
  str: "38;5;107",
  keyword: "38;5;140",
  number: "38;5;179",
  surfaceBg: "48;5;236",
};

/** How a theme installs itself without editing this module. */
export function setColors(next: Partial<ColorParams>): void {
  Object.assign(colors, next);
}

/** Ink `<Text color>` values for components. Names, not hex: no theme dependency. */
export const UI = {
  accent: "green",
  warn: "yellow",
  error: "red",
  info: "cyan",
  muted: "gray",
} as const;

const sgr = (
  params: string,
  s: string,
  off: string,
) => (COLOR ? `\x1b[${params}m${s}\x1b[${off}m` : s);

/**
 * Foreground spans close with `39m` and bold with `22m`, never a full `\x1b[0m`:
 * the themed `<Text color>` wrapping every viewport row re-opens only its own
 * close code (chalk), so a full reset would strip the base color for the rest of
 * the line.
 */
export const fg = (params: string, s: string) => sgr(params, s, "39");
export const bold = (s: string) => sgr("1", s, "22");
export const underline = (s: string) => sgr("4", s, "24");
/**
 * SGR 3, closed with 23. Terminals that do not support italics ignore the pair rather
 * than printing anything, which is why emphasis is safe to render at all: the worst case
 * is the text without its slant, never a stray escape on the screen.
 */
export const italic = (s: string) => sgr("3", s, "23");
/** SGR 9, closed with 29 — same deal as italics: unsupported terminals ignore the pair. */
export const strike = (s: string) => sgr("9", s, "29");
export const dim = (s: string) => fg(colors.muted, s);
export const accent = (s: string) => fg(colors.accent, s);
export const warn = (s: string) => fg(colors.warn, s);
export const danger = (s: string) => fg(colors.error, s);
export const info = (s: string) => fg(colors.info, s);

// ---- measurement ------------------------------------------------------------

/** Display columns, escapes excluded and wide characters counted as two. */
export function width(text: string): number {
  return stringWidth(text);
}

/**
 * Truncate to `max` display columns, keeping every SGR/OSC escape intact and
 * closed. Binary search over visible characters because one character is not one
 * column: a CJK glyph is two and an escape is zero, so a character-count slice
 * would overflow the row on the first wide glyph.
 */
export function truncateAnsi(text: string, max: number, ellipsis = ""): string {
  if (max <= 0) return "";
  if (stringWidth(text) <= max) return text;
  const tailW = stringWidth(ellipsis);
  if (tailW >= max) return "";
  const budget = max - tailW;
  let lo = 0;
  let hi = [...stripAnsi(text)].length;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (stringWidth(sliceAnsi(text, 0, mid)) <= budget) lo = mid;
    else hi = mid - 1;
  }
  return sliceAnsi(text, 0, lo) + ellipsis;
}

/**
 * Hard-wrap one logical line to `max` columns. `hard` splits a word longer than
 * the width (a URL, a minified line) instead of letting it overhang; `trim: false`
 * keeps leading indentation, which is load-bearing for code blocks.
 */
export function wrapLine(text: string, max: number): string[] {
  return wrapAnsi(text, Math.max(MIN_WRAP, max), { hard: true, trim: false }).split("\n");
}

/** Below this a wrap produces one column of letters; clamp instead. */
export const MIN_WRAP = 20;

// ---- escapes back into structure --------------------------------------------

/**
 * One run of text with a single style. The unit `Message.tsx` hands the renderer.
 *
 * WHY THIS EXISTS. Everything above builds strings with SGR escapes INSIDE them,
 * which is the right shape for measuring, wrapping, truncating and copying — and
 * the wrong shape for OpenTUI. A `<text>` whose child string carries raw escapes
 * paints correctly on the frame it is first drawn and then desynchronises: the
 * renderer's cell diff and the escape run stop agreeing about which column is
 * which, so an updated row repaints only part of itself and the rest of the old
 * row shows through — including, when the overwrite lands mid-escape, the tail of
 * the sequence as literal text (`78;201;143mbough`). It is reproducible in a
 * twenty-line OpenTUI app: the same content renders perfectly as chunks and
 * corrupts as an escaped string.
 *
 * So the escapes are parsed back out at the last possible moment and handed over
 * as structure. `lines.ts` and every string helper here are unchanged; only the
 * boundary with the renderer is.
 */
export interface AnsiSpan {
  text: string;
  /** `#rrggbb`, resolved from either truecolor or 256-color params. */
  fg?: string;
  bg?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
  reverse?: boolean;
  strikethrough?: boolean;
  /** OSC 8 target — the run is a hyperlink. */
  link?: string;
}

/** The 6-value ramp the xterm cube is built from. */
const CUBE = [0, 95, 135, 175, 215, 255];
/** xterm's first sixteen, which are palette-defined and have no formula. */
const BASE16 = [
  "#000000",
  "#cd0000",
  "#00cd00",
  "#cdcd00",
  "#0000ee",
  "#cd00cd",
  "#00cdcd",
  "#e5e5e5",
  "#7f7f7f",
  "#ff0000",
  "#00ff00",
  "#ffff00",
  "#5c5cff",
  "#ff00ff",
  "#00ffff",
  "#ffffff",
];

const hex = (r: number, g: number, b: number) =>
  `#${[r, g, b].map((c) => Math.max(0, Math.min(255, c)).toString(16).padStart(2, "0")).join("")}`;

/** 256-color index → hex. 0–15 from the table, 16–231 the cube, 232–255 the ramp. */
function xterm256(n: number): string {
  if (n < 16) return BASE16[n] ?? "#ffffff";
  if (n < 232) {
    const i = n - 16;
    return hex(CUBE[Math.floor(i / 36) % 6], CUBE[Math.floor(i / 6) % 6], CUBE[i % 6]);
  }
  const v = 8 + (n - 232) * 10;
  return hex(v, v, v);
}

/**
 * Read one SGR parameter list into `style`, returning how many extra parameters
 * the code at `i` consumed. Only the codes this file and `theme.ts` actually
 * emit are honoured; an unknown parameter is skipped rather than guessed at.
 */
function applySgr(style: AnsiSpan, ps: number[], i: number): number {
  const p = ps[i];
  if (p === 0) {
    const cleared: AnsiSpan = { text: style.text, link: style.link };
    // A full reset clears colour and attributes; an OSC 8 link is not SGR state
    // and survives one, which is what keeps a bolded URL clickable to its end.
    if (!style.link) delete cleared.link;
    Object.assign(style, cleared);
    for (const k of ["fg", "bg"] as const) if (!cleared[k]) delete style[k];
    delete style.bold;
    delete style.dim;
    delete style.italic;
    delete style.underline;
    delete style.reverse;
    delete style.strikethrough;
    return 0;
  }
  if (p === 1) style.bold = true;
  else if (p === 2) style.dim = true;
  else if (p === 3) style.italic = true;
  else if (p === 4) style.underline = true;
  else if (p === 7) style.reverse = true;
  else if (p === 9) style.strikethrough = true;
  else if (p === 22) {
    delete style.bold;
    delete style.dim;
  } else if (p === 23) delete style.italic;
  else if (p === 24) delete style.underline;
  else if (p === 27) delete style.reverse;
  else if (p === 29) delete style.strikethrough;
  else if (p === 39) delete style.fg;
  else if (p === 49) delete style.bg;
  else if (p >= 30 && p <= 37) style.fg = BASE16[p - 30];
  else if (p >= 90 && p <= 97) style.fg = BASE16[p - 90 + 8];
  else if (p >= 40 && p <= 47) style.bg = BASE16[p - 40];
  else if (p >= 100 && p <= 107) style.bg = BASE16[p - 100 + 8];
  else if (p === 38 || p === 48) {
    const key = p === 38 ? "fg" : "bg";
    if (ps[i + 1] === 5) {
      style[key] = xterm256(ps[i + 2] ?? 0);
      return 2;
    }
    if (ps[i + 1] === 2) {
      style[key] = hex(ps[i + 2] ?? 0, ps[i + 3] ?? 0, ps[i + 4] ?? 0);
      return 4;
    }
  }
  return 0;
}

// Anything that is not a printable run: an SGR sequence, an OSC 8 open or close
// (both string terminators), or any other CSI, which is dropped.
// deno-lint-ignore no-control-regex -- parsing escapes is the point.
const ESCAPES = /\x1b\[([0-9;]*)m|\x1b\]8;;([^\x07\x1b]*)(?:\x07|\x1b\\)|\x1b\[[0-9;?]*[A-Za-z]/g;

/**
 * Split a styled string into runs. Every escape is consumed; the returned spans
 * concatenate to exactly `stripAnsi(text)`, which is what makes the rendered row
 * the same width as the measured one.
 */
export function ansiSpans(text: string): AnsiSpan[] {
  const out: AnsiSpan[] = [];
  const style: AnsiSpan = { text: "" };
  let last = 0;
  const push = (chunk: string) => {
    if (!chunk) return;
    const prev = out[out.length - 1];
    // Adjacent runs that agree on style are one run: fewer chunks is less work for
    // the renderer, and a style that never changed must not look like it did.
    if (prev && sameStyle(prev, style)) prev.text += chunk;
    else out.push({ ...style, text: chunk });
  };
  ESCAPES.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = ESCAPES.exec(text)) !== null) {
    push(text.slice(last, m.index));
    last = m.index + m[0].length;
    if (m[1] !== undefined) {
      const ps = m[1] === "" ? [0] : m[1].split(";").map((p) => (p === "" ? 0 : Number(p)));
      for (let i = 0; i < ps.length; i++) i += applySgr(style, ps, i);
    } else if (m[2] !== undefined) {
      if (m[2]) style.link = m[2];
      else delete style.link;
    }
  }
  push(text.slice(last));
  return out;
}

function sameStyle(a: AnsiSpan, b: AnsiSpan): boolean {
  return a.fg === b.fg && a.bg === b.bg && a.bold === b.bold && a.dim === b.dim &&
    a.italic === b.italic && a.underline === b.underline && a.reverse === b.reverse &&
    a.strikethrough === b.strikethrough && a.link === b.link;
}

// ---- text helpers -----------------------------------------------------------

export function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}

/**
 * Text forced onto ONE row: newlines, tabs and stray control bytes collapse to
 * single spaces.
 *
 * A surface that reserves N rows for N items and then paints a string containing a
 * newline does not merely look wrong — every row below it is off by one. That is
 * exactly what a multi-line shell command did to the live-work rail: `App` sizes
 * the transcript as `rows - railH - …` with `railH = units.length`, so a two-line
 * command pushed the composer and the status line off their rows and the frame came
 * apart. Anything that lands in a fixed-height row goes through here first.
 *
 * The `¶` is deliberate rather than a plain space: `git commit -m "one" && push`
 * across two lines is not the same command as one with a space there, and a reader
 * comparing a rail row to their scrollback should be able to see the join.
 */
export function oneLine(s: string): string {
  // deno-lint-ignore no-control-regex
  return s.replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, " ")
    .replace(/\s*\r?\n\s*/g, " ¶ ")
    .replace(/\t/g, " ")
    .replace(/ {2,}/g, " ")
    .trim();
}

/**
 * One-line excerpt of a tool call's input: the first meaningful code line, or
 * compact JSON. A bare tool name ("run_steps") tells the reader nothing about what
 * ran, and the transcript's collapsed fold is the only place most programs are
 * ever seen — so the gist is what makes a folded step readable.
 */
export function codeGist(input: unknown, max = 60): string {
  const raw = input as Record<string, unknown> | null | undefined;
  const code = raw && typeof raw.code === "string" ? raw.code : null;
  const src = code ?? (input === undefined ? "" : JSON.stringify(input));
  const line = src.trim().split("\n").map((l) => l.trim())
    .find((l) => l.length > 0 && !l.startsWith("//")) ?? "";
  return clip(line, max);
}

/**
 * `3 agents`, `1 agent`. One place, because eight hand-written ternaries is eight
 * chances to render `1 tool calls` — which is what the run view did.
 */
export function plural(n: number, word: string, plural = `${word}s`): string {
  return `${n} ${n === 1 ? word : plural}`;
}

/**
 * A legend row that DEGRADES instead of being cut off.
 *
 * Every tab's footer is a `·`-joined list of keys rendered `wrapMode="none"`, so at a
 * narrow width the terminal cut it mid-word and the keys at the END simply vanished —
 * `↑↓ move · → focus one file · x revert this path · X revert all · e`. Those are the
 * discoverability rows: the whole reason the footer exists is that a key nobody can see
 * is a key nobody presses, and the failure hit the last item hardest.
 *
 * Items are dropped from the middle — the list is ordered by importance, so the tail
 * goes first — and an `…` marks that something was dropped, so the row never claims to
 * be the whole list. **The LAST item survives**: a legend's final entry is the way out
 * (`esc back`, `esc cancel`), and a degradation that drops the escape hatch to keep
 * `c test` on screen has thrown away the one key the user cannot do without.
 *
 * With no width, or one that fits, the list is returned untouched.
 */
export function legendLine(items: readonly string[], width?: number): string {
  const kept = items.filter((i) => i.trim() !== "");
  const full = kept.join(" · ");
  if (width === undefined || width <= 0 || stringWidth(full) <= width) return full;
  const exit = kept[kept.length - 1];
  for (let n = kept.length - 2; n > 0; n--) {
    const candidate = [...kept.slice(0, n), "…", exit].join(" · ");
    if (stringWidth(candidate) <= width) return candidate;
  }
  // Room for the way out and nothing else, or not even that: an honest truncation
  // beats a row of half a word.
  const pair = [kept[0], "…", exit].join(" · ");
  if (kept.length > 1 && stringWidth(exit) <= width) {
    return stringWidth(pair) <= width ? pair : exit;
  }
  return truncateAnsi(kept[0] ?? "", width, "…");
}

/**
 * Slice bounds for a viewport of `height` rows keeping `selected` centered,/**
 * Slice bounds for a viewport of `height` rows keeping `selected` centered,
 * clamped so the window never runs past either edge. A list shorter than the
 * viewport yields `start = 0` and the whole list — no blank-row padding.
 */
export function windowAround(
  selected: number,
  total: number,
  height: number,
): { start: number; end: number } {
  const start = Math.max(0, Math.min(selected - Math.floor(height / 2), total - height));
  return { start, end: start + height };
}

// ---- part folding -----------------------------------------------------------

export type Segment =
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "image"; part: ImagePart }
  | { kind: "ask"; part: AskPart }
  | { kind: "workflow"; part: WorkflowPart }
  | { kind: "tools"; parts: Part[] };

/**
 * Group a message's parts into renderable segments, preserving order.
 *
 * The rule that matters: **consecutive tool_call/tool_result parts fold into ONE
 * group**, so a round of program + result reads as a single collapsed step rather
 * than as two entries; prose between two groups splits them, because that prose is
 * the model narrating a boundary. A settled `ask()` stands alone — it is a human
 * exchange, not tool plumbing, and folding it would hide an answer the user gave.
 * A launched workflow stands alone for the same reason: it is a card with its own
 * live status and its own click target, and folding it into a tool group would
 * bury the one row that leads back to the run.
 */
export function segmentParts(parts: Part[]): Segment[] {
  const segs: Segment[] = [];
  for (const p of parts) {
    if (p.type === "text") segs.push({ kind: "text", text: p.text });
    else if (p.type === "reasoning") segs.push({ kind: "reasoning", text: p.text });
    else if (p.type === "image") segs.push({ kind: "image", part: p });
    else if (p.type === "ask") segs.push({ kind: "ask", part: p });
    else if (p.type === "workflow") segs.push({ kind: "workflow", part: p });
    else {
      const last = segs[segs.length - 1];
      if (last?.kind === "tools") last.parts.push(p);
      else segs.push({ kind: "tools", parts: [p] });
    }
  }
  return segs;
}

export function outputText(r: ToolResult): string {
  return typeof r.output === "string" ? r.output : JSON.stringify(r.output);
}

/**
 * The facts a collapsed tool header shows without expanding: which calls, which
 * one is still running, whether anything errored or was interrupted.
 *
 * There is deliberately no "check passed" verdict here. The port had one; bough
 * has no acceptance gate (spec §17), so a harness verdict would be a claim the
 * system cannot back.
 */
export function toolSummary(parts: Part[]) {
  const calls = parts.filter((p): p is ToolCall => p.type === "tool_call");
  const results = new Map(
    parts.filter((p): p is ToolResult => p.type === "tool_result").map((p) => [p.callId, p]),
  );
  const running = calls.find((c) => !results.has(c.id));
  const errors = [...results.values()].filter((r) => r.isError).length;
  const hasError = errors > 0;
  const interrupted = [...results.values()].some((r) => r.interrupted);
  // `errors` as well as `hasError`, because "one call failed and the next one worked" and
  // "every call failed" are different news and the header used to render them identically.
  return { calls, results, running, hasError, errors, interrupted };
}

/**
 * What a program DID, named the way a reviewer would name it.
 *
 * The collapsed step header used to be the program's first line of code, clipped:
 *
 *   ▸ 1 step  run_steps · const out = await bash(`node --input-type=module -e "
 *
 * — which reads as debug output rather than as a UI, and answers none of the
 * questions a reader has (which files did it touch? did it run something?). Every
 * comparable harness names the operation and its target: `Update(app.mjs)`,
 * `Read 1 file`, `Ran 1 shell command`.
 *
 * bough writes ONE program per round rather than one call, so the equivalent is a
 * tally of the host functions it called: `read app.mjs · ran 1 command`. Derived
 * by scanning the source for host-function call sites, which is a heuristic and is
 * allowed to be — it is a LABEL. When nothing is recognized the code gist is still
 * the fallback, so an unusual program degrades to what was shown before rather
 * than to nothing.
 *
 * `running` puts the verbs in the present tense. A call with no result yet is a
 * call still in flight, and "ran 1 command" under a shell that has been blocked
 * for ten seconds is a statement the reader acts on and should not.
 */
export function programSummary(code: string, max = 64, running = false): string {
  if (!code) return "";
  const bits: string[] = [];
  const files = (re: RegExp): string[] => {
    const out: string[] = [];
    for (const m of code.matchAll(re)) {
      const p = m[1];
      if (p && !out.includes(p)) out.push(p);
    }
    return out;
  };
  // `(?<![.\w])`, not `\b`: `\b` matches between a dot and a letter, so EVERY host-fn
  // name also matched its member-call twin. Seen on screen: a program whose only host call
  // was `artifact()` was headlined `collected subagent reports · published an artifact`,
  // because it also contained `functions.join("\n")`. The same trap was waiting in
  // `.write(` on a stream, `.search(` on a string, and `.view(`/`.ask(` on anything.
  const count = (re: RegExp) => [...code.matchAll(re)].length;
  /**
   * `join` pulled out of `node:path`, in either spelling — the host verb is shadowed.
   *
   * BOTH forms, because the one I did not write is the one the model used: it wrote
   * `import { join } from "node:path"` and the row said "collected subagent reports".
   */
  const shadowsJoin =
    /\{[^}]*\bjoin\b[^}]*\}\s*=\s*(?:await\s+)?(?:import|require)\s*\(\s*["'`](?:node:)?path/
      .test(code) ||
    /import\s*\{[^}]*\bjoin\b[^}]*\}\s*from\s*["'`](?:node:)?path/.test(code);

  const name = (p: string) => p.split("/").filter(Boolean).pop() ?? p;
  /**
   * A path with `${…}` in it is a TEMPLATE, not a name.
   *
   * Seen on a fresh walk: `▸ 1 step · wrote ${cartPath}` — the regex captured the text between
   * the backticks and printed the placeholder at the reader. An interpolated path is unnamed, so
   * it falls through to the counting branch below and reads `wrote 1 file`.
   */
  const named = (p: string) => !p.includes("${");
  const list = (paths: string[]) =>
    paths.length <= 2 ? paths.map(name).join(", ") : `${name(paths[0])} +${paths.length - 1} more`;

  // `patch` is NOT in this alternation, and that is the whole point: it takes ONE
  // string — the patch body — not a path, so matching it here captured the entire
  // template literal and the header read `wrote cart.js#8902] SWAP 3.=3: + for (…`.
  // Its files are the `[path#hash]` section tags inside that body, and there may be
  // several of them in one call.
  const wrote = [
    ...files(/(?:(?<![.\w])(?:write|edit)|writeFileSync|writeFile)\s*\(\s*["'`]([^"'`]+)/g),
    ...(/\bpatch\s*\(/.test(code) ? files(/\[([^\]\s#]+)#[^\]\s]*\]/g) : []),
  ].filter(named).filter((p, i, a) => a.indexOf(p) === i);
  // `Bun.file(path)` counts as reading it. `files.md` sends the model there for raw content
  // ("there is no read()"), so it is a DOCUMENTED path — and a documented path that produces
  // an unlabeled row is the same defect as an undocumented one. Seen:
  // `▸ 1 step · const content = await Bun.file("/private/tmp/…")`.
  // Three spellings, because a program reads a file three ways in practice: the host verb,
  // the `Bun.file` the prompt recommends, and `node:fs` — which models reach for whatever the
  // prompt says. `readFileSync`/`readFile` are matched WITH a possible dot (`fs.readFileSync`)
  // unlike the host names, because those two words mean the same thing on any object.
  const readAll = files(
    /(?:(?<![.\w])(?:view|read)|Bun\.file|readFileSync|readFile)\s*\(\s*["'`]([^"'`]+)/g,
  );
  const read = readAll.filter(named);
  if (wrote.length) bits.push(`${running ? "writing" : "wrote"} ${list(wrote)}`);
  if (read.length) bits.push(`${running ? "reading" : "read"} ${list(read)}`);
  // A call whose path is a VARIABLE yields no name — `fs.readFileSync(filePath, "utf8")` —
  // and naming nothing is not a reason to fall back to a line of source. Count it instead.
  if (wrote.length === 0) {
    // `patch` belongs in the COUNT even though it is deliberately absent from the naming
    // alternation above. Its files come from the `[path#tag]` tags inside its body, so a
    // program that builds that path — `patch(\`[${path}#3AF0]…\`)`, which is exactly what a
    // skill told to view-then-patch writes — named nothing AND counted nothing, and the
    // header fell all the way back to a line of source:
    //
    //   ▸ 1 step · const path = "/private/tmp/claude-501/-Users-andrey-repos-bo…
    const n = count(/(?:(?<![.\w])(?:write|edit|patch)|writeFileSync|writeFile)\s*\(/g);
    if (n) bits.push(`${running ? "writing" : "wrote"} ${plural(n, "file")}`);
  }
  if (read.length === 0) {
    const n = count(/(?:(?<![.\w])view|Bun\.file|readFileSync|readFile)\s*\(/g);
    if (n) bits.push(`${running ? "reading" : "read"} ${plural(n, "file")}`);
  }

  // `execSync`/`spawnSync`/`Bun.$` are running a command too. A program that reached for
  // `node:child_process` instead of `bash()` was unrecognized, so the header fell back to
  // source: `const { execSync } = require("node:child_process");`.
  //
  // The SYNC names only, plus `Bun.$`. Bare `spawn(` and `exec(` are excluded and that is
  // not fussiness: `spawn` IS bough's detached-delegation verb, so counting it here made
  // a fan-out report `ran 1 command · 1 subagent`.
  const shells = count(/(?<![.\w])bash\s*\(/g) + count(/(?<![.\w])sh\s*\(/g) +
    count(/(?<![.\w])(?:execSync|execFileSync|spawnSync)\s*\(/g) + count(/\bBun\.\$/g);
  if (shells) {
    bits.push(`${running ? "running" : "ran"} ${shells} command${shells === 1 ? "" : "s"}`);
  }
  // `bashBg(` does not match `\bbash\s*\(`, so a round that only backgrounded a
  // command was unrecognized — the one round whose whole point is that something is
  // still running after it returns.
  const bg = count(/(?<![.\w])bashBg\s*\(/g);
  if (bg) bits.push(`started ${bg} background command${bg === 1 ? "" : "s"}`);
  // The three shell-management verbs, which completed the set. Observed unnamed:
  // `▸ 1 step · const output = await bashOutput("bg_1");` — a round whose whole act was
  // reading a job's output, headlined with the line that read it.
  else if (count(/(?<![.\w])bashKill\s*\(/g)) bits.push("killed a background command");
  else if (count(/(?<![.\w])bashWait\s*\(/g)) {
    bits.push(running ? "waiting for a background command" : "waited for a background command");
  } else if (count(/(?<![.\w])bashOutput\s*\(/g)) bits.push("read a background command's output");
  // Delegation is FOUR verbs, not one. Counting only `agent(` meant the round that
  // fanned three subagents out with `spawn()` matched nothing and fell back to the
  // gist — the header read `const tasks = [`, raw source, on the single round a
  // reader most needs named. `join`/`adopt` collect reports for spawns issued in an
  // earlier round, so they are named only when no spawn happens here.
  const agents = count(/(?<![.\w])(?:agent|spawn)\s*\(/g);
  if (agents) bits.push(`${agents} subagent${agents === 1 ? "" : "s"}`);
  // `node:path` exports a BARE `join`, so a program that did
  // `const { join } = await import("node:path")` was headlined "collected subagent reports".
  // Requiring `await` does not separate them — the real delegation pattern is
  // `Promise.all(ids.map((id) => join(id)))`, with no await in sight. What DOES separate them
  // is the destructure: a program that pulled `join` out of `path` has shadowed the host verb
  // and cannot be calling it.
  else if (!shadowsJoin && count(/(?<![.\w])(?:join|adopt)\s*\(/g)) {
    bits.push(running ? "collecting subagent reports" : "collected subagent reports");
  }
  // `workflow(…)` starts one; `workflow.status(…)` asks after one already running.
  // Naming only the first left every poll round falling back to the gist, so waiting
  // for a fan-out read as `await new Promise(r => setTimeout(r, 2000));`.
  if (count(/(?<![.\w])workflow\s*\(/g)) {
    bits.push(running ? "running a workflow" : "ran a workflow");
  } else if (count(/(?<![.\w])workflow\.\w+\s*\(/g)) bits.push("checked the workflow run");
  else if (/setTimeout\s*\(/.test(code)) bits.push(running ? "waiting" : "waited");
  if (count(/(?<![.\w])ask\s*\(/g)) bits.push("asked you a question");
  if (count(/(?<![.\w])artifact\s*\(/g)) {
    bits.push(running ? "publishing an artifact" : "published an artifact");
  }
  // THE REST OF THE HOST SURFACE. Everything not named here falls back to a line of source,
  // which is the state `programSummary` exists to end — and after this morning's additions it
  // was still true for half of `HOST_FN_NAMES`: a program whose only call was
  // `state.set({key, value})` was headlined
  // `await state.set({key: "campaign", value: "overnight-ux"});`.
  if (count(/(?<![.\w])state\.(?:set|delete)\s*\(/g)) bits.push("wrote session state");
  else if (count(/(?<![.\w])state\.(?:get|list)\s*\(/g)) bits.push("read session state");
  if (count(/(?<![.\w])schedule\.\w+\s*\(/g)) bits.push("changed a schedule");
  const fetched = count(/(?<![.\w])fetch\s*\(/g);
  if (fetched) bits.push(`${running ? "fetching" : "fetched"} ${plural(fetched, "URL")}`);
  if (count(/(?<![.\w])image\s*\(/g)) bits.push("attached an image");
  // Matched in the COMMAND TEXT, not as a call shape: MCP is reached by running
  // `bough mcp call`, the same way `ast-grep` is. There is no host function left to
  // count.
  const mcpCalls = count(/\bbough mcp call\b/g);
  if (mcpCalls) bits.push(`${plural(mcpCalls, "MCP call")}`);
  else if (count(/\bbough mcp\b/g)) bits.push("checked the MCP servers");
  // `ast-grep` rides inside a shell string rather than arriving as its own host
  // function, so this matches the command text, not a call shape. It stays in the
  // list because a structural search is a different ACT from a text sweep and the
  // header is the only place that distinction is visible to the user.
  if (count(/\bast-grep\b/g)) bits.push("searched by structure");

  const searches = count(/(?<![.\w])(?:grep|glob|search)\s*\(/g);
  if (searches && bits.length === 0) bits.push(running ? "searching the tree" : "searched the tree");

  // Nothing recognized: fall back to the old gist rather than to an empty header.
  if (bits.length === 0) return "";
  const joined = bits.join(" · ");
  return joined.length > max ? `${joined.slice(0, max - 1).trimEnd()}…` : joined;
}

// ---- markdown-lite ----------------------------------------------------------
// Terminal styling for prose: headings/bold via SGR bold, `code` spans colored,
// fenced blocks highlighted on a raised surface, "- " bullets prettified.
// Deliberately conservative — italics, tables and images are left as-is.

/**
 * OSC 8 hyperlink. Supporting terminals make the text clickable, the rest ignore
 * the sequence. Zero-width for wrap-ansi/slice-ansi/strip-ansi (their ansi-regex
 * matches OSC with BEL and ST terminators), so layout math is unchanged.
 */
const osc8 = (url: string, text: string) =>
  COLOR ? `\x1b]8;;${url}\x1b\\${text}\x1b]8;;\x1b\\` : text;

/** A run of characters that can belong to a URL, once the scheme has started. */
const URL_CHARS = /[^\s"'`<>()[\]{}│]/;

/**
 * The bare URL under 0-based column `col` of a PLAIN row, with where it sits.
 *
 * `linkAt` answers the same question from OSC 8 markers, which only the transcript
 * emits. Everything else bough paints — a panel message, a rail row, a job card —
 * is plain text, so a URL sitting in it was unclickable no matter how obviously it
 * was a URL. This reads the characters instead, which works on any surface.
 */
export function urlAt(
  plain: string,
  col: number,
): { url: string; start: number; end: number } | null {
  for (const m of plain.matchAll(/https?:\/\//g)) {
    const start = m.index;
    let end = start;
    while (end < plain.length && URL_CHARS.test(plain[end]!)) end++;
    if (col >= start && col < end) {
      // Trailing sentence punctuation is not part of the address — the same rule
      // `linkifyUrl` applies when it makes prose clickable.
      const url = plain.slice(start, end).replace(/[.,;:!?]+$/, "");
      return { url, start, end: start + url.length };
    }
  }
  return null;
}

/**
 * The URL under `(row, col)`, rejoined across the rows it was wrapped onto.
 *
 * A long URL — an OAuth authorization link is the case that matters — is laid out
 * across four or five rows, and each of them holds a fragment that is not an
 * address. Clicking one and opening the fragment would be worse than doing nothing.
 *
 * `rows` are CONTENT rows: already stripped of any border or padding, so "these
 * two rows join" is a fact about the text rather than about the box it is drawn in.
 * Two rows join when the upper ends and the lower begins on characters that could
 * both belong to a URL — which is what a wrap inside an address looks like, and
 * what a wrap between two words does not.
 */
export function urlAcross(rows: readonly string[], row: number, col: number): string | null {
  // A row that CONTINUES an address is one unbroken token — an address has no
  // spaces, so a wrap inside one produces a row that is nothing but more address. A
  // row with a space in it is prose, or the next list entry.
  //
  // This replaced a "the upper row must be filled to the wrap width" rule that was
  // both too strict and too loose: the first row of a message is rarely full (the
  // wrapper moves a long token down whole, so a wrapped URL stopped joining at its
  // first row), and "…%2Fmcp" sitting above "1 ❯ ○ linear" still looked like a
  // join, so a click opened the address with a stray "1" welded onto the end.
  const joins = (above: string, below: string): boolean => {
    const a = above.trimEnd(), b = below.trim();
    return a.length > 0 && b.length > 0 && !/\s/.test(b) &&
      URL_CHARS.test(a[a.length - 1]!) && URL_CHARS.test(b[0]!);
  };
  // BACKWARD FIRST. The click usually lands in the middle of a long address, on a
  // row that carries no scheme at all — the whole reason the first cut of this
  // found nothing when you clicked an authorization link anywhere but its first
  // row.
  let start = row;
  while (start > 0 && joins(rows[start - 1] ?? "", rows[start] ?? "")) start--;
  // Then forward, remembering where the clicked cell ended up in the joined text.
  let joined = "";
  let clickAt = -1;
  for (let y = start; y < rows.length; y++) {
    if (y > start && !joins(rows[y - 1] ?? "", rows[y] ?? "")) break;
    if (y === row) clickAt = joined.length + col;
    joined += (rows[y] ?? "").trimEnd();
  }
  if (clickAt < 0) return null;
  return urlAt(joined, clickAt)?.url ?? null;
}

/**
 * The OSC 8 target under 0-based column `col`, or null — a plain click can
 * then open the link even though the TUI's mouse reporting keeps the terminal's
 * own hit-testing away. The escapes are zero-width, so the column math counts only
 * the visible text between markers; a wrapped URL works because wrap-ansi re-opens
 * the link (with the full target) on each continuation line.
 */
export function linkAt(text: string, col: number): string | null {
  // deno-lint-ignore no-control-regex -- OSC 8 hyperlinks are literal escapes.
  const re = /\x1b\]8;;([^\x07\x1b]*)(?:\x07|\x1b\\)/g;
  let url: string | null = null;
  let w = 0;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    w += stringWidth(text.slice(last, m.index));
    if (col < w) return url;
    url = m[1] || null;
    last = m.index + m[0].length;
  }
  return url && col < w + stringWidth(text.slice(last)) ? url : null;
}

/** A string that is entirely one bare URL (promotes `code`-span URLs to links). */
const BARE_URL = /^https?:\/\/[^\s)\]>'"]+$/;

function linkifyUrl(m: string): string {
  const url = m.replace(/[.,;:!?]+$/, "");
  return osc8(url, underline(url)) + m.slice(url.length);
}

/**
 * Make bare URLs in RAW (non-markdown) text clickable. Program output is where
 * served links land — `artifact()` returns one — and a printed link must open on
 * click exactly like one in prose.
 */
export function linkifyUrls(line: string): string {
  return line.replace(/https?:\/\/[^\s)\]>'"]+/g, linkifyUrl);
}

function mdInline(line: string): string {
  // Swap code spans and rendered links for NUL-fenced placeholders so their
  // contents are exempt from later passes: bold must still match across a code
  // span ("**bold with `code`**"), and the bare-URL pass must not re-match a URL
  // already inside an OSC 8 wrapper (nesting truncates the link).
  const spans: string[] = [];
  const guard = (rendered: string) => `\x00${spans.push(rendered) - 1}\x00`;
  return line
    // A code span that IS a bare URL renders clickable — models present artifact
    // links as `http://…`, and a dead link there is the common failure. A URL
    // inside a longer span (a `curl https://…` example) stays literal code.
    .replace(
      /`([^`]+)`/g,
      (_m, inner: string) =>
        BARE_URL.test(inner) ? guard(linkifyUrl(inner)) : guard(fg(colors.code, inner)),
    )
    .replace(/\*\*([^*]+)\*\*/g, (_m, inner: string) => bold(inner))
    // ITALIC, after bold so `**x**` is never seen as two single-star spans. A model uses
    // `*emphasis*` and `_emphasis_` as freely as it uses bold, and untouched they reached
    // the screen as literal asterisks and underscores in the middle of prose.
    //
    // `_` only at a word boundary: `snake_case_name` and `__init__` are identifiers, not
    // emphasis, and they appear in this kind of prose constantly. `*` needs no such guard
    // — it is not a word character anywhere.
    // The delimiter must HUG its text (CommonMark's rule), or `2 * 3 * 4` becomes an
    // italic " 3 " and arithmetic in prose comes out slanted and missing its operators.
    // `~~struck~~`, before the single-delimiter passes for the same reason bold goes before
    // italic. Left literal until now, so a model striking out a superseded step wrote tildes
    // at the reader.
    .replace(/~~([^~\n]+)~~/g, (_m, inner: string) => strike(inner))
    // The edge characters exclude the delimiter itself as well as whitespace, or `__init__`
    // is read as an italic `_init_` — the boundary character is allowed to BE an underscore
    // otherwise, and dunder names are everywhere in this kind of prose.
    .replace(/\*([^*\s][^*\n]*?[^*\s]|[^*\s])\*/g, (_m, inner: string) => italic(inner))
    .replace(
      /(?<![\w\\])_([^_\s][^_\n]*?[^_\s]|[^_\s])_(?!\w)/g,
      (_m, inner: string) => italic(inner),
    )
    // [text](url) → clickable underlined text with the url dimmed alongside. The
    // lookbehind keeps the "[" of an already-inserted SGR escape from being taken
    // as the link opener and swallowing the escape.
    .replace(
      // deno-lint-ignore no-control-regex -- the SGR lookbehind needs a literal ESC.
      /(?<!\x1b)\[([^\]]+)\]\((\S+?)\)/g,
      // A label that IS the url skips the parenthetical — "url (url)" was noise.
      (_m, text: string, url: string) =>
        guard(osc8(url, text === url ? underline(text) : `${underline(text)} ${dim(`(${url})`)}`)),
    )
    // Bare URLs become clickable as themselves; trailing punctuation stays prose.
    // The \x1b stop keeps a bolded URL from swallowing its own reset code.
    // deno-lint-ignore no-control-regex -- \x1b bounds a URL wrapped in SGR.
    .replace(/https?:\/\/[^\s)\]>'"\x1b]+/g, (m) => guard(linkifyUrl(m)))
    // deno-lint-ignore no-control-regex -- NUL fences the guarded-span placeholders.
    .replace(/\x00(\d+)\x00/g, (_m, i: string) => spans[+i]);
}

// ---- code highlighting ------------------------------------------------------
// A one-pass approximate tokenizer for fenced blocks and program source: strings,
// comments, keywords, numbers. Candy, not a parser — a wrong color on an exotic
// line is fine; a flat gray wall of the program that ran was the bug.

const KW = {
  js:
    "const|let|var|function|return|if|else|for|while|do|switch|case|break|continue|new|class|extends|import|export|from|default|try|catch|finally|throw|await|async|typeof|instanceof|in|of|delete|void|yield|static|get|set|this|super|null|undefined|true|false",
  python:
    "def|return|if|elif|else|for|while|break|continue|import|from|as|class|try|except|finally|raise|with|lambda|yield|global|nonlocal|assert|del|pass|and|or|not|in|is|None|True|False|async|await|match|case",
  go:
    "func|return|if|else|for|range|switch|case|break|continue|import|package|type|struct|interface|map|chan|go|defer|select|const|var|nil|true|false",
  rust:
    "fn|return|if|else|for|while|loop|break|continue|use|mod|pub|struct|enum|impl|trait|match|let|mut|const|static|ref|move|async|await|dyn|where|Self|self|None|Some|Ok|Err|true|false",
  bash:
    "if|then|else|elif|fi|for|do|done|while|case|esac|function|return|exit|export|local|readonly|set|unset|shift|source|echo|true|false",
  sql:
    "SELECT|FROM|WHERE|AND|OR|NOT|INSERT|INTO|VALUES|UPDATE|SET|DELETE|CREATE|TABLE|INDEX|JOIN|LEFT|RIGHT|INNER|OUTER|ON|AS|ORDER|BY|GROUP|HAVING|LIMIT|NULL|IS|IN|LIKE|BETWEEN|DISTINCT",
} as const;

const LANG_ALIASES: Record<string, keyof typeof KW> = {
  js: "js",
  jsx: "js",
  ts: "js",
  tsx: "js",
  javascript: "js",
  typescript: "js",
  json: "js",
  c: "js",
  cpp: "js",
  java: "js",
  python: "python",
  py: "python",
  go: "go",
  rust: "rust",
  rs: "rust",
  bash: "bash",
  sh: "bash",
  zsh: "bash",
  shell: "bash",
  sql: "sql",
};

const LINE_COMMENT: Partial<Record<keyof typeof KW, string>> = {
  js: "//",
  go: "//",
  rust: "//",
  python: "#",
  bash: "#",
  sql: "--",
};

// One combined regex per language, applied in a single pass so inserted SGR codes
// are never re-matched (the digits inside an escape would look like a number).
const HL_RE = new Map<keyof typeof KW, RegExp>();
function hlRegex(lang: keyof typeof KW): RegExp {
  let re = HL_RE.get(lang);
  if (!re) {
    re = new RegExp(
      `("(?:[^"\\\\]|\\\\.)*"|'(?:[^'\\\\]|\\\\.)*'|\`(?:[^\`\\\\]|\\\\.)*\`)|\\b(${
        KW[lang]
      })\\b|\\b(\\d+(?:\\.\\d+)?)\\b`,
      lang === "sql" ? "gi" : "g",
    );
    HL_RE.set(lang, re);
  }
  return re;
}

/** Highlight one line of code. `langTag` is the fence tag (`""` is fine). */
export function highlightCode(line: string, langTag: string): string {
  const lang = LANG_ALIASES[langTag.toLowerCase()] ?? "js"; // generic ≈ C-family
  // Split off a trailing line comment first (approximate: marker outside quotes).
  const marker = LINE_COMMENT[lang];
  let code = line;
  let comment = "";
  if (marker) {
    let quote: string | null = null;
    for (let i = 0; i < line.length; i++) {
      const c = line[i];
      if (quote) {
        if (c === "\\") i++;
        else if (c === quote) quote = null;
      } else if (c === '"' || c === "'" || c === "`") quote = c;
      else if (line.startsWith(marker, i)) {
        code = line.slice(0, i);
        comment = line.slice(i);
        break;
      }
    }
  }
  const styled = code.replace(
    hlRegex(lang),
    (_m, str: string, kw: string, num: string) =>
      str ? fg(colors.str, str) : kw ? fg(colors.keyword, kw) : fg(colors.number, num),
  );
  return styled + (comment ? dim(comment) : "");
}

/**
 * Paint a subtly raised background behind one rendered line, padded to `w` so a
 * block reads as a contained surface. Any full reset inside the line re-opens the
 * background, so a styled span cannot punch a hole in it.
 */
export function surface(line: string, w: number): string {
  if (!COLOR) return line;
  const bg = `\x1b[${colors.surfaceBg}m`;
  const pad = Math.max(0, w - stringWidth(line));
  return `${bg}${line.replaceAll("\x1b[0m", `\x1b[0m${bg}`)}${" ".repeat(pad)}\x1b[0m`;
}

/**
 * A GitHub-style table, laid out as columns instead of printed as pipes.
 *
 * `md` is otherwise line-by-line, and a table is the one construct that is not: the
 * column widths depend on every row, so it has to be gathered before any of it can be
 * drawn. Untouched, a model's table reached the screen as its raw source —
 * `| name | lines |` over `|------|-------|` — which is the single most common thing a
 * frontier model emits and the least readable way to show it.
 *
 * Header in bold over a dim rule, cells padded to the column, two spaces between. No outer
 * pipes: they cost four columns a terminal does not have and add nothing the alignment
 * does not already say. Alignment comes from the separator row (`--:` right, `:-:` centre),
 * which is also how a numeric column ends up reading as one.
 *
 * When the natural width does not fit, the WIDEST column is shrunk until it does and its
 * cells are clipped with an ellipsis — narrowing the long prose column keeps the numbers
 * beside it, where scaling everything equally would lose both.
 */
function tableLines(rows: string[][], aligns: Align[], max: number): string[] {
  const cols = Math.max(...rows.map((r) => r.length));
  const cell = (r: string[], i: number) => r[i] ?? "";
  const widths = Array.from(
    { length: cols },
    (_v, i) => Math.max(...rows.map((r) => width(stripAnsi(mdInline(cell(r, i)))))),
  );
  const GUTTER = 2;
  let total = widths.reduce((a, b) => a + b, 0) + GUTTER * (cols - 1);
  while (max > 0 && total > max) {
    const widest = widths.indexOf(Math.max(...widths));
    if (widths[widest] <= 4) break;
    widths[widest]--;
    total--;
  }
  const pad = (text: string, i: number) => {
    const shown = truncateAnsi(mdInline(text), widths[i], "…");
    const slack = Math.max(0, widths[i] - width(stripAnsi(shown)));
    if (aligns[i] === "right") return " ".repeat(slack) + shown;
    if (aligns[i] === "center") {
      const left = Math.floor(slack / 2);
      return " ".repeat(left) + shown + " ".repeat(slack - left);
    }
    return shown + " ".repeat(slack);
  };
  const line = (r: string[], bolded: boolean) =>
    widths.map((_w, i) => (bolded ? bold(pad(cell(r, i), i)) : pad(cell(r, i), i)))
      .join(" ".repeat(GUTTER))
      .trimEnd();
  const [head, ...body] = rows;
  return [
    line(head, true),
    dim(widths.map((w) => "─".repeat(w)).join(" ".repeat(GUTTER))),
    ...body.map((r) => line(r, false)),
  ];
}

type Align = "left" | "right" | "center";

/** `| a | b |` → `["a", "b"]`, tolerant of a missing leading or trailing pipe. */
function tableCells(line: string): string[] {
  return line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((c) => c.trim());
}

/** The `|---|:--:|` row under a header, which is what makes a table a table. */
function tableAligns(line: string): Align[] | null {
  const cells = tableCells(line);
  if (cells.length === 0 || !cells.every((c) => /^:?-{1,}:?$/.test(c))) return null;
  return cells.map((c) =>
    c.startsWith(":") && c.endsWith(":")
      ? "center"
      : c.endsWith(":")
      ? "right"
      : "left"
  );
}

/** Markdown-lite for one block of prose. With `codeWidth`, fences get a surface. */
export function md(text: string, codeWidth?: number): string {
  let fence: string | null = null; // the open fence's language tag
  const raise = (line: string) => (codeWidth ? surface(line, codeWidth) : line);
  const src = text.split("\n");
  // TABLES FIRST, because they are the one block construct here: a header line, the
  // separator that identifies it, and every body row until something that is not a row.
  // Rendered rows are marked so the line pass below leaves them alone.
  const done = new Map<number, string[]>();
  let scanFence = false;
  for (let i = 0; i < src.length; i++) {
    // INSIDE A FENCE NOTHING IS A TABLE. A code block showing markdown source — or any
    // language with `|` in it — must reach the reader as it was written; laying it out as
    // columns would be the renderer editing the code it was asked to display.
    if (/^\s*```/.test(src[i])) {
      scanFence = !scanFence;
      continue;
    }
    if (scanFence) continue;
    if (!src[i].includes("|")) continue;
    const aligns = i + 1 < src.length ? tableAligns(src[i + 1]) : null;
    if (!aligns) continue;
    const rows = [tableCells(src[i])];
    let j = i + 2;
    for (; j < src.length && src[j].includes("|") && src[j].trim() !== ""; j++) {
      rows.push(tableCells(src[j]));
    }
    done.set(i, tableLines(rows, aligns, codeWidth ?? 0));
    for (let k = i + 1; k < j; k++) done.set(k, []);
    i = j - 1;
  }
  return src.flatMap((line, i) => {
    const table = done.get(i);
    if (table) return table;
    if (table !== undefined) return [];
    return [oneMdLine(line)];
  }).join("\n");

  function oneMdLine(line: string): string {
    const open = line.match(/^\s*```(\S*)\s*$/);
    if (open) {
      // Fence markers frame the block instead of rendering as raw backticks.
      if (fence === null) {
        fence = open[1];
        return raise(dim(`╭ ${fence || "code"}`));
      }
      fence = null;
      return raise(dim("╰"));
    }
    if (fence !== null) return raise(`${dim("│")} ${highlightCode(line, fence)}`);
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) return h[1].length === 1 ? bold(underline(h[2])) : bold(h[2]);
    if (/^\s*(-{3,}|\*{3,})\s*$/.test(line)) return dim("─".repeat(24));
    const quoted = line.match(/^>\s?(.*)$/);
    if (quoted) return dim(`│ ${quoted[1]}`);
    return mdInline(line.replace(/^(\s*)- /, "$1• "));
  }
}

// ---- numbers in view --------------------------------------------------------

/** 1234 → "1.2k", 999 → "999". */
export function fmtTokens(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : `${n}`;
}

/** 1.234 → "$1.23", 0.0042 → "$0.004" — sub-dollar spend keeps a visible digit. */
export function fmtUsd(n: number): string {
  return `$${n >= 1 ? n.toFixed(2) : n >= 0.001 ? n.toFixed(3) : n.toFixed(4)}`;
}

/**
 * Whole-percent usable context left, measured against the model's usable prompt
 * budget. Null when the limit is unknown — an invented percentage is worse than no
 * chip, because the number people act on is "am I about to overflow" (spec §5:
 * context overflow fails the turn with an explicit error).
 */
/** Below this the context chip raises its voice — see `meterLine`. */
export const CTX_WARN_PCT = 20;

export function ctxPctLeft(
  usage: { contextTokens: number; contextLimit?: number | null },
): number | null {
  const limit = usage.contextLimit;
  if (!limit || limit <= 0) return null;
  return Math.max(0, Math.min(100, Math.floor((1 - usage.contextTokens / limit) * 100)));
}

/**
 * The always-visible cost + context line (spec §15: "chat shows … live cost and
 * context"). Pure so the chat header is data, not layout arithmetic.
 */
export function meterLine(m: {
  costUsd?: number | null;
  contextTokens?: number | null;
  contextLimit?: number | null;
  model?: string | null;
  /**
   * Thinking depth, when it is not the default. It multiplies the price of every
   * later turn and lived only inside `^o` page 2, so the one screen you could not
   * learn it from was the one you spend on.
   */
  effort?: string | null;
  /** Where the turn runs. Shortened by the caller — this only joins. */
  workspace?: string | null;
  /**
   * The branch the workspace is on. Edits land in the checkout as they happen, so
   * "where" is only half an answer without "on what".
   */
  branch?: string | null;
  /** Background shells still running. Nothing may run with no pixels on screen. */
  shells?: number | null;
  /**
   * Delegated agents and workflow runs still going.
   *
   * The same rule as `shells`, applied to the two units it did not cover. The rail is
   * the detailed answer, and the rail is DISPLACED by the panel — so while you were in
   * the tree or the changes tab, three running subagents had no pixels anywhere on the
   * screen and the one row that survives said `⚙ 0 shells`-worth of nothing about them.
   */
  agents?: number | null;
  runs?: number | null;
  /** Append the `? help` hint. False for surfaces that are not the chat. */
  help?: boolean;
  /**
   * This conversation was spawned by another, so `←` goes back to it.
   *
   * The key is bound and documented (`keys.ts`, the "read" section), and a delegator
   * persona still pressed Escape three times and concluded there was no way out — Escape is
   * the back key on every other surface here, and nothing ON the subagent screen named the
   * one that works. Overloading Escape is not the fix: with a turn running it must stay the
   * interrupt. Saying so where they were looking is.
   */
  out?: boolean;
  /** Columns available. Omitted = no degradation, the caller accepts any length. */
  width?: number;
}): string {
  // Workspace FIRST and at the bottom of the screen, next to the composer, because
  // that is where the eye already is when you are about to press enter. It sat on
  // a top line a whole screen above the input before, which is a status bar you
  // have to go and look for.
  const place = (dir: string) => (dir && m.branch ? `${dir}@${m.branch}` : dir);
  const workspace = place(m.workspace ?? "");
  // The effort rides the model token rather than taking a separator of its own:
  // it is a property OF the model choice, and the two read as one fact.
  const model = m.model ? (m.effort ? `${m.model} · ${m.effort}` : m.model) : "";
  const cost = typeof m.costUsd === "number" && m.costUsd > 0 ? fmtUsd(m.costUsd) : "";
  let context = "";
  if (typeof m.contextTokens === "number" && m.contextTokens > 0) {
    const pct = ctxPctLeft({ contextTokens: m.contextTokens, contextLimit: m.contextLimit });
    // bough has no auto-compaction by design, so this chip is the ONLY warning
    // before a turn fails on overflow — and 97% and 7% used to read identically.
    // The mark is text, not colour: the caller renders this row as one dim string.
    // And when it warns, it says the way OUT. The chip is the only notice before a
    // turn fails on overflow, and "⚠ 7% ctx left" tells a user their problem without
    // telling them the one command that solves it.
    context = pct === null
      ? `${fmtTokens(m.contextTokens)} ctx`
      : pct <= CTX_WARN_PCT
      ? `⚠ ${pct}% ctx left — /compact`
      : `${pct}% ctx left`;
  }
  const shells = m.shells && m.shells > 0 ? `⚙ ${m.shells} shell${m.shells === 1 ? "" : "s"}` : "";
  const agents = m.agents && m.agents > 0 ? `◆ ${m.agents} agent${m.agents === 1 ? "" : "s"}` : "";
  const runs = m.runs && m.runs > 0 ? `⧉ ${m.runs} run${m.runs === 1 ? "" : "s"}` : "";
  // Glyph-and-number, for the widths where the spelled-out words do not fit. What is
  // running must survive degradation: it is the one part of this row that is not a
  // property of the session but a statement about right now.
  const live = [
    m.shells && m.shells > 0 ? `⚙${m.shells}` : "",
    m.agents && m.agents > 0 ? `◆${m.agents}` : "",
    m.runs && m.runs > 0 ? `⧉${m.runs}` : "",
  ].filter(Boolean).join(" ");
  const help = m.help ? "? help" : "";
  const out = m.out ? "← back" : "";
  const join = (...bits: string[]) => bits.filter(Boolean).join(" · ");

  const full = join(workspace, model, cost, context, shells, agents, runs, out, help);
  if (!m.width || width(full) <= m.width) return full;

  // Too narrow for everything. Degrade in priority order instead of wrapping onto
  // a second row: a status bar that reflows steals a line from the transcript and
  // reads as a rendering bug. Cost and context go last because they are the two
  // numbers that change, and the whole point of a live meter is watching them.
  const base = place((m.workspace ?? "").replace(/\/+$/, "").split("/").pop() ?? "");
  for (
    const candidate of [
      // `out` rides with `help` down the ladder: both are three-character hints, and the
      // one that says how to LEAVE a conversation you did not open on purpose is worth more
      // than a token count. Adding it only to `full` above is exactly how the first version
      // of this shipped invisible — the full line begins with an ABSOLUTE workspace path, so
      // on a 100-column terminal it always degrades and the chip was always dropped.
      join(base, model, cost, context, shells, agents, runs, out, help),
      join(model, cost, context, shells, agents, runs, out, help),
      join(cost, context, live, out, help),
      join(cost, context, out, live),
      join(context, live),
      join(context),
    ]
  ) {
    if (width(candidate) <= m.width) return candidate;
  }
  return truncateAnsi(full, m.width, "…");
}

// The conversation prefix rides a 5-minute sliding cache TTL.
const CACHE_TTL_MS = 5 * 60_000;
// Contexts below this re-cache for pennies — no chip, no noise.
const COLD_NOTE_MIN_TOKENS = 20_000;

/**
 * The note shown when the next message would re-write the prompt cache: the
 * context is substantial and the last round is older than the TTL. Null while
 * warm, small, or never-run. `now` is a parameter — this is pure core.
 */
export function coldCacheNote(
  usage: { contextTokens: number; lastLlmAt?: number | null },
  now: number,
): string | null {
  if (usage.contextTokens < COLD_NOTE_MIN_TOKENS) return null;
  if (!usage.lastLlmAt || now - usage.lastLlmAt < CACHE_TTL_MS) return null;
  return `❄ re-caches ~${fmtTokens(usage.contextTokens)}`;
}

export function relTime(ts: number, now: number): string {
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

/** How long a disconnect stays a quiet "reconnecting…" before escalating. */
export const DISCONNECT_ESCALATE_MS = 15_000;

/**
 * Status text while the event stream is down. A dead server must not read like a
 * blip forever, so past the escalation window it names the elapsed time and the
 * move that fixes it.
 */
export function disconnectNote(sinceMs: number, now: number): { text: string; urgent: boolean } {
  if (now - sinceMs < DISCONNECT_ESCALATE_MS) return { text: "reconnecting…", urgent: false };
  const secs = Math.floor((now - sinceMs) / 1000);
  return {
    text: `server unreachable for ${secs}s — is it running? ` +
      `(bough serves the TUI; restart it and this will reconnect)`,
    urgent: true,
  };
}

/** Row label for a session: its title, else the workspace basename, else untitled. */
export function sessionLabel(title: string | null | undefined, workspace?: string | null): string {
  const t = (title ?? "").trim();
  if (t && t !== "untitled") return t;
  const base = (workspace ?? "").replace(/\/+$/, "").split("/").pop() ?? "";
  return base || "(untitled)";
}

/**
 * A provider's retry reason, reduced to something a person can read.
 *
 * The raw value is whatever the provider sent, and providers send JSON. A real
 * one, straight onto a first-time user's screen mid-turn:
 *
 *   retrying (attempt 1) — openrouter: 429 {"error":{"message":"Provider returned e…
 *
 * — a truncated JSON blob, in a notice, to someone who is already unsure whether
 * they have broken something. The useful content of that string is "rate limited";
 * everything after the brace is noise that also crowds out the part that is not.
 *
 * Deliberately conservative: it lifts a nested `message` when there is one,
 * otherwise it keeps the prose and drops the payload. It never tries to classify
 * an error it does not recognize — an unfamiliar reason is shown, just shorter.
 */
export function humanizeRetryReason(raw: string, max = 60): string {
  const text = (raw ?? "").trim();
  if (text === "") return "no reason given";

  // A well-known status is worth naming, because the number is the whole meaning.
  const status = /\b(429|408|500|502|503|504)\b/.exec(text)?.[1];
  const named: Record<string, string> = {
    "429": "rate limited",
    "408": "request timed out",
    "500": "provider error",
    "502": "provider unreachable",
    "503": "provider overloaded",
    "504": "provider timed out",
  };

  // The provider's own sentence, if it buried one in the JSON.
  const nested = /"message"\s*:\s*"((?:[^"\\]|\\.)*)"/.exec(text)?.[1];
  // The status token itself is not prose: "503" next to "provider overloaded" is
  // the same fact twice, and the name is the readable half.
  const prose = (nested ?? text.split(/[{[]/)[0])
    .replace(status ? new RegExp(`\\b${status}\\b`, "g") : /(?!)/g, "")
    .replace(/\s+/g, " ").trim()
    .replace(/^[:\-\s]+|[:\-\s]+$/g, "");

  const prefix = status ? named[status] : "";
  const body = prose && prose !== prefix ? prose : "";
  const joined = prefix && body ? `${prefix} · ${body}` : prefix || body || text;
  return joined.length > max ? `${joined.slice(0, max - 1).trimEnd()}…` : joined;
}

/** `/Users/me/repos/x` → `~/repos/x`. Absolute paths eat a header; `~` does not. */
export function shortenPath(path: string, home?: string | null): string {
  const h = (home ?? "").replace(/\/+$/, "");
  if (h && (path === h || path.startsWith(h + "/"))) return "~" + path.slice(h.length);
  return path;
}

/** Braille spinner frames. Ten of them, so the phase reads as motion, not a glitch. */
const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/** Ticks per spinner cycle, exposed so the caller's interval and this agree. */
export const SPINNER_MS = 120;

/**
 * The line shown while a turn is running.
 *
 * It exists because the running state used to be indistinguishable from a hung
 * one: the ONLY feedback was the cheap-tier `activity` blurb, which is best-effort
 * by construction and absent for the whole first stretch of a turn. A turn that
 * has printed nothing yet looked exactly like a frozen terminal, and the one key
 * that would fix a frozen terminal — esc — was documented nowhere on screen.
 *
 * So: motion, elapsed time, and the way out, always, for as long as a turn runs.
 * The activity blurb rides along when there is one rather than replacing this.
 */
export function busyLine(
  opts: {
    activity?: string | null;
    elapsedMs: number;
    tick: number;
    /**
     * Tokens streamed SO FAR in this turn.
     *
     * The spinner and the elapsed seconds were the whole running line, so a long
     * turn moved nothing that said how much work it was doing. Absent is the normal
     * case for a provider that reports usage only at the end, and the line degrades
     * to what it always said.
     *
     * Deliberately NOT cost: a per-turn dollar figure was asked to be removed. The
     * session total is on the status row and is the number that matters.
     */
    tokens?: number | null;
  },
): string {
  const frame = SPINNER[Math.abs(Math.trunc(opts.tick)) % SPINNER.length];
  const what = (opts.activity ?? "").trim() || "working";
  const bits = [what, fmtDuration(opts.elapsedMs)];
  if (typeof opts.tokens === "number" && opts.tokens > 0) bits.push(`${fmtTokens(opts.tokens)} tok`);
  bits.push("esc interrupts");
  return `${frame} ${bits.join(" · ")}`;
}

/** Cells in a unit's progress bar. Eight, so 12.5% is the smallest visible step. */
const BAR_CELLS = 8;

/**
 * A determinate bar, and ONLY when the fraction is real.
 *
 * Spec §9: an expensive thing gets a bar. The failure the rule guards against is the
 * other one — a bar drawn from a number nobody knows, which reads as progress and is
 * decoration. So this is called only where `progress !== null`; a null unit renders
 * no bar rather than an empty trough.
 */
function progressBar(fraction: number): string {
  const filled = Math.max(0, Math.min(BAR_CELLS, Math.round(fraction * BAR_CELLS)));
  return `${"█".repeat(filled)}${"░".repeat(BAR_CELLS - filled)} ${
    Math.round(Math.max(0, Math.min(1, fraction)) * 100)
  }%`;
}

/**
 * One row of the live-work rail: what is running, for how long, and what it costs.
 *
 * SPEC §5 — nothing runs invisibly, and every unit is attributed SEPARATELY. The rail
 * used to say `◆ sleep 45  ⋯ working` for every agent alive, which cannot tell a stuck
 * one from a slow one: two identical rows, one of them wedged. Elapsed answers that
 * question, and tokens answer the next one (an agent burning tokens is thinking; one
 * that has burnt none in four minutes is blocked on a shell).
 *
 * Nothing is re-derived here — `LiveUnit` already carries elapsed, tokens, spend and
 * progress (`store.ts`), because the numbers must be the same ones a stop acts on.
 * The DETAIL is last and is the only thing that clips: a command line is context, and
 * the numbers are the message.
 */
export function unitLine(u: LiveUnit, cols: number): string {
  const glyph = u.kind === "shell"
    ? "⚙"
    : u.kind === "subagent"
    ? "◆"
    : u.kind === "schedule"
    ? "⏱"
    : "⧉";
  // A schedule's glyph is DIM where the live kinds get a colour: the rail is a
  // "what is happening" surface and a schedule is a thing that will happen.
  const hue = u.kind === "shell" ? warn : u.kind === "subagent" ? info : u.kind === "schedule" ? dim : accent;
  // A schedule counts DOWN — `elapsedMs` is time until it fires (store.ts) — and
  // once due it says so: the ticker fires within ~30s, and a negative countdown
  // rendered as elapsed time would read as a schedule that has been running.
  const bits = [
    u.kind === "schedule"
      ? u.elapsedMs < 1000 ? "due" : `in ${fmtDuration(u.elapsedMs)}`
      : fmtDuration(u.elapsedMs),
  ];
  if (typeof u.tokens === "number" && u.tokens > 0) bits.push(`${fmtTokens(u.tokens)} tok`);
  if (typeof u.costUsd === "number" && u.costUsd > 0) bits.push(fmtUsd(u.costUsd));
  if (u.progress !== null) bits.push(progressBar(u.progress));
  const name = clip(u.title, 28);
  const tail = bits.join(" · ");
  // Two spaces separate the name from the numbers; the detail takes whatever is left
  // and is dropped entirely rather than rendered as an ellipsis on its own.
  const room = cols - width(`${glyph} ${name}`) - width(tail) - 6;
  // A DETAIL THAT REPEATS THE NAME IS NOT CONTEXT. A background shell's title and detail
  // are both its command line, so the rail read
  //
  //   ⚙ sleep 120  5s · sleep 120
  //
  // — the same six characters twice, with the numbers wedged between them. The field is
  // there for a unit whose detail says something the title does not (a subagent's task
  // under its label); when it does not, the row is better without it.
  // Compared against the UNCLIPPED title: `name` may carry a trailing ellipsis, which no
  // detail starts with, so comparing to it silently never matched.
  const repeats = u.detail !== null && u.detail !== undefined &&
    (u.detail === u.title || u.detail.startsWith(u.title));
  const detail = u.detail && room >= 8 && !repeats ? dim(` · ${clip(u.detail, room)}`) : "";
  return `${hue(glyph)} ${name}  ${dim(tail)}${detail}`;
}

/** `9s`, `1m04s`. Seconds below a minute; a turn that runs an hour still reads. */
export function fmtDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  if (total < 60) return `${total}s`;
  const mins = Math.floor(total / 60);
  const secs = total % 60;
  if (mins < 60) return `${mins}m${String(secs).padStart(2, "0")}s`;
  return `${Math.floor(mins / 60)}h${String(mins % 60).padStart(2, "0")}m`;
}

// ---- composer completion ----------------------------------------------------

/**
 * Fuzzy rank: exact prefix beats word-boundary prefix beats substring beats
 * in-order subsequence; a non-match scores 0 and drops out.
 */
export function fuzzyScore(candidate: string, query: string): number {
  if (!query) return 1;
  const c = candidate.toLowerCase();
  const q = query.toLowerCase();
  if (c.startsWith(q)) return 4;
  if (c.includes("-" + q) || c.includes("_" + q) || c.includes(" " + q) || c.includes("/" + q)) {
    return 3;
  }
  if (c.includes(q)) return 2;
  let i = 0;
  for (const ch of c) {
    if (ch === q[i]) i++;
    if (i === q.length) return 1;
  }
  return 0;
}

/**
 * The candidate indices `fuzzyScore` matched, for highlighting a popup row — same
 * tier order, so the marked characters are the ones that made it match.
 */
export function fuzzyPositions(candidate: string, query: string): number[] {
  if (!query) return [];
  const c = candidate.toLowerCase();
  const q = query.toLowerCase();
  const run = (start: number) => Array.from(q, (_v, i) => start + i);
  if (c.startsWith(q)) return run(0);
  for (const b of ["-", "_", " ", "/"]) {
    const i = c.indexOf(b + q);
    if (i >= 0) return run(i + 1);
  }
  const sub = c.indexOf(q);
  if (sub >= 0) return run(sub);
  const pos: number[] = [];
  for (let j = 0; j < c.length && pos.length < q.length; j++) {
    if (c[j] === q[pos.length]) pos.push(j);
  }
  return pos.length === q.length ? pos : [];
}

/** What the composer is currently completing, if anything. */
export interface Trigger {
  /** `file` = an `@` workspace reference; `skill` = a `/` skill invocation. */
  kind: "file" | "skill";
  /** The text between the marker and the cursor — what to rank candidates by. */
  query: string;
  /** Index of the marker, and the end of the token being replaced. */
  start: number;
  end: number;
}

/**
 * Which completion the cursor is inside.
 *
 * THE RULE, and the reason this is a function rather than a `startsWith` check:
 * **both markers fire at ANY word boundary** — position 0 or after whitespace —
 * not only at the start of the input. "look at @src/x.ts" completes a path and
 * "fix this /commit" completes a skill, because a marker mid-input is exactly
 * where a reference belongs in a sentence. The complement matters just as much: a
 * `/` inside a word (`a/path/b`) or an `@` inside one (`user@host`) is NOT a
 * marker and must never swallow the token — that misfire is what makes a picker
 * feel possessed.
 *
 * A marker with whitespace between it and the cursor has been left behind: the
 * user finished the reference and moved on, so nothing is being completed.
 */
export function activeTrigger(text: string, cursor: number): Trigger | null {
  const end = (() => {
    const ws = text.slice(cursor).search(/\s/);
    return ws < 0 ? text.length : cursor + ws;
  })();
  for (const [marker, kind] of [["/", "skill"], ["@", "file"]] as const) {
    const at = text.lastIndexOf(marker, Math.max(0, cursor - 1));
    if (at < 0) continue;
    if (/\s/.test(text.slice(at + 1, cursor))) continue; // the reference is finished
    if (at !== 0 && !/\s/.test(text[at - 1])) continue; // mid-word: not a marker
    return { kind, query: text.slice(at + 1, cursor), start: at, end };
  }
  return null;
}

/**
 * The directory an `@` query is browsing, when it points OUTSIDE the workspace.
 *
 * `git ls-files` is the right candidate source for `@src/x.ts` and cannot answer
 * `@~/notes/todo.md` at all — nothing outside the repo is tracked by it — so a
 * path-shaped query switches the popup to a plain directory listing instead. The
 * shapes that count as "leaving": `~`, an absolute `/`, and explicit `./` or `../`.
 * A bare `src/` is NOT one of them; that is a repo path and stays on git.
 *
 * Returns the literal prefix to prepend to each entry — so a completed row reads
 * back as the same path the user was typing — and nothing when the query is a
 * plain workspace reference.
 */
export function browsePrefix(query: string): string | null {
  if (!/^(~|\/|\.\.?\/)/.test(query)) return null;
  const q = query === "~" ? "~/" : query;
  const cut = q.lastIndexOf("/");
  return cut < 0 ? null : q.slice(0, cut + 1);
}

/** One popup row. `insert` replaces `[trigger.start, trigger.end)` wholesale. */
export interface Completion {
  label: string;
  detail: string;
  insert: string;
  /**
   * A built-in `/command` this row DISPATCHES instead of inserting — the caller
   * still removes `[trigger.start, trigger.end)`, but sends this rather than
   * leaving `/model` sitting in the draft as text. Absent on skill and file rows,
   * which are references and belong in the message.
   */
  run?: string;
  /** Label indices the fuzzy match hit, for highlighting. */
  hl?: number[];
}

/**
 * Rank candidates for a trigger and cap the list. `total` is the pre-cap count so
 * the popup can say "↓ N more" — without it a first-run user reads a six-row menu
 * as the whole catalogue and never types to narrow.
 */
export function rankCompletions(
  candidates: { name: string; detail?: string; run?: string }[],
  trigger: Trigger,
  limit = 6,
): { items: Completion[]; total: number } {
  const marker = trigger.kind === "skill" ? "/" : "@";
  const ranked = candidates
    .map((c, i) => ({ c, i, score: fuzzyScore(c.name, trigger.query) }))
    .filter((x) => x.score > 0)
    // Shorter-is-better is a statement about how WELL a name matched, so it only
    // applies once something was typed. On a bare `/` every candidate scores the
    // same and that tiebreak sorts the menu by name length — which interleaved the
    // built-in commands with whatever skills happen to have short names, at exactly
    // the moment the list is being read as "what can this thing do". With no query,
    // source order wins, and the caller puts the commands first.
    .sort((a, b) =>
      b.score - a.score ||
      (trigger.query ? a.c.name.length - b.c.name.length : 0) ||
      a.i - b.i
    );
  const items = ranked.slice(0, limit).map(({ c }) => ({
    label: `${marker}${c.name}`,
    detail: c.detail ?? "",
    insert: `${marker}${c.name}${c.name.endsWith("/") ? "" : " "}`,
    ...(c.run ? { run: c.run } : {}),
    // Positions are against the bare name; the leading marker shifts them by one.
    hl: fuzzyPositions(c.name, trigger.query).map((p) => p + 1),
  }));
  return { items, total: ranked.length };
}

/** Apply a completion to the input, returning the new text and cursor. */
export function applyCompletion(
  text: string,
  trigger: Trigger,
  item: Completion,
): { text: string; cursor: number } {
  return {
    text: text.slice(0, trigger.start) + item.insert + text.slice(trigger.end),
    cursor: trigger.start + item.insert.length,
  };
}

// ---- readline word motion ---------------------------------------------------

export function wordLeft(text: string, cursor: number): number {
  let i = cursor;
  while (i > 0 && /\s/.test(text[i - 1])) i--;
  while (i > 0 && !/\s/.test(text[i - 1])) i--;
  return i;
}

export function wordRight(text: string, cursor: number): number {
  let i = cursor;
  while (i < text.length && /\s/.test(text[i])) i++;
  while (i < text.length && !/\s/.test(text[i])) i++;
  return i;
}
