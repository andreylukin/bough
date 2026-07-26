// The conversation as a flat list of pre-wrapped visual lines — the viewport
// slices these for rendering, scrolling is an index offset, and a mouse click
// maps (row → line → click key). Replaces the old Static-seal architecture so
// any tool group, however old, can be expanded in place.
import wrapAnsi from "wrap-ansi";
import type { Message, Role } from "../schema/parts.ts";
import {
  clip,
  codeGist,
  COLOR,
  dim,
  highlightCode,
  linkifyUrls,
  md,
  outputText,
  segmentParts,
  surface,
  toolSummary,
} from "./format.ts";
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

// Spans close with the attribute-specific reset (39m for fg, 22m for bold) —
// not a full \x1b[0m: the themed <Text color> wrapping every viewport row
// re-opens only its own close code (chalk), so a full reset would strip the
// base text color for the rest of the line.
const SGR = (n: number | string, s: string) =>
  COLOR ? `\x1b[${n}m${s}\x1b[${String(n).startsWith("38;") ? "39" : "22"}m` : s;
const bold = (s: string) => SGR(1, s);
// Hue helpers read the live theme palette (truecolor) — evaluated per call, so
// an applied theme recolors rebuilt lines without a restart.
const green = (s: string) => SGR(fgParams(palette.accent), s);
const yellow = (s: string) => SGR(fgParams(palette.warn), s);
const red = (s: string) => SGR(fgParams(palette.error), s);
const blue = (s: string) => SGR(fgParams(palette.info), s);

// One accent: green is bough's color (identity + affirmative status); the user
// speaks in plain bright text. A function (not a const map) so labels pick up
// the palette active at render time.
const roleLabel = (role: Role): string =>
  role === "user"
    ? bold("you")
    : role === "supervisor"
    ? bold(green("bough"))
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
  /** An orphan note can't recover its file list ("unknown", not "none"). */
  filesUnknown: boolean;
  report: string | null;
}

