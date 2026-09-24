import { describe, expect, test } from "bun:test";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { renderToStaticMarkup } from "react-dom/server";
import type { ReactElement } from "react";
const { ChangesChip, HeadMore, JumpLatest, SessionChanges, Thread } = await import("../src/app");
import type { Line, Row } from "../src/types";

// go/tests/model/specs/ui_thread.fizz at the component level: every
// settled state of the checked-in graph is rendered from props built from
// that node, and the node's meaning and the spec's invariants are asserted
// on the markup. The browser walk (tests/web/specs/model/ui_thread.spec.ts)
// drives the same graph through a real serve; this one names a broken
// render in milliseconds and needs no Chromium.
//
// What the server says (the transcript read, the status, the turns, the
// stream, the sends on their way) is Thread's props, so the whole Thread
// is rendered. What the person did is the page's own: the Settings
// popover and the jump button are rendered through the views Thread
// draws them with (HeadMore, JumpLatest); the strip's <details> popovers
// and the call row's and Working fold's open state are the browser's, so
// here the markup owes their triggers, and each row its default.

type Node = {
  viewport: "wide" | "phone"; route: "thread" | "changes" | "context"; transcript: "loading" | "ok" | "failed";
  status: "idle" | "running" | "done" | "error"; sending: boolean; streamed: boolean;
  call: "none" | "running" | "ok" | "failed"; callOpen: boolean; workOpen: boolean; away: boolean; fresh: boolean;
  settings: boolean; chg: boolean; details: boolean; focus: "settings" | "trigger" | "other";
  usage: boolean; edits: boolean; errCard: boolean;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_thread", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Thread#0.")) n[k.slice(9)] = v;
  nodes.set(JSON.stringify(n, Object.keys(n).sort()), n as Node);
}

// --- the spec's own definitions, over a node

const pops = (n: Node) => Number(n.settings) + Number(n.chg) + Number(n.details);
function head(n: Node): string {
  if (n.transcript !== "ok") return "";
  if (n.status === "error") return "error";
  if (n.sending || n.status === "running") return "working";
  return n.status;
}
function note(n: Node): string {
  if (n.transcript === "loading") return "loading";
  if (n.transcript === "failed") return "failed";
  return n.status === "idle" && !n.sending ? "empty" : "";
}
function working(n: Node): string {
  if (n.route !== "thread" || n.transcript !== "ok" || n.status !== "running") return "";
  return n.streamed || n.call !== "none" ? "Working" : "Model is thinking";
}

// --- props from a node

