import { marked } from "marked";
import DOMPurify from "dompurify";
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
  const html = DOMPurify.sanitize(marked.parse(text, { async: false }) as string);
  return <div className="md" dangerouslySetInnerHTML={{ __html: html }} />;
}

/**
 * What a code block is doing, read off the program itself — the same
 * vocabulary the TUI uses (plugins/ui/blocks.go labels bash, write,
 * patch, view and spawn). A bare "Code" header tells you nothing when
 * every turn has one.
 */
export function codeLabel(code: string): { label: string; detail: string } {
  const first = (re: RegExp) => code.match(re)?.[1]?.trim() ?? "";
  const bash = first(/tools\.bash\(\s*["'`]([^"'`]{0,120})/);
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

/**
 * A reply carries the fenced program it wants run, and the loop records
 * that program again as its own `code` entry. Rendering both shows the
 * same code twice — the TUI de-duplicates it for the same reason.
 */
export function stripRunFences(text: string, codes: string[]): string {
  if (!codes.length) return text;
  return text.replace(/```[a-zA-Z]*\n([\s\S]*?)```/g, (whole, inner: string) =>
    codes.some((c) => c.trim() === inner.trim()) ? "" : whole,
  ).replace(/\n{3,}/g, "\n\n").trim();
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

/** Five "Untitled session" rows are indistinguishable; an id tail is not. */
export function untitled(id: string): string {
  return "Session " + id.slice(-6);
}

/** Titles can arrive as raw markdown ("## What I found"). Show the words. */
export function plainTitle(t: string): string {
  return t.replace(/^#{1,6}\s+/, "").replace(/[*_`]/g, "").trim();
}

const QUIET = new Set(["job", "hook", "usage", "system", "nudge", "command", "meta", "title", "undo"]);

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
}

export function groupTurns(lines: Line[]): Turn[] {
  const turns: Turn[] = [];
  let cur: Turn | null = null;
  for (const l of lines) {
    if (l.kind === "meta" || l.kind === "title") continue;
    if (l.kind === "input") {
      if (cur) turns.push(cur);
      cur = { seq: l.seq, prompt: l, body: [], done: null };
      continue;
    }
    // A cancel is followed by the done the loop always writes. With the
    // turn already closed, that done opened an empty turn of its own and
    // a stopped turn read "Stopped" then "Finished".
    if (!cur && (l.kind === "done" || l.kind === "cancelled") && turns.length && turns[turns.length - 1].done) continue;
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

function readStr(v: unknown): string {
  return typeof v === "string" ? v : typeof v === "number" ? String(v) : "";
}

export function groupSubs(body: Line[]): Item[] {
  const out: Item[] = [];
  let run: { kind: "sub"; seq: number; agents: SubAgent[] } | null = null;
  let lane = new Map<string, SubAgent>();

  for (const l of body) {
    if (!l.kind.startsWith("sub:")) {
      run = null;
      lane = new Map();
      out.push({ kind: "line", seq: l.seq, line: l });
      continue;
    }
    const worker = readStr(l.data?.worker) || "1";
    if (!run) {
      run = { kind: "sub", seq: l.seq, agents: [] };
      out.push(run);
    }
    let a = lane.get(worker);
    if (!a) {
      a = { worker, task: "", status: "", steps: 0, lines: [], seq: l.seq };
      lane.set(worker, a);
      run.agents.push(a);
    }
    if (l.kind === "sub:start") { a.task = l.text; continue; }
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
