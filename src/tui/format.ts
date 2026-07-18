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
// Honor the NO_COLOR convention (https://no-color.org) for the hand-rolled SGR
// paths — ink's own <Text color> already respects it via chalk, but these raw
// escapes would otherwise leak styling into a colorless terminal.
export const COLOR = (Deno.env.get("NO_COLOR") ?? "") === "";
const sgr = (code: string) => (COLOR ? code : "");

const B = sgr("\x1b[1m");
const B_OFF = sgr("\x1b[22m");
const DIM = sgr("\x1b[2m");
const CYAN = sgr("\x1b[36m");
const FG_OFF = sgr("\x1b[39m");

// OSC 8 hyperlink — supporting terminals make the wrapped text clickable, the
// rest ignore the sequence. Zero-width for wrap-ansi/slice-ansi/strip-ansi
// (ansi-regex matches OSC with BEL/ST terminators), so layout math is unchanged.
const osc8 = (url: string, text: string) =>
  COLOR ? `\x1b]8;;${url}\x1b\\${text}\x1b]8;;\x1b\\` : text;

function mdInline(line: string): string {
  // Swap code spans for placeholders so their contents are exempt from prose
  // rewriting, but bold/links still match ACROSS them ("**bold with `code`**"
  // was left as literal asterisks when styling split the line at spans).
  // Rendered links are guarded the same way so the bare-URL pass can't re-match
  // a URL already inside an OSC 8 wrapper (nesting would truncate the link).
  const spans: string[] = [];
  const guard = (rendered: string) => `\x00${spans.push(rendered) - 1}\x00`;
  return line
    .replace(/`[^`]+`/g, (m) => guard(`${CYAN}${m.slice(1, -1)}${FG_OFF}`))
    .replace(/\*\*([^*]+)\*\*/g, `${B}$1${B_OFF}`)
    // [text](url) → clickable underlined text, url dimmed alongside.
    .replace(
      /\[([^\]]+)\]\((\S+?)\)/g,
      (_m, text, url) => guard(osc8(url, `${UL}${text}${UL_OFF} ${DIM}(${url})${B_OFF}`)),
    )
    // Bare URLs become clickable as themselves; trailing punctuation stays prose.
    .replace(/https?:\/\/[^\s)\]>'"]+/g, (m) => {
      const url = m.replace(/[.,;:!?]+$/, "");
      return guard(osc8(url, url)) + m.slice(url.length);
    })
    .replace(/\x00(\d+)\x00/g, (_, i) => spans[+i]);
}

const UL = sgr("\x1b[4m");
const UL_OFF = sgr("\x1b[24m");

// ---- code highlighting -------------------------------------------------------
// A one-pass approximate tokenizer for fenced blocks and tool-call code: strings
// green, comments dim, keywords magenta, numbers yellow, the rest plain. Candy,
// not a parser — a wrong color on an exotic line is fine; flat gray was the bug.
const MAGENTA = sgr("\x1b[35m");
const GREEN = sgr("\x1b[32m");
const YELLOW = sgr("\x1b[33m");

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
// One combined regex per language: strings | keyword | number, applied in a single
// pass so inserted SGR codes are never re-matched (a digits-in-escape hazard).
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

/** Highlight one line of code for the terminal. `langTag` is the fence tag ("" ok). */
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
    (_m, str, kw, num) =>
      str
        ? `${GREEN}${str}${FG_OFF}`
        : kw
        ? `${MAGENTA}${kw}${FG_OFF}`
        : `${YELLOW}${num}${FG_OFF}`,
  );
  return styled + (comment ? `${DIM}${comment}${B_OFF}` : "");
}

export function md(text: string): string {
  let fence: string | null = null; // the open fence's language tag
  return text.split("\n").map((line) => {
    const open = line.match(/^\s*```(\S*)\s*$/);
    if (open) {
      // Fence markers frame the block instead of rendering as raw backticks.
      if (fence === null) {
        fence = open[1];
        return `${DIM}╭ ${fence || "code"}${B_OFF}`;
      }
      fence = null;
      return `${DIM}╰${B_OFF}`;
    }
    if (fence !== null) return `${DIM}│${B_OFF} ${highlightCode(line, fence)}`;
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) return h[1].length === 1 ? `${B}${UL}${h[2]}${UL_OFF}${B_OFF}` : `${B}${h[2]}${B_OFF}`;
    if (/^\s*(-{3,}|\*{3,})\s*$/.test(line)) return `${DIM}${"─".repeat(24)}${B_OFF}`;
    const quoted = line.match(/^>\s?(.*)$/);
    if (quoted) return `${DIM}│ ${quoted[1]}${B_OFF}`;
    return mdInline(line.replace(/^(\s*)- /, "$1• "));
  }).join("\n");
}

/** 1234 → "1.2k", 999 → "999". */
export function fmtTokens(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : `${n}`;
}

/**
 * Fuzzy rank for the composer popups: exact prefix beats word-boundary prefix
 * beats substring beats in-order subsequence; non-matches drop out (score 0).
 */
export function fuzzyScore(candidate: string, query: string): number {
  if (!query) return 1;
  const c = candidate.toLowerCase();
  const q = query.toLowerCase();
  if (c.startsWith(q)) return 4;
  if (c.includes("-" + q) || c.includes("_" + q) || c.includes(" " + q)) return 3;
  if (c.includes(q)) return 2;
  let i = 0;
  for (const ch of c) {
    if (ch === q[i]) i++;
    if (i === q.length) return 1;
  }
  return 0;
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
