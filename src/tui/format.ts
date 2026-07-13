// Part folding + text helpers — port of segmentParts and the tool-group header
// rules from web/src/components/Conversation.tsx, minus the DOM.
import type { Part } from "../schema/parts.ts";

export type ToolCall = Extract<Part, { type: "tool_call" }>;
export type ToolResult = Extract<Part, { type: "tool_result" }>;

export type Segment =
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "tools"; parts: Part[] };

// Group a turn's parts into renderable segments, preserving their order. Consecutive
// tool_call/tool_result parts fold into one collapsible group between prose blocks.
export function segmentParts(parts: Part[]): Segment[] {
  const segs: Segment[] = [];
  for (const p of parts) {
    if (p.type === "text") segs.push({ kind: "text", text: p.text });
    else if (p.type === "reasoning") segs.push({ kind: "reasoning", text: p.text });
    else {
      const last = segs[segs.length - 1];
      if (last?.kind === "tools") last.parts.push(p);
      else segs.push({ kind: "tools", parts: [p] });
    }
  }
  return segs;
}

export function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}

export function outputText(r: ToolResult): string {
  return typeof r.output === "string" ? r.output : JSON.stringify(r.output);
}

/** Collapsed-header facts for a tools segment: names, running call, error/verdict. */
export function toolSummary(parts: Part[]) {
  const calls = parts.filter((p): p is ToolCall => p.type === "tool_call");
  const results = new Map(
    parts.filter((p): p is ToolResult => p.type === "tool_result").map((p) => [p.callId, p]),
  );
  const running = calls.find((c) => !results.has(c.id));
  const outputs = [...results.values()].map(outputText);
  // Harness verdict (worker check gating) — visible without expanding the fold.
  const verdict = outputs.some((o) => o.includes("[done] accepted"))
    ? { text: "✓ check passed", ok: true }
    : outputs.some((o) => o.includes("[done] rejected"))
    ? { text: "✗ check failed", ok: false }
    : null;
  const hasError = [...results.values()].some((r) => r.isError);
  return { calls, results, running, verdict, hasError };
}

// ---- markdown-lite ----------------------------------------------------------
// Terminal styling for prose messages: headings/bold via SGR bold, `code` spans
// cyan, fenced blocks dim, "- " bullets prettified. Deliberately conservative —
// italic/links/tables are left as-is (ink's wrap handles ANSI widths correctly).
const B = "\x1b[1m";
const B_OFF = "\x1b[22m";
const DIM = "\x1b[2m";
const CYAN = "\x1b[36m";
const FG_OFF = "\x1b[39m";

function mdInline(line: string): string {
  // Style code spans first so their contents are exempt from bold rewriting.
  return line
    .split(/(`[^`]+`)/)
    .map((seg) =>
      seg.startsWith("`") && seg.endsWith("`") && seg.length > 2
        ? `${CYAN}${seg.slice(1, -1)}${FG_OFF}`
        : seg.replace(/\*\*([^*]+)\*\*/g, `${B}$1${B_OFF}`)
    )
    .join("");
}

export function md(text: string): string {
  let inFence = false;
  return text.split("\n").map((line) => {
    if (/^\s*```/.test(line)) {
      inFence = !inFence;
      return `${DIM}${line}${B_OFF}`;
    }
    if (inFence) return `${DIM}${line}${B_OFF}`;
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) return `${B}${h[2]}${B_OFF}`;
    return mdInline(line.replace(/^(\s*)- /, "$1• "));
  }).join("\n");
}

/** 1234 → "1.2k", 999 → "999". */
export function fmtTokens(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : `${n}`;
}

// Readline-style word boundaries for the composer (⌥b/⌥f, ctrl+w).
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

export function relTime(ts: number): string {
  const s = Math.max(0, Math.round((Date.now() - ts) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}
