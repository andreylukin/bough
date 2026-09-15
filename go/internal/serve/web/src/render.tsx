import { marked } from "marked";
import DOMPurify from "dompurify";
import { useEffect, useRef } from "react";
import type { Line } from "./types";

/**
 * Assistant replies are markdown. Rendering them as plain text — which
 * the first version did — shows every heading, list marker and code
 * fence as literal punctuation.
 *
 * The text comes from a model, via a local agent, and can contain
 * anything including HTML, so it is sanitized rather than trusted.
 */
export function Markdown({ text }: { text: string }) {
  // A wide table scrolls in its own box; a fade on the right says there is more.
  // Trimmed: a trailing newline is a break point that parts inline chips after it from the last word.
  const html = DOMPurify.sanitize(marked.parse(text, { async: false }) as string).trim()
    .replace(/<table>/g, '<div class="md-table"><div class="md-table-scroll" tabindex="0" role="region" aria-label="Table, scrolls sideways"><table>')
    .replace(/<\/table>/g, "</table></div></div>")
    // An empty fence is a recorded fact, not a grey box that looks like loading.
    .replace(/<pre><code[^>]*>\s*<\/code><\/pre>/g, '<p class="md-empty">Empty code block</p>');
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const boxes = [...(ref.current?.querySelectorAll<HTMLElement>(".md-table-scroll") ?? [])];
    const mark = (b: HTMLElement) => {
      const p = b.parentElement!;
      p.toggleAttribute("data-end", b.scrollLeft + b.clientWidth >= b.scrollWidth - 1);
      // Only a table that overflows says how many columns it has and that it scrolls.
      const over = b.scrollWidth > b.clientWidth + 1;
      if (over) p.setAttribute("data-over", `${b.querySelector("tr")?.children.length ?? 0} columns · swipe →`);
      else p.removeAttribute("data-over");
    };
    const on = (e: Event) => mark(e.currentTarget as HTMLElement);
    // A resize or a streamed row changes the overflow, not only a scroll.
    const ro = new ResizeObserver(() => boxes.forEach(mark));
    boxes.forEach((b) => { mark(b); ro.observe(b); if (b.firstElementChild) ro.observe(b.firstElementChild); b.addEventListener("scroll", on, { passive: true }); });
    // A code block copies like a tool output does.
    const copies = [...(ref.current?.querySelectorAll<HTMLElement>("pre") ?? [])].map((pre) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "copy-btn md-copy";
      btn.title = btn.ariaLabel = "Copy code";
      btn.textContent = "Copy";
      btn.onclick = () => navigator.clipboard?.writeText(pre.querySelector("code")?.textContent ?? pre.textContent ?? "").then(() => {
        btn.textContent = "Copied";
        setTimeout(() => { btn.textContent = "Copy"; }, 1400);
      }, () => {});
      pre.classList.add("md-pre");
      pre.appendChild(btn);
      return btn;
    });
    return () => { ro.disconnect(); boxes.forEach((b) => b.removeEventListener("scroll", on)); copies.forEach((c) => c.remove()); };
  }, [html]);
  return <div className="md" ref={ref} dangerouslySetInnerHTML={{ __html: html }} />;
}

/**
 * A changed file's path as a row shows it: relative to the session's cwd,
 * a scratch dir's uuid folded ("scratch/…/notes.md"), the home dir as "~".
 */