export function parseSubagentNote(text: string): SubagentNote | null {
  const head = text.match(/^\[subagent finished\] "(.*)" \(([^)]+)\) — (.+)\.$/m);
  if (!head) return null;
  const [, title, sessionId, status] = head;
  const filesLine = text.match(/^Changed files on its branch: (.+)\.$/m);
  // An orphan note says "unknown" (the server restarted; the list is gone) —
  // that's a fact about our knowledge, not a file named "unknown".
  const filesUnknown = filesLine?.[1] === "unknown";
  const files = filesLine && filesLine[1] !== "none" && !filesUnknown
    ? filesLine[1].split(", ").map((f) => f.trim())
    : [];
  const reportMatch = text.match(/^Report:\n([\s\S]*?)\nIts changes stay on its own branch/m);
  const report = reportMatch ? reportMatch[1].trim() : null;
  // Only a "finished…" note is a success; FAILED / STOPPED / ORPHANED are not.
  return {
    title,
    sessionId,
    status,
    ok: status.startsWith("finished"),
    files,
    filesUnknown,
    report,
  };
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
  // Amber = stopped/attention (interrupted or orphaned — an infra restart, not
  // the agent's fault); red stays reserved for a genuine failure.
  const halted = note.status.startsWith("ORPHANED") || note.status.startsWith("STOPPED");
  const dot = note.ok ? green("◆") : halted ? yellow("◆") : red("◆");
  const statusTag = note.ok
    ? green(note.status)
    : note.status.startsWith("ORPHANED")
    ? yellow("◼ interrupted — server restarted")
    : halted
    ? yellow(note.status)
    : red(note.status);
  const title = note.title.replace(/^subagent · /, "");
  out.push({ text: `${dot} ${bold(title)}  ${statusTag}`, click: open });
  const fileNote = note.filesUnknown
    ? "changed files unknown"
    : note.files.length
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
    // No key opens a card from the transcript (the ◆ header is mouse-only), so
    // the hint names ^f — the conversation tree lists and opens subagents — and
    // never promises an enter binding that doesn't exist.
    dim(`  ↳ click to open (or ^f) · adopt("…") in a turn to merge its changes`),
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
 * `fullKey` — toggling it re-renders the block uncapped. `surface: true` paints
 * a subtly raised background across the block (tool input/output; thinking
 * stays bare — it's process, kept quiet).
 */
function pushBlock(
  out: VLine[],
  text: string,
  width: number,
  opts: {
    maxLines: number;
    style: (l: string) => string;
    click: string;
    fullKey?: string;
    surface?: boolean;
  },
) {
  const finish = (l: string) => (opts.surface ? surface(l, width) : l);
  const logical = text.split("\n");
  const shown = logical.slice(0, opts.maxLines);
  for (const line of shown) {
    for (const l of wrap(line, width - 2)) {
      out.push({ text: finish(`${dim("│")} ${l ? opts.style(l) : ""}`), click: opts.click });
    }
  }
  if (logical.length > shown.length) {
    out.push({
      text: finish(
        `${dim("│")} ${dim(`… +${logical.length - shown.length} more lines · click to show all`)}`,
      ),
      click: opts.fullKey ?? opts.click,
    });
  }
}

// Harness verdict lines inside run_steps output get their own colors; everything
// else in an output block reads dim (it's the result, not the intent).
function styleOutputLine(line: string, isError: boolean): string {
  // URLs first: output is where served links land (artifact(), ship notes),
  // and a printed link must open on click like one in prose.
  const l = linkifyUrls(line);
  if (isError) return red(l);
  if (line.startsWith("[program error]")) return red(l);
  if (line.startsWith("[done] accepted")) return green(l);
  if (line.startsWith("[done] rejected")) return red(l);
  if (line.startsWith("[check]")) return yellow(l);
  return dim(l);
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
  // The fold glyph stays at text weight (not dim) — it's the affordance that
  // says "expandable"; an all-dim header reads as inert.
  let head = `${expanded ? "▾" : "▸"} ` + dim(
    `${calls.length} tool ${calls.length === 1 ? "call" : "calls"}  ${
      calls.map((c) => c.name).join(" · ")
    }`,
  );
  // Collapsed single-call groups carry a gist of what ran (expanded shows the
  // real thing, multi-call headers are already crowded with names).
  if (!expanded && calls.length === 1) {
    const gist = codeGist(calls[0].input);
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
    const status = !res
      ? yellow("⚙ running")
      : res.isError
      ? red("✗ error")
      : res.interrupted
      ? yellow("⏹ interrupted")
      : green("✓ done");
    // The ◇ marker takes the call's status color — accent green next to red
    // error text misreads as success.
    const mark = res?.isError ? red("◇") : res?.interrupted ? yellow("◇") : green("◇");
    push(out, `${mark} ${call.name} ${status}`, width, key);
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
        surface: true,
      });
    }
    if (res && outputText(res) !== "") {
      out.push({ text: dim("↳ output"), click: key });
      pushBlock(out, outputText(res), width, {
        maxLines: capOut,
        style: (l) => styleOutputLine(l, res.isError),
        click: key,
        fullKey: `${key}!full`,
        surface: true,
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
          surface: true,
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
      // The width lets md() paint fenced code on a raised surface.
      push(seg, md(s.text, w), w);
      copy = s.text;
    } else if (s.kind === "reasoning") {
      // Thinking folds like a tool group: a long reasoning wall is process, not
      // answer. Collapsed = one clickable gist line; expanded = a gutter block
      // (capped like outputs, "+N more" lifts the cap via the !full key).
      // Empty reasoning (thinking happened, text not captured) renders nothing.
      if (!s.text.trim()) return;
      const logical = s.text.split("\n");
      if (isExpanded(key)) {
        // Fold glyph at text weight (see toolGroupLines) — the header must read
        // as clickable; the thinking itself stays dim.
        seg.push({ text: "▾ " + dim(`thinking (${logical.length} lines)`), click: key });
        pushBlock(seg, s.text, w, {
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
      // An attached image renders as a compact placeholder (terminals don't do
      // pixels); the bytes live in ~/.bough/attachments and went to the model.
      const kb = Math.max(1, Math.round(s.part.size / 1024));
      seg.push({ text: dim(`🖼 ${s.part.name} (${kb} KB)`) });
      copy = s.part.path;
    } else if (s.kind === "prose") {
      // prose() — the turn's marked-up answer: full markdown treatment behind an
      // accent gutter, so the final answer stands out from interstitial chatter.
      const physical = md(s.text, w - 2).split("\n").flatMap((line) => wrap(line, w - 2));
      for (const l of physical) seg.push({ text: `${green("▎")} ${l}` });
      copy = s.text;
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
  /** Persisted delegation outcome (session.outcomeOk/outcomeCheckPassed) — the
   * in-band agent() result the parent program saw. Nullable: absent on legacy
   * rows and sessions that never finished a spawned turn. */
  ok?: boolean | null;
  checkPassed?: boolean | null;
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
    // outcome from the session status + persisted {ok, checkPassed} instead of
    // always showing "✓ done": green is reserved for ok AND check passed.
    // Hue semantics: blue = in flight, amber = stopped/attention (interrupted,
    // orphaned by a server restart, or finished with a failing check — not a
    // hard failure), red = failed.
    const { dot, tail } = b.busy
      ? { dot: blue("◆"), tail: blue(" ⋯ working") }
      : b.status === "orphaned"
      ? { dot: yellow("◆"), tail: yellow(" ◼ interrupted — server restarted") }
      : b.status === "interrupted"
      ? { dot: yellow("◆"), tail: yellow(" ◼ interrupted") }
      : b.status === "error" || b.ok === false
      ? { dot: red("◆"), tail: red(" ✗ failed") }
      : b.ok === true && b.checkPassed === false
      ? { dot: yellow("◆"), tail: yellow(" ✓ done (check failed)") }
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
  endedAt?: number;
  status: "running" | "exited" | "killed";
  exitCode?: number;
  signal?: string;
  outputLines: number;
  tailLines: string[];
}

// Minutes used to be the largest unit, so a long-lived (or stale-timestamped)
// job read "527213m 46s". Roll up through hours and days; only the two most
// significant units show — nobody needs seconds on a two-day run.
function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
}

/** The status run of a job card — the one thing you actually look for. */
function jobStatusText(job: BgJob): string {
  if (job.status === "running") return yellow("⋯ running");
  if (job.status === "killed") return red("✗ killed");
  if (job.signal) return red(`✗ ${job.signal}`);
  return job.exitCode === 0 ? green("✓ done") : red(`✗ exit ${job.exitCode}`);
}

// A background shell's card. It persists past the exit: a job that ended used to
// erase its own card and leave nothing but a note written *for the model*
// ("Read it with bashOutput(...)"), so a build that failed while you were reading
// something else left no user-visible trace of having failed at all. Now the card
// stays and states the outcome, and ^b opens the full output.
export function jobCardLines(out: VLine[], job: BgJob, width: number) {
  const w = width - 2;
  const body: VLine[] = [];
  const glyph = job.status === "running"
    ? yellow("⚙")
    : job.status === "exited" && job.exitCode === 0 && !job.signal
    ? green("⚙")
    : red("⚙");
  const took = fmtElapsed((job.endedAt ?? Date.now()) - job.startedAt);
  body.push({
    text: `${glyph} ${bold(job.id)} ${jobStatusText(job)}  ${
      dim(`${clip(job.command, 60)} · ${took}`)
    }`,
  });
  for (const line of job.tailLines) {
    for (const l of wrap(line, w - 2)) body.push({ text: `${dim("│")} ${dim(l)}` });
  }
  // Only worth pointing at the full log when there's more of it than the tail.
  if (job.outputLines > job.tailLines.length) {
    body.push({ text: dim(`  ${job.outputLines} lines total · ^b opens the full output`) });
  }
  const copy = [`${job.id} · ${job.command}`, ...job.tailLines].join("\n");
  out.push({ text: "" });
  out.push(...body.map((l) => ({ ...l, copy, click: "jobs", text: "  " + l.text })));
}

/** The model-facing background note (`postSystemNote` in turn.ts). It exists to
 * wake the agent, not to inform the user — its card says the same thing in the
 * user's language, so the raw note is dropped from the transcript. */
const BG_NOTE_RE = /^\[background\] (bg_\d+) finished/;
export function parseBgNote(text: string): string | null {
  return BG_NOTE_RE.exec(text.trim())?.[1] ?? null;
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
  // Jobs still in the registry render as cards, so their raw wake notes are dropped.
  // Once a job ages out of the registry the note is all that's left — keep it then.
  const jobIds = new Set(jobs.map((j) => j.id));
  const byOrigin = new Map<string, Branch[]>();
  const orphans: Branch[] = [];
  for (const b of branches) {
    // A running subagent lives in the rail under the status bar, not as a card
    // that scrolls out of view; the transcript keeps the finished report.
    if (b.busy && !b.note) continue;
    if (b.originMessageId) {
      byOrigin.set(b.originMessageId, [...(byOrigin.get(b.originMessageId) ?? []), b]);
    } else orphans.push(b);
  }
  const out: VLine[] = [];
  // True once a pending (in-flight) reply has rendered: any user message after it
  // was steered into the running turn and is only *queued* server-side.
  let midTurn = false;
  for (const m of thread) {
    // Skip the raw [subagent finished] system message — its card renders at the
    // spawn point instead.
    if (m.role === "system") {
      const t = m.parts.filter((p) => p.type === "text").map((p) => (p as { text: string }).text)
        .join("\n");
      const parsed = parseSubagentNote(t);
      if (parsed && notedIds.has(parsed.sessionId)) continue;
      // Same for the [background] wake note: its job card carries the outcome.
      const bg = parseBgNote(t);
      if (bg && jobIds.has(bg)) continue;
    }
    out.push(...messageLines(m, isExpanded, isFull, width, streaming[m.id], toolLogs));
    // Honest ack under a steered message: the turn yields only at its next round
    // boundary, and a blocking host call (a parallel fan-out) can hold that off
    // for minutes — silence reads as being ignored. The marker disappears once a
    // later reply follows the message.
    if (midTurn && m.role === "user") {
      out.push({ text: "  " + dim("⏳ queued — the agent will see this after the current step") });
    }
    if (m.pending) midTurn = true;
    for (const b of byOrigin.get(m.id) ?? []) branchCardLines(out, b, width, isFull);
    byOrigin.delete(m.id);
  }
  // Anything left in byOrigin is anchored to a message that isn't in this thread
  // (a fork or compaction dropped the spawn turn). Those keys are never drained
  // by the loop above, so the card used to render nowhere at all — and since the
  // rail was narrowed to busy branches it wasn't a catch-all either. Tail them.
  const tail = [...orphans, ...[...byOrigin.values()].flat()];
  if (tail.length) {
    out.push({ text: "" });
    out.push({ text: "  " + dim("subagents with no spawn point in this thread") });
  }
  for (const b of tail) branchCardLines(out, b, width, isFull);
  // Background shells at the tail — every one of them, including the finished:
  // the outcome is the whole point, and dropping exited jobs meant a failure
  // showed up nowhere at all.
  for (const job of jobs) jobCardLines(out, job, width);
  return out;
}
