import { afterAll, beforeAll, describe, expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { PendingThread, Sidebar, SessionSettings, Thread, ViewNav, phoneNav } from "../src/app";
import { DialogView } from "../src/dialog";
import { HooksPageView } from "../src/hooks";
import type { Line, Row } from "../src/types";
import type { Worker } from "../src/work";
import { WorkContext, WorkDialog } from "../src/work-ui";
// No DOM under bun: the palette's portal renders in place (as in model-palette.test.tsx).
const real = await import("react-dom");
mock.module("react-dom", () => ({ ...real, createPortal: (node: unknown) => node }));
const { PaletteView } = await import("../src/palette");

// go/tests/model/specs/ui_narrow.fizz at the component level: every
// settled state of the checked-in graph is drawn the way the page draws
// it on a 390px phone — the route's pane (the list, the thread or Hooks),
// the bottom bar when App mounts it, and each layer the node has up —
// from props built from the node, and the node's claims are checked on
// the markup. The browser walk (tests/web/specs/model/ui_narrow.spec.ts)
// drives the same graph through a real serve in minutes; this names a
// broken render in milliseconds.
//
// What static markup cannot show is the walk's alone: where focus is,
// the on-screen keyboard and what a layer measured itself against (fit),
// whether a <details> is open, a Select's open list, and the 200 ms
// before "Loading transcript…" earns its line. Here each open layer is
// checked for what it owes those: a named dialog with its focus target.

type Node = {
  route: "list" | "thread" | "hooks"; back: boolean; nav: boolean; transcript: "none" | "loading" | "loaded" | "error";
  status: "idle" | "running"; archived: boolean; pending: boolean; draft: boolean;
  settings: boolean; picker: boolean; details: boolean; work: boolean; palette: boolean; dialog: boolean;
  focus: string; keyboard: boolean; fit: string; shown: string; send: "off" | "send" | "steer"; stop: boolean;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_narrow", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Phone#0.")) n[k.slice(8)] = v;
  nodes.set(Object.keys(n).sort().map((k) => `${k}=${n[k]}`).join(" "), n as Node);
}

// ---- a phone: the 720px (and 480px) queries match ----

const g = globalThis as unknown as { window?: unknown; localStorage?: unknown; document?: unknown };
const saved = { window: g.window, localStorage: g.localStorage, document: g.document };
let drafts: Record<string, string> = {};
beforeAll(() => {
  g.window = { matchMedia: (q: string) => ({ matches: /max-width:\s*(720|480)px/.test(q), addEventListener() {}, removeEventListener() {} }) };
  g.localStorage = { getItem: (k: string) => drafts[k] ?? null, setItem() {}, removeItem() {} };
  g.document ??= { body: {} };
});
afterAll(() => {
  if (saved.window === undefined) delete g.window; else g.window = saved.window;
  if (saved.localStorage === undefined) delete g.localStorage; else g.localStorage = saved.localStorage;
  if (saved.document === undefined) delete g.document; else g.document = saved.document;
});

type Pending = Parameters<typeof PendingThread>[0]["sending"][number];

// ---- the fixture behind the abstract values (the spec's abstractions) ----

const S = "s-narrow00001";
const TITLE = "narrow walk";
const DRAFT = "next step";
const LOAD_FAIL = "ui_narrow: transcript read failed on purpose";
const at = new Date(Date.now() - 60_000).toISOString();

/** The one session as serve lists it: idle reads Done; recorded work (a background agent). */
const rowOf = (n: Node): Row => ({
  id: S, title: TITLE, cwd: "/home/u/work", status: n.status === "running" ? "running" : "done", live: true,
  archived: n.archived, entries: 4, modified: at, lastAt: at, model: "m1", agents: { running: 0, queued: 0, total: 1 },
} as unknown as Row);

/** One finished turn with recorded usage, so a phone folds the strip's chips behind Details. */
const lines: Line[] = [
  { seq: 1, at, kind: "input", text: "first task" },
  { seq: 2, at, kind: "done", text: "first turn done", data: { usage: { in: 1200, out: 300, cost: 0.01, last_in: 1200 } } },
] as Line[];

/** A send serve has not answered: a steer while a turn runs, else the send that starts one. */
const sendingOf = (n: Node): Pending[] =>
  n.pending ? [{ id: "p1", text: DRAFT, after: 2, steer: n.status === "running", seen: ["1|" + at], at }] as Pending[] : [];