export function changedPath(p: string, cwd = ""): string {
  if (cwd && p.startsWith(cwd.replace(/\/$/, "") + "/")) return p.slice(cwd.replace(/\/$/, "").length + 1);
  const scratch = /(?:^|\/)scratch\/[^/]+\/(.+)$/.exec(p);
  if (scratch) return `scratch/…/${scratch[1]}`;
  return p.replace(/^\/(?:Users|home)\/[^/]+\//, "~/");
}

/**
 * A script's first line that says what it does: shebangs, comments, blank
 * lines, `set -e`-style options and a bare `cd` are skipped, so forty rows
 * no longer all read "Ran set -e". Falls back to the first non-blank line.
 */
export function scriptHead(script: string): string {
  const lines = script.split("\n").map((l) => l.trim()).filter(Boolean);
  const boiler = (l: string) => l.startsWith("#") || /^set\s+[-+][a-zA-Z]+(\s+\S+)*$/.test(l) || /^cd\s+("[^"]*"|'[^']*'|\S+)\s*(&&|;)?$/.test(l);
  const pick = lines.find((l) => !boiler(l)) ?? lines[0] ?? "";
  return pick.replace(/^cd\s+("[^"]*"|'[^']*'|\S+)\s*&&\s*/, "").slice(0, 120);
}

/**
 * What a code block is doing, read off the program itself — the same
 * vocabulary the TUI uses (plugins/ui/blocks.go labels bash, write,
 * patch, view and spawn). A bare "Code" header tells you nothing when
 * every turn has one.
 */
export function codeLabel(code: string): { label: string; detail: string } {
  const first = (re: RegExp) => code.match(re)?.[1]?.trim() ?? "";
  // A quoted script spells its newlines "\n".
  const bash = scriptHead((code.match(/tools\.bash\(\s*["'`]([^"'`]{0,4000})/)?.[1] ?? "").replace(/\\n/g, "\n"));
  if (bash) return { label: "Ran", detail: bash };
  const write = first(/tools\.write\(\s*["'`]([^"'`]{0,120})/);
  if (write) return { label: "Wrote", detail: write };
  const patch = first(/tools\.patch\(\s*["'`]([^"'`]{0,120})/);
  if (patch) return { label: "Patched", detail: patch };
  const view = first(/tools\.view\(\s*["'`]([^"'`]{0,120})/);
  if (view) return { label: "Read", detail: view };
  if (/tools\.spawnAll\(/.test(code)) return { label: "Spawned subagents", detail: "" };
  if (/tools\.spawn\(/.test(code)) return { label: "Spawned a subagent", detail: "" };
  if (/tools\.ask\(/.test(code)) return { label: "Asked you", detail: "" };
  return { label: "Code", detail: "" };
}

/** "✔ wrote a, b · exit 1" — the TUI's done summary, same shape. */
export function doneSummary(line: Line): string {
  const d = line.data ?? {};
  const files = Array.isArray(d.files) ? (d.files as string[]) : [];
  const parts: string[] = [];
  if (files.length) parts.push(`wrote ${files.join(", ")}`);
  const exit = d.exit;
  if (typeof exit === "number" && exit !== 0) parts.push(`exit ${exit}`);
  return parts.join(" · ");
}

/** 21043 → "21k", 1_234_000 → "1.2M": a count read at a glance. */
export function tokenCount(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

/** "$0.39"; a fraction of a cent is "<$0.01", not "$0.00". */
export function money(n: number): string {
  return n > 0 && n < 0.01 ? "<$0.01" : `$${n.toFixed(2)}`;
}

/** 252000 → "4m 12s". */
export function duration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/**
 * What a turn spent, as the loop stamps it on the done entry (usage:
 * in/out/cost, and last_in — the context the next turn starts from).
 * cost is absent for a provider that does not price; null means the
 * turn recorded no usage at all.
 */
export interface Usage { in: number; out: number; cost?: number; lastIn: number }

export function usageOf(done: Line | null): Usage | null {
  const u = done?.data?.usage as Record<string, unknown> | undefined;
  if (!u) return null;
  const n = (v: unknown) => (typeof v === "number" ? v : 0);
  return { in: n(u.in), out: n(u.out), cost: typeof u.cost === "number" ? u.cost : undefined, lastIn: n(u.last_in) };
}

/** The session's tally: every turn's usage summed; context is the latest turn's. */
export function sessionUsage(lines: Line[]): Usage | null {
  let seen = false, priced = false;
  let tokensIn = 0, tokensOut = 0, cost = 0, lastIn = 0;
  for (const l of lines) {
    if (l.kind !== "done") continue;
    const u = usageOf(l);
    if (!u) continue;
    seen = true;
    tokensIn += u.in;
    tokensOut += u.out;
    lastIn = u.lastIn;
    if (u.cost !== undefined) { priced = true; cost += u.cost; }
  }
  return seen ? { in: tokensIn, out: tokensOut, cost: priced ? cost : undefined, lastIn } : null;
}

/**
 * A reply carries the fenced program it wants run, and the loop records
 * that program again as its own `code` entry. Rendering both shows the
 * same code twice — the TUI de-duplicates it for the same reason.
 * The loop runs only a reply's first program; any further program
 * fences never ran and have no entry, so they rendered as raw
 * `console.log(tools.bash(...))` boxes. A fence that calls tools is a
 * program, not prose, and is dropped whether it ran or not.
 */
export function stripRunFences(text: string, codes: string[]): string {
  return text.replace(/```[a-zA-Z]*\n([\s\S]*?)```/g, (whole, inner: string) =>
    codes.some((c) => c.trim() === inner.trim()) || /\btools\.\w+\s*\(/.test(inner) ? "" : whole,
  ).replace(/\n{3,}/g, "\n\n").trim();
}

/**
 * A reply whose fence was lost ends in a bare program: marked renders
 * it as one run-on paragraph of escaped JavaScript. Split it off at the
 * first code-shaped line outside any fence, when a tools call follows.
 */
export function splitBareProgram(text: string): [string, string] {
  const lines = text.split("\n");
  let fenced = false;
  for (let i = 0; i < lines.length; i++) {
    if (/^\s*```/.test(lines[i])) { fenced = !fenced; continue; }
    if (fenced || !/^\s*(?:const |let |var |await |console\.log\(|out\.push\()/.test(lines[i])) continue;
    const rest = lines.slice(i).join("\n");
    if (/tools\.\w+\s*\(/.test(rest)) return [lines.slice(0, i).join("\n").replace(/`+\s*$/, "").trim(), rest.trim()];
    return [text, ""];
  }
  return [text, ""];
}

/**
 * Nothing a reader would see. A reply can be a program plus a stray
 * marker the model emitted ("<focus seq=12>"): the sanitiser drops the
 * tag, the Markdown renders empty, and a bare "bough" label was left
 * between two runs of tool calls, splitting them.
 */
export function blank(text: string): boolean {
  return !text.replace(/<[^>\n]*>/g, "").trim();
}

/** No title yet: when it was, not an opaque hex id. The id tail belongs in a secondary chip. */
export function untitled(_id: string, at?: string): string {
  const d = at ? new Date(at) : null;
  return d && !Number.isNaN(d.getTime())
    ? `Untitled · ${d.toLocaleDateString([], { month: "short", day: "numeric" })}`
    : "Untitled session";
}

/** Titles that would read the same once cut to a row's width collide too. */
export function titleKey(t: string): string {
  return t.trim().toLowerCase().slice(0, 40);
}

/** Titles can arrive as raw markdown ("## What I found"). Show the words. */
export function plainTitle(t: string): string {
  return t.replace(/^#{1,6}\s+/, "").replace(/[*_`]/g, "").trim();
}

/** One name for a session on every surface: its title, else the summary's first sentence, else "Untitled · <date>". */
export function sessionTitle(r: { id: string; title?: string; summary?: string; lastAt?: string }): string {
  return plainTitle(r.title ?? "") || (r.summary ?? "").split(/(?<=[.!?])\s/)[0].trim() || untitled(r.id, r.lastAt);
}

/** Whether a session's name is only the fallback, so its id tail should show beside it. */
export function hasOwnTitle(r: { title?: string; summary?: string }): boolean {
  return Boolean(plainTitle(r.title ?? "") || (r.summary ?? "").trim());
}

const QUIET = new Set(["job", "hook", "usage", "system", "nudge", "command", "meta", "title", "turn-summary", "undo"]);

/** Kinds that are bookkeeping, not conversation. */
export function isQuiet(kind: string): boolean {
  return QUIET.has(kind);
}

/**
 * A turn: one prompt and everything the agent did before it stopped.
 * Grouping matters because a long session is a list of turns, not a
 * flat stream of forty entries.
 */
export interface Turn {
  seq: number;
  prompt: Line | null;
  body: Line[];
  done: Line | null;
  /** Ended by a cancel; `done` is then the loop's trailing record when it came. */
  stopped?: boolean;
}

export function groupTurns(lines: Line[]): Turn[] {
  const turns: Turn[] = [];
  let cur: Turn | null = null;
  for (const l of lines) {
    // Turn summaries live in the sidebar's turn log, not the transcript.
    if (l.kind === "meta" || l.kind === "title" || l.kind === "turn-summary") continue;
    if (l.kind === "input") {
      if (cur) turns.push(cur);
      cur = { seq: l.seq, prompt: l, body: [], done: null };
      continue;
    }
    // A cancel is followed by the done the loop always writes. With the
    // turn already closed, that done opened an empty turn of its own and
    // a stopped turn read "Stopped" then "Finished".
    // That done still carries the turn's usage and files: it replaces the
    // cancel as the record, and the turn remembers it was stopped.
    if (!cur && (l.kind === "done" || l.kind === "cancelled") && turns.length && turns[turns.length - 1].done) {
      const last = turns[turns.length - 1];
      if (l.kind === "done" && last.done!.kind === "cancelled") { last.stopped = true; last.done = l; }
      continue;
    }
    if (!cur) cur = { seq: l.seq, prompt: null, body: [], done: null };
    if (l.kind === "done" || l.kind === "cancelled") {
      cur.done = l;
      turns.push(cur);
      cur = null;
      continue;
    }
    cur.body.push(l);
  }
  if (cur) turns.push(cur);
  return turns;
}

/* ---------------- subagents ---------------- */

/**
 * One subagent inside a run. `worker` is the lane the loop stamps on
 * every sub:* entry (data.worker), which is the only thing that tells
 * two concurrent subagents apart — their entries interleave in the
 * transcript in the order they happened to finish a step.
 */
export interface SubAgent {
  worker: string;
  task: string;
  status: string; // "ok" | "error" | "" while it is still working
  steps: number;
  lines: Line[];
  seq: number;
  /** When its first and latest entries were recorded, for elapsed time. */
  from: string;
  to: string;
}

/**
 * A turn body is mostly the parent's own work with runs of subagent
 * work spliced into it. Rendered flat, a subagent's code and results
 * are indistinguishable from the parent's — which is what they looked
 * like. So a contiguous stretch of sub:* entries becomes one item.
 */
export type Item =
  | { kind: "line"; seq: number; line: Line }
  | { kind: "sub"; seq: number; agents: SubAgent[] }
  | { kind: "tools"; seq: number; lines: Line[] };

const TOOL = new Set(["code", "result", "job"]);

/**
 * Several tool calls in a row are one thing the agent did. Shown as a
 * row per call and a row per result, twenty steps of exploring were a
 * wall of equally weighted pills burying the reply after them. A run
 * of two or more calls folds into one item; opened, it lists the calls,
 * and each call still opens onto its output — collapse in levels.
 * A reply that was only the program it ran renders as nothing, so it
 * does not break a run.
 */
export function groupTools(items: Item[], codes: string[]): Item[] {
  const out: Item[] = [];
  let run: Line[] = [];
  const flush = () => {
    // Even a single call is emitted as a run: the renderer pairs a call
    // with its output, and only wraps runs of two or more in a header.
    if (run.some((l) => l.kind === "code")) out.push({ kind: "tools", seq: run[0].seq, lines: run });
    else for (const l of run) out.push({ kind: "line", seq: l.seq, line: l });
    run = [];
  };
  for (const it of items) {
    if (it.kind === "line" && TOOL.has(it.line.kind)) { run.push(it.line); continue; }
    // Reasoning between calls is part of the same stretch of work: a
    // "Thinking" row before every call split every run into singles.
    if (it.kind === "line" && it.line.kind === "thinking") { run.push(it.line); continue; }
    if (it.kind === "line" && it.line.kind === "assistant" && blank(stripRunFences(it.line.text, codes))) continue;
    // An ask sits between the call that asked and that call's result; it
    // renders as its own card, so it must not split the pair or the run.
    if (it.kind === "line" && it.line.kind === "ask") continue;
    flush();
    out.push(it);
  }
  flush();
  return out;
}

/** Hook fires, and the "hook <event>: notice" lines they emit, belong in the turn's hooks row. */
export function isHookLine(l: Line): boolean {
  return l.kind === "hook" || (l.kind === "system" && l.text.startsWith("hook "));
}

/**
 * The loop records a retry twice: its note to the model ("[unfinished]
 * …") and a line for you ("that reply ran nothing …; asking again (1/2)").
 * Folded onto the note, they render as one row.
 */
export function foldRetries(body: Line[]): Line[] {
  const out: Line[] = [];
  for (const l of body) {
    const m = l.kind === "system" ? /^that reply (.+); asking again \((\d+\/\d+)\)$/.exec(l.text) : null;
    const prev = out[out.length - 1];
    if (m && prev?.kind === "nudge") {
      out[out.length - 1] = { ...prev, data: { ...prev.data, retry: m[2], why: m[1] } };
      continue;
    }
    out.push(l);
  }
  return out;
}

/**
 * A /model switch lands as the command, the command with its argument,
 * and the loop's "model: …" echo. A run of them folds into one line
 * naming where it ended up; the verbatim record rides along.
 */
export function foldModelSwitch(body: Line[]): Line[] {
  const out: Line[] = [];
  for (const l of body) {
    const is = isQuiet(l.kind) && /^(\/model\b|model: )/.test(l.text);
    const prev = out[out.length - 1];
    if (!is) { out.push(l); continue; }
    const recs = prev?.kind === "model-switch" ? [...(prev.data?.lines as string[]), l.text] : [l.text];
    const to = [...recs].reverse().find((t) => t.startsWith("model: "))?.slice(7)
      ?? [...recs].reverse().find((t) => /^\/model \S/.test(t))?.slice(7) ?? "";
    const next: Line = { ...(prev?.kind === "model-switch" ? prev : l), kind: "model-switch", text: to, data: { lines: recs } };
    if (prev?.kind === "model-switch") out[out.length - 1] = next; else out.push(next);
  }
  return out;
}

function readStr(v: unknown): string {
  return typeof v === "string" ? v : typeof v === "number" ? String(v) : "";
}

/** A task recorded as a structured payload names its words, never "[object Object]". */
function taskText(l: Line): string {
  const d = l.data?.text as unknown;
  const o = (d && typeof d === "object" ? d : l.data?.task) as Record<string, unknown> | undefined;
  if (o && typeof o === "object") return readStr(o.task) || readStr(o.prompt) || readStr(o.text);
  return l.text === "[object Object]" ? "" : l.text;
}

export function groupSubs(body: Line[]): Item[] {
  const out: Item[] = [];
  let run: { kind: "sub"; seq: number; agents: SubAgent[] } | null = null;
  // Lanes live for the whole turn: a parent entry landing between two
  // steps of the same worker (a /sessions command, say) used to split
  // that worker into a second card. It keeps the card it started in.
  const lane = new Map<string, SubAgent>();

  for (const l of body) {
    if (!l.kind.startsWith("sub:")) {
      run = null;
      out.push({ kind: "line", seq: l.seq, line: l });
      continue;
    }
    const worker = readStr(l.data?.worker) || "1";
    let a = lane.get(worker);
    // A finished worker's lane is free again: a new start is a new agent.
    if (a && a.status && l.kind === "sub:start") a = undefined;
    if (!a) {
      if (!run) {
        run = { kind: "sub", seq: l.seq, agents: [] };
        out.push(run);
      }
      a = { worker, task: "", status: "", steps: 0, lines: [], seq: l.seq, from: l.at, to: l.at };
      lane.set(worker, a);
      run.agents.push(a);
    }
    a.to = l.at;
    if (l.kind === "sub:start") { a.task = taskText(l); continue; }
    if (l.kind === "sub:done") {
      a.status = readStr(l.data?.status) || "ok";
      const n = l.data?.steps;
      a.steps = typeof n === "number" ? n : 0;
      continue;
    }
    a.lines.push(l);
  }
  return out;
}

/** "1 step" / "4 steps" — the same English as lineCount. */
export function stepCount(n: number): string {
  return n === 1 ? "1 step" : `${n} steps`;
}

/** "1 line" / "4 lines" — a count that reads as English. */
export function lineCount(n: number): string {
  return n === 1 ? "1 line" : `${n} lines`;
}

/**
 * The loop's note that later blocks of a reply did not run, as a count and
 * the loop's own reason: "[2 further code block(s) dropped — …]" or
 * "[the 2 code block(s) after this one … were not run: this block failed.]".
 */
export function execNote(text: string): { notRun: number; reason: string } | null {
  const m = /\[(\d+) further code block\(s\) dropped\s*(?:—\s*)?([^\]]*)\]/.exec(text)
    ?? /\[the (\d+) code block\(s\) after this one[^\]]*?were not run:?\s*([^\]]*)\]/.exec(text);
  if (!m) return null;
  return { notRun: Number(m[1]), reason: m[2].trim().replace(/\.$/, "") };
}

/** A result line's label: never payload characters, a SubRun's results counted. */
export function resultLabel(subRunCount?: number | null): string {
  return subRunCount ? `Subagent results · ${subRunCount}` : "Result";
}