const ID = "s1";
const PROMPT = "Split the parser into modules and report what moved where.";
const STREAMED = "Reading the parser first.";
const LOAD_FAIL = "Unexpected token 'o', \"not json\" is not valid JSON";
const TURN_FAIL = "model says no";
const t0 = Date.parse("2026-09-24T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const USAGE = { in: 1200, out: 80, last_in: 1200, cost: 0.01 };

/** The session's history at a node: the spec keeps only the latest turn's call, so the rest is the shortest history that explains the node. */
function transcript(n: Node): { lines: Line[]; pending: boolean } {
  const lines: Line[] = [];
  const push = (kind: string, text: string, data?: Record<string, unknown>) => lines.push({ seq: lines.length + 1, at: at(lines.length + 1), kind, text, data });
  const callLine = (c: Node["call"]) => {
    const base = { id: "toolu_1", tool: "bash", cmd: "./split.sh" };
    if (c === "running") push("call", "./split.sh", { ...base, phase: "start", tail: "" });
    if (c === "ok") push("call", "./split.sh", { ...base, ms: 1200, exit: 0, output: "moved" });
    if (c === "failed") push("call", "./split.sh", { ...base, ms: 1200, exit: 1, error: "exit status 1", output: "no such module" });
  };
  const end = (failed: boolean, usage: boolean, files: boolean) => {
    if (failed) push("error", TURN_FAIL);
    push("done", "", { ...(usage ? { usage: USAGE } : {}), ...(files ? { files: ["out.txt"] } : {}) });
  };
  if (!n.errCard) {
    // One turn, from the composer: Send needs a thread with no usage and no error card.
    if (n.status === "idle") return { lines, pending: n.sending };
    push("input", PROMPT);
    callLine(n.call);
    if (n.status === "done") end(false, true, n.edits);
    return { lines, pending: false };
  }
  // The first turn failed; the error card's Retry sends its prompt again and makes no call.
  if (n.status === "error" && !n.sending && (n.call !== "none" || !n.usage)) {
    push("input", PROMPT);
    callLine(n.call);
    end(true, n.call !== "none", n.call === "ok");
    return { lines, pending: false };
  }
  const first = n.call !== "none" ? n.call : n.edits ? "ok" : n.usage && n.status !== "done" ? "failed" : "none";
  push("input", PROMPT);
  callLine(first);
  end(true, first !== "none", first === "ok");
  if (n.sending) {
    // The latest turn's call is none while the first turn's is not: a retry already failed.
    if (first !== "none" && n.call === "none") { push("input", PROMPT); end(true, false, false); }
    return { lines, pending: true };
  }
  push("input", PROMPT);
  if (n.status === "done") end(false, true, false);
  if (n.status === "error") end(true, false, false);
  return { lines, pending: false };
}

const noop = () => {};
const handlers = { onSend: async () => null, onAnswer: async () => null, onInterrupt: noop, onArchive: noop, onRename: async () => {}, onModel: noop,
  onEffort: noop, onAssign: noop, onBack: noop, onContext: noop, onRetry: noop, projects: [], busy: false, jump: null };

/** Render under a viewport: the page reads its width through matchMedia, and bun has no window. */
function atWidth(viewport: Node["viewport"], el: ReactElement): string {
  const width = viewport === "wide" ? 1100 : 600;
  const g = globalThis as { window?: unknown };
  g.window = { location: { hash: "" }, matchMedia: (q: string) => ({ matches: /max-width:\s*(\d+)px/.test(q) && width <= +/max-width:\s*(\d+)px/.exec(q)![1] }) };
  try { return renderToStaticMarkup(el); } finally { delete g.window; }
}

const threads = new Map<string, string>();
/** The Thread at a node: what the server said and the viewport are all it is drawn from. */
function threadHtml(n: Node): string {
  const key = JSON.stringify([n.viewport, n.transcript, n.status, n.sending, n.streamed, n.call, n.usage, n.edits, n.errCard]);
  const hit = threads.get(key);
  if (hit !== undefined) return hit;
  const { lines, pending } = transcript(n);
  const inputs = lines.filter((l) => l.kind === "input");
  const sending = pending ? [{ id: "p1", text: PROMPT, after: lines.length, steer: false, seen: inputs.map((l) => `${l.seq}|${l.at}`), at: at(90) }] : [];
  const row = { id: ID, cwd: "/w/thread", title: PROMPT, status: n.status, live: n.status === "running", writable: "/w/thread" } as unknown as Row;
  const html = atWidth(n.viewport, <Thread {...handlers} row={row} lines={lines} sending={sending}
    loading={n.transcript !== "ok"} loadError={n.transcript === "failed" ? LOAD_FAIL : undefined}
    stream={n.streamed ? [{ kind: "assistant", text: STREAMED }] : []} />);
  threads.set(key, html);
  return html;
}

// --- markup helpers (as model-hooks.test.tsx)

const count = (html: string, re: RegExp) => (html.match(new RegExp(re.source, "g")) ?? []).length;
/** The markup of one element, found by an opening-tag pattern, through its matching close. */
function element(html: string, open: RegExp): string {
  const m = open.exec(html);
  if (!m) return "";
  const tag = /^<(\w+)/.exec(m[0])![1];
  let depth = 0;
  const re = new RegExp(`<(/?)${tag}\\b[^>]*?(/?)>`, "g");
  re.lastIndex = m.index;
  for (let t = re.exec(html); t; t = re.exec(html)) {
    if (t[2]) continue;
    depth += t[1] ? -1 : 1;
    if (depth === 0) return html.slice(m.index, re.lastIndex);
  }
  return html.slice(m.index);
}
/** Every element an opening-tag pattern finds, each through its close. */
function elements(html: string, open: RegExp): string[] {
  const out: string[] = [];
  const re = new RegExp(open.source, "g");
  for (let m = re.exec(html); m; m = re.exec(html)) out.push(element(html.slice(m.index), open));
  return out;
}

const CONTEXT_CHIP = /<button type="button" class="rt rt-link"[^>]*aria-label="Context: /;
const CHIP_POPOVER = /<details class="rt rt-jobs">/;
const DETAILS = /<details class="rt rt-jobs rt-more">/;

// --- the checks

function checkHead(n: Node, html: string) {
  const main = element(html, /<div class="head-main">/);
  expect(main).not.toBe("");
  const live = /<span class="status head-live[^"]*">/.test(main);
  const failedTurn = main.includes('class="status head-failed"');
  const mark = /<span class="status" [^>]*>.*?<span[^>]*>(\w+)<\/span><\/span>/.exec(main)?.[1] ?? "";
  // StatusAlwaysVisible: the header names a status exactly while the transcript is read.
  switch (head(n)) {
    case "":
      expect([live, failedTurn, mark]).toEqual([false, false, ""]);
      break;
    case "working":
      expect(live).toBe(true);
      expect(main).toContain(n.sending ? ">Sending</span>" : "</svg>");
      break;
    case "error":
      expect([live, failedTurn, mark]).toEqual([false, false, "Failed"]);
      break;
    case "done":
      // A turn that ended on a failed command may say Failed beside a done session (R4-B).
      expect(live).toBe(false);
      expect(failedTurn || mark === "Done").toBe(true);
      break;
    case "idle":
      expect([live, failedTurn, mark]).toEqual([false, false, "Idle"]);
      break;
  }
  // The Settings trigger names its popover; Thread mounts it shut.
  expect(main).not.toContain('role="dialog"');
  expect(html).toMatch(/<button class="more" aria-label="Session settings" aria-expanded="false" aria-controls="more-s1">/);
}