/** The router's pane and view for a route (app.tsx read()): the hash picks both. */
const paneOf = (n: Node) => ({ pane: n.route === "list" ? "list" : "thread", view: n.route === "hooks" ? "hooks" : "sessions" } as const);

const agent: Worker = {
  key: `${S}:agent:k1`, kind: "agent", session: S, id: "k1", label: "side job", task: "side job", life: "finished",
  stepErrors: 0, notRun: 0, outputState: "none", seq: 1, live: false, canStop: false,
} as Worker;
const noop = () => {};
const review = { isReviewed: () => true, isNew: () => false, markReviewed: noop };

// ---- the page, as App mounts it for the node ----

function route(n: Node): string {
  const row = rowOf(n);
  if (n.route === "list") {
    return renderToStaticMarkup(<Sidebar rows={[row]} selected={null} onSelect={noop} query="" onQuery={noop}
      showArchived onToggleArchived={noop} active onNew={noop} />);
  }
  if (n.route === "hooks") return renderToStaticMarkup(<HooksPageView data={null} err="" at={0} onBack={noop} onRetry={noop} />);
  drafts = n.draft ? { ["bough:draft:" + S]: DRAFT } : {};
  return renderToStaticMarkup(
    <Thread row={row} lines={n.transcript === "loaded" ? lines : []} loading={n.transcript !== "loaded"}
      loadError={n.transcript === "error" ? LOAD_FAIL : undefined} onRetry={noop} onBack={noop}
      projects={[]} busy={n.pending} jump={null} sending={sendingOf(n)}
      onSend={async () => null} onAnswer={async () => null} onInterrupt={noop} onArchive={noop} onRename={async () => {}}
      onModel={noop} onEffort={noop} onAssign={noop} />,
  );
}

function nav(n: Node): string {
  const { pane, view } = paneOf(n);
  if (!phoneNav(true, pane, view)) return "";
  return renderToStaticMarkup(<ViewNav phone view={pane === "list" ? "sessions" : view} onView={noop} />);
}

const catalogue = { cat: null, failed: false, retry: noop };

/** Each layer the node has up, drawn by the component that draws it. */
function layers(n: Node): Record<string, string> {
  const row = rowOf(n);
  const out: Record<string, string> = {};
  if (n.settings) {
    out.settings = renderToStaticMarkup(<SessionSettings row={row} projects={[]} catalogue={catalogue} failedLoad={false}
      onModel={noop} onEffort={noop} onAssign={noop} onRename={async () => {}} onArchive={noop} onClose={noop} />);
  }
  if (n.work) {
    out.work = renderToStaticMarkup(
      <WorkContext.Provider value={{ session: S, live: true, workers: [agent], byKey: new Map(), jobs: new Map(), jobFirst: new Map(), review, stops: {}, requestStop: noop } as never}>
        <WorkDialog workers={[agent]} sheet childState="ok" onRetryChildren={noop} parent={S} onClose={noop} onView={noop} onOpenAgent={noop} />
      </WorkContext.Provider>,
    );
  }
  if (n.palette) {
    out.palette = renderToStaticMarkup(<PaletteView open onClose={noop} rows={[row]} commands={[]} mode="new" visited={[]}
      onOpenSession={noop} onOpenWikiPage={noop} onStart={noop} onStartIn={noop} q="" setQ={noop} atId={null} setAtId={noop}
      search={{ hits: [], state: "idle", retry: noop }} pages={[]} places={{ folder: null, dirs: [], home: "" }}
      field={{ current: null }} opener={{ current: null }} />);
  }
  if (n.dialog) {
    out.dialog = renderToStaticMarkup(<DialogView text="" saving={false} failed="" onText={noop} onSubmit={noop} onDismiss={noop}
      req={{ kind: "confirm", title: "Archive this session?", body: `“${TITLE}” leaves the list. It stays under Archived and can be restored.`,
             action: "Archive", danger: false, safe: true, resolve: noop }} />);
  }
  return out;
}

// ---- markup helpers ----

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
const words = (html: string) => html.replace(/<span class="visually-hidden"[^>]*>[^<]*<\/span>/g, "").replace(/<[^>]*>/g, "").trim();
// The browser walk's HEAD map: the page's header word to the spec's.
const HEAD: Record<string, string> = { Sending: "sending", Waiting: "working", Working: "working", Done: "done", Stopped: "done" };

// ---- the checks ----

