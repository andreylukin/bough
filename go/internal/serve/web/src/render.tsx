import { marked } from "marked";
import DOMPurify from "dompurify";
import { useEffect, useRef } from "react";
import type { Line } from "./types";
import { highlight } from "./code";

/**
 * Assistant replies are markdown. Rendering them as plain text — which
 * the first version did — shows every heading, list marker and code
 * fence as literal punctuation.
 *
 * The text comes from a model, via a local agent, and can contain
 * anything including HTML, so it is sanitized rather than trusted.
 */
export function Markdown({ text, live }: { text: string; /** Still streaming: hold back half-written syntax, skip highlighting. */ live?: boolean }) {
  // A wide table scrolls in its own box; a fade on the right says there is more.
  // Trimmed: a trailing newline is a break point that parts inline chips after it from the last word.
  const clean = DOMPurify.sanitize(marked.parse(live ? holdPartial(text) : text, { async: false }) as string).trim();
  const html = (live ? clean : highlightFences(clean))
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
      const over = tableOverflows(b.scrollWidth, b.clientWidth);
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

/** A few pixels of overflow is rounding, not a table worth swiping. */
export function tableOverflows(scrollWidth: number, clientWidth: number): boolean {
  return scrollWidth - clientWidth > 8;
}

/**
 * A streamed reply cut mid-token: "It exports `sub(a" rendered a raw
 * backtick until the closing one arrived. A trailing unclosed inline
 * backtick or link bracket on the last line is held back until it closes
 * or the stream ends. Inside an open fence nothing is held: that is code.
 */
export function holdPartial(text: string): string {
  const fences = (text.match(/^\s*```/gm) ?? []).length;
  if (fences % 2) return text;
  const start = text.lastIndexOf("\n") + 1;
  const last = text.slice(start);
  const ticks = [...last.matchAll(/`+/g)];
  let cut = ticks.length % 2 ? start + ticks[ticks.length - 1].index : text.length;
  const open = last.lastIndexOf("[");
  if (open >= 0 && !last.slice(open).includes("]")) cut = Math.min(cut, start + open);
  return text.slice(0, cut);
}

const unescape = (s: string) => s.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, "&");