function checkTranscript(n: Node, html: string) {
  const tr = element(html, /<div class="scroll transcript"[^>]*>/);
  expect(tr).not.toBe("");
  const failedNote = tr.includes("Couldn’t load this session");
  const empty = tr.includes("Nothing here yet");
  switch (note(n)) {
    case "failed":
      expect(failedNote).toBe(true);
      expect(tr).toMatch(/<button[^>]*>Retry<\/button>/);
      break;
    case "loading":
      // A fast read shows nothing at all; "Loading transcript…" is the slow one's, on a timer.
      expect(failedNote).toBe(false);
      expect(empty).toBe(false);
      break;
    case "empty":
      expect(empty).toBe(true);
      break;
    default:
      expect([failedNote, empty]).toEqual([false, false]);
  }
  if (n.transcript !== "ok") {
    expect(tr).not.toContain('<section class="turn');
    return;
  }

  // The send on its way: its "Sending…" row, in a section of its own.
  expect(tr.includes("turn-sending-state")).toBe(n.sending);
  expect(tr.includes("stream-say")).toBe(n.streamed);

  // WorkingOnlyWhileRunning: "Model is thinking" until anything comes back, then "Working".
  const waiting = tr.includes('<p class="waiting-model"');
  const liveFold = elements(tr, /<details class="block thin work-seg work-seg-live"[^>]*>/);
  const workingRow = /<p class="working" role="status">.*?<span>Working<\/span>/.test(tr);
  switch (working(n)) {
    case "Model is thinking":
      expect([waiting, liveFold.length, workingRow]).toEqual([true, 0, false]);
      break;
    case "Working":
      expect(waiting).toBe(false);
      // A call in flight or done sits inside the fold; a streamed reply alone gets the row.
      if (n.call !== "none") {
        expect(liveFold.length).toBe(1);
        expect(liveFold[0]).toMatch(/<span class="block-label">Working<\/span>/);
      } else expect([liveFold.length, workingRow]).toEqual([0, true]);
      break;
    default:
      expect([waiting, liveFold.length, workingRow]).toEqual([false, 0, false]);
  }
  // WorkFoldHasCall, LiveOnlyWhileRunning: the fold, shut until opened, is there to open.
  if (n.workOpen) expect(liveFold.length).toBe(1);
  if (liveFold.length) expect(liveFold[0]).toContain('aria-expanded="false"');

  // The latest turn's call row: a retried turn makes none.
  const turns = elements(tr, /<section class="turn"[^>]*>/);
  const last = turns.at(-1) ?? "";
  const call = /<details class="block thin toolcall call-native[^"]*"[^>]*>/.exec(last)?.[0] ?? "";
  const state = !call ? "none" : call.includes("block-failed") ? "failed" : element(last, /<details class="block thin toolcall call-native/).includes("tool-running") ? "running" : "ok";
  expect(state).toBe(n.call);
  // CallOpenHasCall; and the row mounts open only for the failure its turn ended on.
  if (n.callOpen) expect(call).not.toBe("");
  if (call) expect(call.includes(' open=""')).toBe(n.call === "failed" && n.status !== "running");

  // ErrorHasRetry: the card and its Retry exactly when a turn ended on an error.
  const cards = elements(tr, /<div class="err err-card">/);
  expect(cards.length > 0).toBe(n.errCard);
  for (const c of cards) expect(c).toMatch(/<button type="button" class="btn">Retry<\/button>/);
  if (n.status === "error") expect(n.errCard).toBe(true);

  // A finished turn that changed a file: its "N files" link.
  expect(tr.includes("turn-files")).toBe(n.edits);
}