function checkWayOut(n: Node, main: string, bar: string) {
  // WayOut: the list is the root (bar, no Back); the thread has Back and no bar; Hooks both.
  expect(n.back).toBe(n.route !== "list");
  expect(n.nav).toBe(n.route !== "thread");
  expect(count(main, /<button class="back"/)).toBe(n.back ? 1 : 0);
  expect(bar !== "").toBe(n.nav);
  if (bar) {
    expect(bar).toContain('class="side-nav phone-nav"');
    const here = /<a[^>]*aria-current="page"[^>]*>/g;
    expect(count(bar, here)).toBe(1);
    expect(/<a[^>]*aria-current="page"[^>]*>/.exec(bar)![0]).toContain(n.route === "list" ? 'href="#/"' : 'href="#/hooks"');
  }
}

function checkList(n: Node, html: string) {
  // StatusVisible, StatusTrue: the session's row names the server's status.
  const rowTag = new RegExp(`<button[^>]*class="row[^"]*"[^>]*data-id="${S}"[^>]*>|<button[^>]*data-id="${S}"[^>]*class="row[^"]*"[^>]*>`).exec(html);
  expect(rowTag).not.toBeNull();
  const label = /aria-label="([^"]*)"/.exec(rowTag![0])![1];
  expect(label.split(", ")[1]).toBe(n.status === "running" ? "Running" : "Done");
  expect(n.shown).toBe(n.status === "running" ? "running" : "done");
  // Archived, it moves under Archived and stays on the list (open here, as the walk opens it).
  const archived = element(html, /<[a-z]+[^>]*id="sec-archived"[^>]*>/);
  expect(archived.includes(`data-id="${S}"`)).toBe(n.archived);
  // The list's New session, which opens the palette.
  expect(html).toMatch(/<button[^>]*class="[^"]*side-new/);
  // No composer, no Send, no Stop on the list.
  expect(html).not.toContain('id="composer"');
  expect([n.send, n.stop]).toEqual(["off", false]);
}

function checkHooks(n: Node, html: string) {
  expect(html).toContain("<h1>Hooks</h1>");
  expect(html).not.toContain('id="composer"');
  expect([n.shown, n.send, n.stop]).toEqual(["", "off", false]);
}

function checkThread(n: Node, html: string) {
  const head = element(html, /<header class="thread-head">/);
  expect(head).toContain(`>${TITLE}</h1>`);
  const status = element(head, /<span class="status[^"]*"[^>]*>/);
  const note = element(html, /<div class="error-note[^"]*"[^>]*>/);
  switch (n.transcript) {
    case "loading":
      // The header names no status until the transcript is read; the
      // "Loading transcript…" line is the 200 ms timer's (the walk's).
      expect(n.shown).toBe("loading");
      expect(status).toBe("");
      expect(note).toBe("");
      break;
    case "error":
      expect(n.shown).toBe("error");
      expect(status).toBe("");
      expect(note).toContain("Couldn’t load this session");
      expect(note).toContain(LOAD_FAIL);
      expect(note).toMatch(/<button[^>]*>Retry<\/button>/);
      break;
    case "loaded":
      // StatusTrue: the header's word is the server's, a send on its way says so.
      expect(status).not.toBe("");
      expect(HEAD[words(status)]).toBe(n.shown);
      expect(n.shown).toBe(n.pending && n.status !== "running" ? "sending" : n.status === "running" ? "working" : "done");
      expect(note).toBe("");
      break;
    default:
      throw new Error(`thread with transcript ${n.transcript}`);
  }

  // NoDeadControl: Send is enabled exactly when the node says, with its word.
  const area = /<textarea[^>]*id="composer"[^>]*>/.exec(html)![0];
  expect(area.includes("disabled")).toBe(n.archived);
  const primary = /<button class="btn btn-primary"[^>]*>/.exec(element(html, /<div class="composer-actions">/))![0];
  const enabled = !/ disabled=""/.test(primary);
  expect(enabled ? /aria-label="([^"]*)"/.exec(primary)![1].toLowerCase() : "off").toBe(n.send);
  expect(count(html, /<button[^>]*class="btn btn-ghost composer-stop"/) + count(html, /class="btn composer-stop-retry"/)).toBe(n.stop ? 1 : 0);

  // The layers' ways in: Settings always in the header; on a loaded
  // transcript the strip folds its chips behind Details and shows Work.
  expect(head).toMatch(/<button class="more" aria-label="Session settings" aria-expanded="false"/);
  if (n.transcript === "loaded") {
    const more = element(html, /<details class="rt rt-jobs rt-more">/);
    expect(more).toContain('aria-label="More session details"');
    const pop = element(more, /<div class="rt-pop">/);
    expect(pop).toContain(">Context<");
    expect(pop).toContain(">Cost<");
    expect(html).toMatch(/<button type="button" class="work-summary" aria-haspopup="dialog" aria-expanded="false"/);
  }
}

