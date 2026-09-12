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

/** Titles can arrive as raw markdown ("## What I found"). Show the words. */
export function plainTitle(t: string): string {
  return t.replace(/^#{1,6}\s+/, "").replace(/[*_`]/g, "").trim();
}

const QUIET = new Set(["job", "usage", "system", "nudge", "command", "meta", "title", "undo"]);

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

/** "1 line" / "4 lines" — a count that reads as English. */
export function lineCount(n: number): string {
  return n === 1 ? "1 line" : `${n} lines`;
}