function checkStrip(n: Node, html: string) {
  const strip = element(html, /<div class="runtime-strip">/);
  const more = element(strip, DETAILS);
  const chip = element(strip, CHIP_POPOVER);
  const failed = n.transcript === "failed";
  // The Context chip: once a turn reported usage, in the strip (wide) or under Details (phone).
  expect(CONTEXT_CHIP.test(strip)).toBe(n.usage && n.transcript === "ok");
  if (n.viewport === "wide") {
    // PopoverFitsViewport: the Changes chip's popover is the wide pane's; Details is the phone's.
    expect(more).toBe("");
    expect(chip !== "").toBe(!failed);
    if (chip) expect(chip).toMatch(/<a class="link chg-full" href="#\/s\/s1\/changes">Open full view<\/a>/);
  } else {
    expect(chip).toBe("");
    expect(strip).not.toContain("chg-full");
    // A phone folds every metric behind Details: the Changes chip always, Context once there is usage.
    expect(more !== "").toBe(!failed);
    if (more) expect(more).toContain('<summary aria-label="More session details">');
    if (n.usage && !failed) expect(more).toMatch(CONTEXT_CHIP);
  }
  if (n.chg) expect([n.viewport, chip !== ""]).toEqual(["wide", true]);
  if (n.details) expect([n.viewport, more !== ""]).toEqual(["phone", true]);
  // The composer: Send says Steer over a running turn, and a failed read disables it and says why.
  const send = /<button class="btn btn-primary"[^>]*>/.exec(element(html, /<div class="composer-actions">/))![0];
  expect(send).toContain(`aria-label="${n.transcript === "ok" && n.status === "running" ? "Steer" : "Send"}"`);
  expect(send.includes('title="Transcript didn’t load"')).toBe(failed);
  expect(send).toContain('disabled=""');
}