function checkLayers(n: Node, up: Record<string, string>) {
  // OneLayer: at most one layer, the picker only inside Settings.
  const open = ["settings", "details", "work", "palette", "dialog"].filter((k) => n[k as keyof Node]);
  expect(open.length).toBeLessThanOrEqual(1);
  if (n.picker) expect(n.settings).toBe(true);
  // LayersBelongToRoute: the thread's layers only over the thread, the palette only over the list.
  if (n.settings || n.picker || n.details || n.work || n.dialog) expect(n.route).toBe("thread");
  if (n.palette) expect(n.route).toBe("list");
  // Layers open only on a loaded, quiet thread (the spec's abstraction).
  if (n.settings || n.details || n.work) {
    expect([n.transcript, n.status, n.pending, n.draft]).toEqual(["loaded", "idle", false, false]);
  }
  // Every layer drawn is one named dialog, and holds its focus target.
  const all = Object.values(up).join("");
  expect(count(all, /role="dialog"/)).toBe(Object.keys(up).length);
  expect(count(all, /aria-modal="true"/)).toBe(["work", "palette", "dialog"].filter((k) => up[k]).length);
  if (up.settings) {
    // Non-modal, labelled, focusable itself (the pointer opens it onto the popover).
    expect(up.settings).toMatch(/^<div class="head-pop" role="dialog" aria-label="Session settings" id="more-[^"]+" tabindex="-1"/);
    // The picker lives here: the next-turn model Select's button.
    expect(up.settings).toMatch(/<button[^>]*class="sel-btn" role="combobox"[^>]*aria-label="Next turn model/);
    expect(up.settings).toContain(n.archived ? ">Unarchive</button>" : ">Archive…</button>");
    expect(up.settings).not.toContain(n.archived ? ">Archive…</button>" : ">Unarchive</button>");
  }
  if (up.work) {
    // A bottom sheet on a phone, focus on its heading.
    expect(up.work).toMatch(/class="[^"]*work-sheet/);
    const labelled = /aria-labelledby="([^"]+)"/.exec(element(up.work, /<div[^>]*role="dialog"[^>]*>/))![1];
    expect(up.work).toMatch(new RegExp(`<h2[^>]*id="${labelled}"[^>]*tabindex="-1"|<h2[^>]*tabindex="-1"[^>]*id="${labelled}"`));
    expect(up.work).toContain("work-close");
  }
  if (up.palette) {
    expect(up.palette).toContain('aria-label="Quick access"');
    expect(up.palette).toMatch(/<input[^>]*class="pal-field"/);
  }
  if (up.dialog) {
    // A safe confirm: Cancel and Archive, neither disabled (focus starts on Cancel).
    expect(up.dialog).toContain(">Archive this session?</h2>");
    expect(up.dialog).toMatch(/<button[^>]*>Cancel<\/button>/);
    expect(up.dialog).toMatch(/<button[^>]*>Archive<\/button>/);
    expect(up.dialog).not.toContain("disabled");
  }
}

function checkFocus(n: Node) {
  // FocusOnTopLayer, KeyboardOnlyForTyping, LayersFitViewport as data:
  // the walk measures them; here they are held to the node's own layers.
  const top = n.picker ? "picker" : ["settings", "details", "work", "palette", "dialog"].find((k) => n[k as keyof Node]);
  expect(n.focus).toBe(top ?? (n.focus === "composer" ? "composer" : "page"));
  expect(n.keyboard).toBe(["composer", "palette", "picker"].includes(n.focus));
  expect(n.fit).toBe(!top ? "" : n.keyboard ? "short" : "full");
  // An archived composer takes no typing; only the thread has one.
  if (n.focus === "composer") expect([n.route, n.archived]).toEqual(["thread", false]);
}

describe("ui_narrow.fizz, every node rendered at phone width", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBe(87);
  });

  for (const [key, n] of nodes) {
    test(key, () => {
      const main = route(n);
      checkWayOut(n, main, nav(n));
      if (n.route === "list") checkList(n, main);
      else if (n.route === "hooks") checkHooks(n, main);
      else checkThread(n, main);
      checkLayers(n, layers(n));
      checkFocus(n);
    });
  }
});