/** A finished fence in a known language gets the same token colours as a tool call's code. */
export function highlightFences(html: string): string {
  return html.replace(/<pre><code class="language-([\w+-]+)">([\s\S]*?)<\/code><\/pre>/g, (whole, lang: string, body: string) => {
    const out = highlight(unescape(body), lang);
    return out === null ? whole : `<pre><code class="language-${lang} hljs">${out}</code></pre>`;
  });
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
/** The live row says what is happening now: "Running go test", not "Ran". */
const PRESENT: Record<string, string> = { Ran: "Running", Wrote: "Writing", Patched: "Patching", Read: "Reading", "Spawned subagents": "Spawning subagents", "Spawned a subagent": "Spawning a subagent", "Asked you": "Asking you" };
export const presentTense = (label: string) => PRESENT[label] ?? label;

/**
 * A block's per-call rows: the tools plugin records one "call" entry per
 * foreground tools.bash/view/write/patch (data.tool, ms, exit, add/del,
 * error), and announces each as it starts (live only, data.phase "start").
 * These are what a block did, read from the runtime, not guessed from
 * its source text.
 */
const CALL_VERBS: Record<string, string> = { bash: "Ran", view: "Read", write: "Wrote", patch: "Patched", spawn: "Spawned" };
export const callVerb = (tool: string) => CALL_VERBS[tool] ?? tool.charAt(0).toUpperCase() + tool.slice(1);
export const isCall = (l: Line) => l.kind === "call" || l.kind === "sub:call";
/**
 * An engine session's call is a row of its own, not a detail of a code
 * block: the model called the tool directly. Its id is the provider's
 * call id (a string); the loop's per-block calls number theirs.
 */
export const isNativeCall = (l: Line) => isCall(l) && typeof l.data?.id === "string";
/** A native call still running: the live start, never recorded. */
export const callRunning = (l: Line) => l.data?.phase === "start";
export const callFailed = (l: Line) => typeof l.data?.error === "string" || (typeof l.data?.exit === "number" && l.data.exit !== 0);
/** Stopped by the person: a call's or a program's end that is a stop, not a failure. */
export const canceled = (l?: Line) => l?.data?.canceled === true;
/** "Running go test ./...": what a call in flight is doing. */
export const callStep = (l: { data?: Record<string, unknown>; text: string }) => [presentTense(callVerb(String(l.data?.tool ?? ""))), l.text].filter(Boolean).join(" ");

/** "Ran 2 commands, read 3 files, edited 1 file": a block named by what it did. */
export function callsHeadline(calls: Line[]): string {
  const n = (k: (l: Line) => boolean) => calls.filter(k).length;
  const cmds = n((l) => l.data?.tool === "bash"), reads = n((l) => l.data?.tool === "view");
  const edits = new Set(calls.filter((l) => l.data?.tool === "write" || l.data?.tool === "patch").map((l) => l.text)).size;
  const plural = (k: number, w: string) => `${k} ${w}${k === 1 ? "" : "s"}`;
  const parts = [cmds ? `ran ${plural(cmds, "command")}` : "", reads ? `read ${plural(reads, "file")}` : "", edits ? `edited ${plural(edits, "file")}` : ""].filter(Boolean);
  if (!parts.length) return calls.length === 1 ? callVerb(String(calls[0].data?.tool ?? "")) : `${calls.length} calls`;
  return parts.join(", ").replace(/^./, (c) => c.toUpperCase());
}
/** R4-D: once its result lands the step reads done: "Running ls" becomes "Ran ls". */
const pastTense = (step: string) => { for (const [done, now] of Object.entries(PRESENT)) if (step === now || step.startsWith(now + " ")) return done + step.slice(now.length); return step; };

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
  let cut = false;
  for (const l of lines) {
    // R4-D: a cancelled turn's figure is partial; it may raise the context reading, never lower it.
    if (l.kind === "cancelled") { cut = true; continue; }
    if (l.kind !== "done") { if (l.kind === "input") cut = false; continue; }
    const u = usageOf(l);
    const partial = cut || l.data?.kind === "cancelled";
    cut = false;
    if (!u) continue;
    seen = true;
    tokensIn += u.in;
    tokensOut += u.out;
    if (!partial || u.lastIn > lastIn) lastIn = u.lastIn;
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

/** A bare program whose text (ignoring whitespace) is inside a code the loop recorded: a duplicate of a run, not an unrun program. */
export function programRan(program: string, codes: string[]): boolean {
  const squash = (t: string) => t.replace(/\s+/g, "");
  const p = squash(program);
  return !!p && squash(codes.join("")).includes(p);
}

/**
 * A reply whose fence was lost carries a bare program: marked renders
 * it as run-on paragraphs of escaped JavaScript. Every run of lines
 * outside a fence that starts code-shaped (await/const/tools.* …) and
 * calls a tool is program text, wherever it sits between the prose.
 * A run continues while a template literal, string or bracket is still
 * open, and across blank lines when the next line is code-shaped too.
 * Returns [prose, program].
 */
const CODE_START = /^\s*(?:const |let |var |await |console\.log\(|out\.push\(|tools\.\w+\s*\()/;
export function splitBareProgram(text: string): [string, string] {
  const lines = text.split("\n");
  const prose: string[] = [];
  const runs: string[] = [];
  let fenced = false;
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (/^\s*```/.test(line)) fenced = !fenced;
    if (fenced || /^\s*```/.test(line) || !CODE_START.test(line)) { prose.push(line); i++; continue; }
    // Scan the run: track open template literals, quotes and brackets across lines.
    let tpl = false, depth = 0, j = i;
    for (;;) {
      let quote = "", prev = "";
      for (const ch of lines[j]) {
        if (tpl) { if (ch === "`") tpl = false; continue; }
        if (quote) { if (ch === quote) quote = ""; continue; }
        if (ch === "/" && prev === "/") break; // line comment
        prev = ch;
        if (ch === "`") tpl = true;
        else if (ch === '"' || ch === "'") quote = ch;
        else if ("([{".includes(ch)) depth++;
        else if (")]}".includes(ch)) depth--;
      }
      j++;
      if (j >= lines.length) break;
      if (tpl || depth > 0) continue;
      let k = j;
      while (k < lines.length && !lines[k].trim()) k++;
      if (k < lines.length && CODE_START.test(lines[k])) { j = k; continue; }
      break;
    }
    const run = lines.slice(i, j).join("\n").trim();
    if (/\btools\.\w+\s*\(/.test(run)) runs.push(run);
    else prose.push(...lines.slice(i, j));
    i = j;
  }
  if (!runs.length) return [text, ""];
  const body = prose.join("\n").replace(/`+\s*$/, "").replace(/\n{3,}/g, "\n\n").trim();
  return [body, runs.join("\n\n")];
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

const QUIET = new Set(["job", "hook", "usage", "system", "nudge", "command", "meta", "origin", "title", "turn-summary", "undo", "model", "engine"]);

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
  /** Of the calls its done left running (done.running), how many have reported since. */
  settled?: number;
}

export function groupTurns(lines: Line[]): Turn[] {
  const turns: Turn[] = [];
  let cur: Turn | null = null;
  // Turns whose done left calls running, with the call ids their job
  // lines named and the ones whose end has since been recorded: the
  // footer's "still running" must stop once they report.
  const ranOn: { turn: Turn; ids: Set<string>; ended: Set<string> }[] = [];
  const settle = (l: Line) => {
    const id = l.kind === "call" ? l.data?.id : l.kind === "job" && l.data?.event === "finished" ? l.data?.call : undefined;
    if (typeof id !== "string") return;
    const owner = ranOn.find((r) => r.ids.has(id))
      // An adopted end whose turn named no ids goes to the latest turn still waiting on one.
      ?? (l.data?.adopted ? [...ranOn].reverse().find((r) => r.ids.size === 0 && r.ended.size < Number(r.turn.done?.data?.running)) : undefined);
    if (owner) { owner.ended.add(id); owner.turn.settled = owner.ended.size; }
  };
  const track = (t: Turn) => {
    if (!(Number(t.done?.data?.running) > 0)) return;
    const ids = new Set(t.body.filter((b) => b.kind === "job" && b.data?.event === "started" && typeof b.data?.call === "string").map((b) => String(b.data!.call)));
    ranOn.push({ turn: t, ids, ended: new Set() });
  };
  // The turn each subagent lane started in. On the engine a subagent
  // runs on past its parent's done, so its steps and finish are recorded
  // in a later turn; shown there they are an untitled agent, and the turn
  // that spawned it never learns how it ended. They go back to its card.
  const lanes = new Map<string, Turn>();
  for (const l of lines) {
    settle(l);
    if (l.kind.startsWith("sub:")) {
      const worker = readStr(l.data?.worker) || "1";
      const home = lanes.get(worker);
      if (l.kind === "sub:start") { if (cur) lanes.set(worker, cur); }
      else if (home && home !== cur) {
        home.body.push(l);
        if (l.kind === "sub:done") lanes.delete(worker);
        continue;
      } else if (l.kind === "sub:done") lanes.delete(worker);
    }
    // Turn summaries live in the sidebar's turn log, not the transcript.
    // The engine entry is the coordinator's build record: provenance for tools, not conversation.
    // A stored notice is shown on its own (storedNotices), then as its wake turn: never as a turn of its own.
    if (l.kind === "meta" || l.kind === "origin" || l.kind === "title" || l.kind === "turn-summary" || l.kind === "model" || l.kind === "engine" || l.kind === "notice" || l.kind === "notice-delivered") continue;
    // R3-C: a steer the open turn took belongs to that turn, not a new one.
    if (l.kind === "input" && l.data?.steer && cur?.prompt) { cur.body.push(l); continue; }
    // A background call's end is recorded between turns, just before the
    // wake turn it starts: that call is what the wake turn is about, so it
    // opens that turn rather than standing alone above it.
    if (l.kind === "input" && l.data?.wake && cur && !cur.prompt && !cur.done) { cur.prompt = l; cur.seq = Math.min(cur.seq, l.seq); continue; }
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
      // The engine's step or cost budget cancels the turn and writes only
      // a done that says why ("stop"); it read "Done" as if the agent had
      // finished. "error" is a failed turn, which says so itself.
      if (l.kind === "done" && typeof l.data?.stop === "string" && l.data.stop !== "error") cur.stopped = true;
      turns.push(cur);
      track(cur);
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

const TOOL = new Set(["code", "result", "job", "call"]);

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
  // Lines that landed while a program in the run still ran: shown after
  // the run, in their order (see below).
  let held: Item[] = [];
  const flush = () => {
    // Even a single call is emitted as a run: the renderer pairs a call
    // with its output, and only wraps runs of two or more in a header.
    if (run.some((l) => l.kind === "code" || isNativeCall(l))) out.push({ kind: "tools", seq: run[0].seq, lines: run });
    else for (const l of run) out.push({ kind: "line", seq: l.seq, line: l });
    run = [];
    out.push(...held);
    held = [];
  };
  for (const [i, it] of items.entries()) {
    // A background agent's finish note is not part of the work around it.
    if (it.kind === "line" && isAgentNotice(it.line)) { flush(); out.push(it); continue; }
    if (it.kind === "line" && TOOL.has(it.line.kind)) { run.push(it.line); continue; }
    // Reasoning between calls is part of the same stretch of work: a
    // "Thinking" row before every call split every run into singles.
    if (it.kind === "line" && it.line.kind === "thinking") { run.push(it.line); continue; }
    if (it.kind === "line" && it.line.kind === "assistant" && blank(stripRunFences(it.line.text, codes))) continue;
    // An ask sits between the call that asked and that call's result; it
    // renders as its own card, so it must not split the pair or the run.
    if (it.kind === "line" && it.line.kind === "ask") continue;
    // On the engine a reply's calls run in parallel: an answer to the ask
    // beside a program, a steer or a provider error is recorded before the
    // program's result. Splitting the run there parted the program from
    // its result, and it read "Running" for good.
    const open = run.filter((l) => l.kind === "code").length > run.filter((l) => l.kind === "result").length;
    if (open && it.kind === "line" && items.slice(i + 1).some((x) => x.kind === "line" && x.line.kind === "result")) {
      held.push(it);
      continue;
    }
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

/* ---------------- work segments ---------------- */

/**
 * A turn as it reads: the agent's replies, and the stretches of work
 * between them folded to one row each ("Worked for 2m 13s · 14 actions").
 * Twenty rows of calls, thoughts and notes buried the two sentences you
 * came to read.
 *
 * - reply: assistant prose that renders visible text. A reply that was
 *   only the program it ran (or the loop's skipped-blocks note) is not.
 * - pinned: a subagent run still working on a live turn. It is live
 *   status, so it never hides inside a fold (and it splits the work).
 * - work: everything else between replies, in order.
 *
 * The collapse rule the renderer applies: a finished segment of a single
 * row (one call, one thought, one plan row, one card) renders bare —
 * wrapping one row in another row is a click for nothing. The running
 * segment is always a row: it is the turn's working indicator.
 */
export type Segment =
  /** early: the reply's words came with its own program, so they follow the work it started. */
  | { kind: "reply"; item: Item; early?: boolean }
  | { kind: "pinned"; item: Item }
  /** A background agent's finish notice that landed inside a turn: its own row, not that turn's work. */
  | { kind: "notice"; item: Item }
  | { kind: "work"; seq: number; items: Item[]; seqs: number[];
      /** Rows as rendered: consecutive todo or job records share one. */
      rows: number;
      /** Tool calls + jobs + subagents + todo changes. */
      actions: number; failed: number; thinkingOnly: boolean;
      /** First and last recorded entry; a lone thought ends when the next entry landed. */
      from: string; to: string;
      /** The latest thing it did, for a running row. */
      step: string;
      /** "ran 2 commands · read 1 file": what the row holds, counted from native calls. */
      what?: string;
      /** No reply follows it in the turn. */
      last: boolean };

type WorkSegment = Extract<Segment, { kind: "work" }>;

const NOTE_RE = /\[(?:\d+ further code block\(s\) dropped|the \d+ code block\(s\) after this one)[^\]]*\]/g;

/** Whether an assistant line renders words, not just a program or a skipped-blocks note. */
export function isReply(l: Line, codes: string[]): boolean {
  if (l.kind !== "assistant") return false;
  return !blank(splitBareProgram(stripRunFences(l.text.replace(NOTE_RE, ""), codes))[0]);
}

/**
 * The error a block threw, when it threw: the recorded field, or for older
 * results the loop's "error: " line ending the output. A program that ran
 * bash with exit 0 and then threw still failed.
 */
export function thrownError(l?: Line): string | undefined {
  if (!l || l.kind !== "result") return undefined;
  if (typeof l.data?.error === "string" && l.data.error) return l.data.error;
  const text = l.text.replace(/(?:\s*\[[^\]\n]*\])+\s*$/, "").trimEnd(); // the loop's trailing notes
  if (/^error\b/i.test(text)) return text.replace(/^error:?\s*/i, "");
  return /(?:^|\n)error: ([^\n]*)$/.exec(text)?.[1];
}

/** An error as a person reads it: no runtime's "GoError:" and no temp script path. */
export function cleanError(text: string): string {
  return text.replace(/\bGoError:\s*/g, "").replace(/(?:\/private)?\/(?:var\/folders|tmp)\/\S*?bough-[\w-]+\.(?:sh|js):\s*/g, "");
}

/** "[agent <title> · <id> finished] <reply>": the note a background agent leaves when it ends. */
export const isAgentNotice = (l: Line) => l.kind === "job" && /^\[agent [^\]]* (?:finished|failed|stopped)\]/.test(l.text ?? "");

/**
 * Reports serve stored for a session with no process ("notice"), as the
 * job lines a live session records them as, until its loop takes them
 * ("notice-delivered") and they become its wake turn. Without this, a
 * parent that was not running showed none of its agents' reports.
 */
export function storedNotices(lines: Line[]): Line[] {
  const taken = new Set(lines.filter((l) => l.kind === "notice-delivered").map((l) => String(l.data?.id ?? "")));
  return lines.filter((l) => l.kind === "notice" && !taken.has(String(l.data?.id ?? ""))).map((l) => ({ ...l, kind: "job" }));
}

const jobFailed = (l: Line) => {
  const d = l.data ?? {};
  if (typeof d.exit === "number") return d.exit !== 0;
  if (d.status === "failed") return true;
  return /^job \d+ \[(?:failed|exited -?[1-9]\d*)\]/.test(String(d.text ?? l.text ?? ""));
};

export function splitWork(items: Item[], codes: string[], live: boolean): Segment[] {
  const out: Segment[] = [];
  let cur: Item[] = [];
  const flush = () => {
    if (!cur.length) return;
    const lines: Line[] = [];
    let actions = 0, failed = 0, rows = 0;
    let step = "";
    const jobs = new Set<string>();
    cur.forEach((it, i) => {
      const prev = cur[i - 1];
      const run = (k: (l: Line) => boolean) => it.kind === "line" && k(it.line) && prev?.kind === "line" && k(prev.line);
      // A run of one call is not wrapped: its thought, call and notes are rows of their own.
      // A call row lives inside its block's row, so it is not a row of the segment.
      // An engine's native call is a row of its own, as a block is.
      if (it.kind === "tools" && it.lines.filter((l) => l.kind === "code" || isNativeCall(l)).length < 2) rows += it.lines.filter((l) => l.kind !== "result" && (l.kind !== "call" || isNativeCall(l))).length;
      else if (!run((l) => l.kind.startsWith("todo/")) && !run((l) => l.kind === "job")) rows++;
      if (it.kind === "sub") {
        actions += it.agents.length;
        failed += it.agents.filter((a) => a.status === "error").length;
        for (const a of it.agents) lines.push(...a.lines, { seq: a.seq, at: a.from, kind: "sub:start", text: "" }, { seq: a.seq, at: a.to, kind: "sub:done", text: "" });
        step = it.agents.length === 1 ? "Subagent" : `${it.agents.length} subagents`;
        return;
      }
      const ls = it.kind === "tools" ? it.lines : [it.line];
      for (const l of ls) {
        lines.push(l);
        if (l.kind === "code") { actions++; const c = codeLabel(l.text); step = [presentTense(c.label), c.detail].filter(Boolean).join(" "); }
        else if (isNativeCall(l)) {
          // No block around it: the call is the action, and its own record says whether it failed.
          actions++;
          if (callFailed(l) && !canceled(l)) failed++;
          step = callRunning(l) ? callStep(l) : pastTense(callStep(l));
        } else if (l.kind === "call") step = callStep(l); // the runtime's word beats the label read off the source
        else if (l.kind === "result") { step = pastTense(step); if (!canceled(l) && ((typeof l.data?.exit === "number" && l.data.exit !== 0) || thrownError(l))) failed++; }
        else if (l.kind === "job") {
          const id = typeof l.data?.id === "number" ? String(l.data.id) : /^job (\d+) /.exec(l.text)?.[1];
          if (!id || !jobs.has(id)) actions++;
          if (id) jobs.add(id);
          if (jobFailed(l)) failed++;
          step = "Job" + (id ? ` ${id}` : "");
        } else if (l.kind.startsWith("todo/")) actions++;
        else if (l.kind === "thinking") step = "Thinking";
        else if (l.kind === "error") failed++;
      }
    });
    const ats = lines.map((l) => Date.parse(l.at)).filter(Number.isFinite);
    const iso = (n: number) => new Date(n).toISOString();
    const thinkingOnly = lines.length > 0 && lines.every((l) => l.kind === "thinking");
    out.push({
      kind: "work", seq: cur[0].seq, items: cur, seqs: lines.map((l) => l.seq), rows, actions, failed, thinkingOnly,
      from: ats.length ? iso(Math.min(...ats)) : "", to: ats.length ? iso(Math.max(...ats)) : "",
      step: step.length > 80 ? step.slice(0, 79) + "…" : step, last: false, what: nativeWhat(lines) || undefined,
    });
    cur = [];
  };
  // Prose written in the same reply as a program predates that program's results:
  // it is placed after the work, not above rows still running.
  let held: Item | undefined;
  const release = () => { if (held) out.push({ kind: "reply", item: held, early: true }); held = undefined; };
  for (const it of items) {
    if (it.kind === "line" && isReply(it.line, codes)) {
      flush();
      release();
      const text = it.line.text.replace(NOTE_RE, "");
      if (stripRunFences(text, codes) !== text.trim() || splitBareProgram(text)[1]) { held = it; continue; }
      // A lone thought's span is until the reply it led to.
      const prev = out[out.length - 1];
      if (prev?.kind === "work" && prev.thinkingOnly && Date.parse(it.line.at) > Date.parse(prev.to)) prev.to = it.line.at;
      out.push({ kind: "reply", item: it });
      continue;
    }
    // Subagents are the work worth seeing: a run still going, or one the
    // engine started (no program of the parent's to fold under), stands
    // on its own. A loop's spawn stays with the program that made it,
    // which reads its outcome off the card.
    if (it.kind === "sub") {
      const prev = cur.at(-1), prevLine = prev?.kind === "tools" ? prev.lines.at(-1) : prev?.kind === "line" ? prev.line : undefined;
      if ((live && it.agents.some((a) => !a.status)) || prevLine?.kind !== "code") { flush(); out.push({ kind: "pinned", item: it }); continue; }
    }
    if (it.kind === "line" && isAgentNotice(it.line)) { flush(); out.push({ kind: "notice", item: it }); continue; }
    // A steer is something you said: it ends the work it interrupted and
    // stays in view, never folded into a "Worked for" row.
    if (it.kind === "line" && it.line.kind === "input") { flush(); out.push({ kind: "notice", item: it }); continue; }
    // The error that ended the turn is the turn's outcome, not one of its actions: keep its card in view.
    const before = cur.at(-1), last = before?.kind === "tools" ? before.lines.at(-1) : before?.kind === "line" ? before.line : undefined;
    const blockFailed = last?.kind === "result" && ((typeof last.data?.exit === "number" && last.data.exit !== 0) || thrownError(last));
    if (it.kind === "line" && it.line.kind === "error" && !blockFailed && items.slice(items.indexOf(it) + 1).every((x) => x.kind === "line" && (x.line.kind === "done" || isHookLine(x.line)))) { flush(); out.push({ kind: "notice", item: it }); continue; }
    cur.push(it);
  }
  flush();
  release();
  // A reply moved after its work does not end that work: it may still be running.
  for (let i = out.length - 1; i >= 0; i--) {
    const s = out[i];
    if (s.kind === "reply" && !s.early) break;
    if (s.kind === "work") s.last = true;
  }
  return out;
}

/** "ran 2 commands · read 1 file · edited 2 files": an engine segment's calls, counted by what they did. */
export function nativeWhat(lines: Line[]): string {
  const n = { bash: 0, view: 0, edit: 0, job: 0 };
  for (const l of lines) {
    if (!isNativeCall(l) || callRunning(l)) continue;
    const tool = String(l.data?.tool ?? "");
    if (tool === "bash") n.bash++;
    else if (tool === "view") n.view++;
    else if (tool === "patch" || tool === "write") n.edit++;
  }
  const one = (k: number, sing: string, plural: string) => k ? `${k} ${k === 1 ? sing : plural}` : "";
  return [n.bash ? "ran " + one(n.bash, "command", "commands") : "", n.view ? "read " + one(n.view, "file", "files") : "",
    n.edit ? "edited " + one(n.edit, "file", "files") : ""].filter(Boolean).join(" · ");
}

/** "Worked for 12s · 6 actions", "Worked for 12s · ran 2 commands", "Thought for 9s": a finished segment's row. */
export function workHeadline(s: { actions: number; thinkingOnly: boolean; from: string; to: string; what?: string }): string {
  const ms = s.from && s.to ? Date.parse(s.to) - Date.parse(s.from) : 0;
  // Under a second is not a fact worth a slot ("Worked for 0s").
  const took = ms >= 1000 ? ` for ${duration(ms)}` : "";
  if (s.thinkingOnly) return "Thought" + took;
  // MB-TR: never a bare "Worked": no duration leaves the count alone.
  const count = s.what || (s.actions ? `${s.actions} ${s.actions === 1 ? "action" : "actions"}` : "");
  if (!took) return count || "Worked briefly";
  return "Worked" + took + (count ? " · " + count : "");
}