const changesCache = new Map<string, string>();
/** The Changes chip as the phone draws it once the session's edits are read: a link only when there is something to see. */
function phoneChip(edits: boolean): string {
  const key = String(edits);
  if (!changesCache.has(key)) {
    const read = { files: edits ? [{ path: "out.txt", add: 1, del: 0 }] : [], repo: "thread", failed: false };
    changesCache.set(key, atWidth("phone",
      <SessionChanges.Provider value={{ session: read, tree: read, turn: undefined, turnSeq: undefined, retry: noop } as never}>
        <ChangesChip row={{ id: ID, cwd: "/w/thread" } as Row} scope="session" onScope={noop} />
      </SessionChanges.Provider>));
  }
  return changesCache.get(key)!;
}

function checkLinks(n: Node, html: string) {
  // ChangesOnlyFromALink: the page is reached only through a link that was on screen.
  const phoneLink = /<a class="rt rt-link" href="#\/s\/s1\/changes"/.test(phoneChip(n.edits));
  if (n.viewport === "phone") expect(phoneLink).toBe(n.edits);
  const fullView = n.viewport === "wide" && CHIP_POPOVER.test(html);
  const files = element(html, /<div class="scroll transcript"[^>]*>/).includes("turn-files");
  if (n.route === "changes") expect(fullView || phoneLink || files).toBe(true);
  // ContextOnlyWithUsage: the Context page only from a chip that exists.
  if (n.route === "context") expect(CONTEXT_CHIP.test(html)).toBe(true);
}

function checkSettings(n: Node) {
  const html = renderToStaticMarkup(<HeadMore id={ID} open={n.settings} onToggle={noop} onTabOut={noop}><p>controls</p></HeadMore>);
  const btn = /<button class="more"[^>]*>/.exec(html)![0];
  expect(btn).toContain(`aria-expanded="${n.settings}"`);
  const dialogs = [...html.matchAll(/<div class="head-pop" role="dialog"[^>]*>/g)].map((m) => m[0]);
  expect(dialogs.length).toBe(n.settings ? 1 : 0);
  if (dialogs.length) {
    // SettingsHoldsFocus: the dialog is what focus moves into, and the trigger names it.
    expect(dialogs[0]).toContain('aria-label="Session settings"');
    expect(dialogs[0]).toContain('tabindex="-1"');
    expect(dialogs[0]).toContain(`id="more-${ID}"`);
    expect(btn).toContain(`aria-controls="more-${ID}"`);
  }
  expect(n.settings).toBe(n.focus === "settings");
}

function checkJump(n: Node) {
  const html = renderToStaticMarkup(<JumpLatest away={n.away} fresh={n.fresh} onClick={noop} />);
  // FreshOnlyAway: the button shows only scrolled away, and says "New activity" only when something landed since.
  expect(count(html, /<button class="btn jump-latest"/)).toBe(n.away ? 1 : 0);
  if (!n.away) return;
  expect(html).toContain(`aria-label="${n.fresh ? "New activity, jump to latest" : "Jump to latest"}"`);
  expect(html).toContain(`<span class="jump-word">${n.fresh ? "New activity" : "Latest"}</span>`);
}

describe("ui_thread.fizz, every node rendered", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBeGreaterThan(100);
  });

  for (const [key, n] of nodes) {
    test(key, () => {
      // OnePopover, NoPopoverOffThread.
      expect(pops(n)).toBeLessThanOrEqual(1);
      if (n.route !== "thread") expect(pops(n)).toBe(0);
      const html = threadHtml(n);
      checkLinks(n, html);
      checkJump(n);
      if (n.route !== "thread") return;
      checkHead(n, html);
      checkTranscript(n, html);
      checkStrip(n, html);
      checkSettings(n);
    });
  }
});
