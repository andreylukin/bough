import { describe, expect, mock, test } from "bun:test";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { renderToStaticMarkup } from "react-dom/server";
import type {
  WikiActivityData, WikiFlag, WikiIndexData, WikiPageData, WikiPageRef, WikiPageState, WikiReviewData, WikiSourceData,
} from "../src/wiki";
// No DOM under bun: the palette's portal renders in place.
const real = await import("react-dom");
mock.module("react-dom", () => ({ ...real, createPortal: (node: unknown) => node }));
(globalThis as { document?: unknown }).document ??= { body: {} };
const { Loading, WikiActivityView, WikiIndexView, WikiPageUi, WikiReviewUi } = await import("../src/wiki");
const { PaletteView } = await import("../src/palette");

// go/tests/model/specs/ui_wiki.fizz at the component level: every settled
// state of the checked-in graph is rendered from props built from that
// node, and the node's meaning is asserted on the markup. The browser walk
// (tests/web/specs/model/ui_wiki.spec.ts) drives the same graph through a
// real serve; this one names a broken render in milliseconds.

type Node = {
  viewport: "wide" | "narrow"; route: "index" | "review" | "activity" | "page";
  data: "loading" | "error" | "missing" | "loaded"; cite: boolean; source: "none" | "loading" | "error" | "loaded";
  panel: "none" | "edit" | "history"; saving: boolean; alert: boolean; menu: boolean;
  item: "none" | "open" | "busy" | "failed"; note: "" | "started" | "failed"; palette: boolean; flag: boolean;
  escape_under_dialog: boolean;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_wiki", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Wiki#0.")) n[k.slice(7)] = v;
  nodes.set(JSON.stringify(n, Object.keys(n).sort()), n as Node);
}

// ---- the seeded wiki of the browser spec, as the API answers it ----

const PORTS = "topics/bough/ports.md";
const GATE = "topics/bough/gate.md";
const counts = { cited: 1, inferred: 0, uncited: 1, unsupported: 0, superseded: 1 };
const portsRef: WikiPageRef = { path: PORTS, topic: "bough", title: "Ports", summary: "who binds first", updated: "2026-09-10", counts };
const gateRef: WikiPageRef = { path: GATE, topic: "bough", title: "Gate", summary: "the test gate", updated: "2026-09-10",
  counts: { cited: 1, inferred: 0, uncited: 0, unsupported: 0, superseded: 0 } };
const S1_4 = { session: "s1", seq: 4, label: "result", excerpt: "listening on 7683" };
const S1_5 = { session: "s1", seq: 5, label: "assistant", excerpt: "The web port is 7683." };
const BODY = "# Ports\n\nWho binds first wins.\n\n## Facts\n\n- The web port is 7683 `s1#4`.\n- The artifact port is 7683.\n";
const page: WikiPageData = {
  ...portsRef,
  blocks: [
    { kind: "lede", text: "Who binds first wins.", line: 3, end: 3, raw: "", bullet: false, cites: [] },
    { kind: "heading", text: "Facts", line: 5, end: 5, raw: "", bullet: false, cites: [] },
    { kind: "claim", text: "The web port is 7683.", line: 7, end: 7, raw: "", bullet: true, state: "cited", cites: [S1_4] },
    { kind: "claim", text: "The artifact port is 7683.", line: 8, end: 8, raw: "", bullet: true, state: "uncited", cites: [] },
    { kind: "claim", text: "the old port was 7682", line: 9, end: 9, raw: "", bullet: true, state: "superseded", cites: [S1_4], supersededBy: S1_5 },
    { kind: "links", text: "[Gate](gate.md)", line: 12, end: 12, raw: "", bullet: true, cites: [] },
  ],
  sessions: ["s1"], linkedFrom: [gateRef], body: BODY,
};
const source: WikiSourceData = {
  session: { id: "s1", title: "which port", repo: "/repo", branch: "", cwd: "/repo" },
  seq: 4, at: "2026-09-10T12:00:04Z", total: 5,
  lines: [{ seq: 3, label: "thinking", text: "hmm" }, { seq: 4, label: "result", text: "listening on 7683" }],
  citedBy: [portsRef],
};
const uncited: WikiFlag = { kind: "uncited", page: PORTS, title: "Ports", line: 8, end: 8, raw: "- The artifact port is 7683.",
  claim: "The artifact port is 7683.", why: "no citation", evidence: "" };
const superseded: WikiFlag = { kind: "superseded", page: PORTS, title: "Ports", line: 9, end: 9, raw: "",
  claim: "the old port was 7682", why: "a later session replaced it", evidence: "", cite: S1_5 };
const indexData = (flag: boolean): WikiIndexData => ({
  dir: "/home/u/.bough/wiki", exists: true, topics: [{ name: "bough", pages: [portsRef, gateRef] }],
  health: { installed: true, every: "5m0s", pending: 1, ingesting: false, lastIngest: null, unsupported: 0, superseded: 1, uncited: flag ? 1 : 0 },
  thin: 0, orphans: 0,
});
const reviewData = (flag: boolean): WikiReviewData => ({
  flags: flag ? [uncited, superseded] : [superseded],
  pending: [{ id: "s2", title: "not ingested yet", entries: 2, last: "2026-09-10T12:00:02Z" }],
});
const activityData: WikiActivityData = {
  runs: [{ session: "r1", command: "/llm-wiki ingest s1", at: "2026-09-10T12:00:00Z", done: "2026-09-10T12:00:01Z", ms: 1000, cost: 0,
    running: false, outcomes: [{ id: "s1", title: "which port", disposition: "New", pages: [GATE] }], files: [GATE, "log.md"], commit: "" }],
  every: "5m0s", pending: 1, today: { runs: 1, ingested: 1, noMaterial: 0, spent: 0 }, spent: 0,
};
const READ_FAIL = "read failed";
const SAVE_FAIL = "save failed";
const ACT_FAIL = "claim failed";
const INGEST_FAIL = "ingest failed";
const STARTED = "Started — the run appears here in a moment.";

// ---- what WikiPage and WikiPageView hold at a node ----

/** WikiPage's `note` as setNote leaves it. */
const noteText = (n: Node) => (n.note === "started" ? STARTED : n.note === "failed" ? INGEST_FAIL : "");

/** WikiPageView's own state at a node. History has answered: its read is not a step of the spec. */
function pageState(n: Node): WikiPageState {
  return {
    editing: n.panel === "edit" ? BODY : null,
    saving: n.saving,
    err: n.alert ? SAVE_FAIL : "",
    history: n.panel === "history" ? [{ hash: "abc1234", at: "2026-09-10T12:00:00Z", subject: "wiki: ports" }] : null,
    showHistory: n.panel === "history",
    histErr: "",
    menu: n.menu,
  };
}

/** One review row's busy slot, keyed as WikiReviewView keys it. */
function busy(n: Node): Record<string, string> {
  const k = `${uncited.page}:${uncited.line}:${uncited.kind}`;
  return n.item === "busy" ? { [k]: "…" } : n.item === "failed" ? { [k]: ACT_FAIL } : {};
}

const noop = () => {};
const pnoop = () => Promise.resolve();

/** The screen WikiPage renders for a node: its view once the read answered, Loading until then. */
function screen(n: Node): string {
  const err = n.data === "error" ? READ_FAIL : "";
  switch (n.route) {
    case "index":
      return renderToStaticMarkup(n.data === "loaded"
        ? <WikiIndexView data={indexData(n.flag)} onBack={noop} onOpen={noop} onReview={noop} onActivity={noop} onIngest={noop} ingestErr="" check={() => Promise.resolve([])} />
        : <Loading what="The wiki" title="Wiki" err={err} onBack={noop} onRetry={noop} />);
    case "review":
      return renderToStaticMarkup(n.data === "loaded"
        ? <WikiReviewUi data={reviewData(n.flag)} onBack={noop} onIndex={noop} onSearch={noop} onOpenPage={noop} onAct={pnoop} onIngest={pnoop}
                        filter="all" busy={busy(n)} onFilter={noop} onDecide={noop} />
        : <Loading what="Review" title="Review" err={err} onBack={noop} onIndex={noop} onRetry={noop} />);
    case "activity":
      return renderToStaticMarkup(n.data === "loaded"
        ? <WikiActivityView data={activityData} onBack={noop} onIndex={noop} onOpenPage={noop} onOpenSession={noop} note={noteText(n)} onIngest={noop} />
        : <Loading what="Activity" title="Activity" err={err} onBack={noop} onIndex={noop} onRetry={noop} />);
    case "page":
      return renderToStaticMarkup(
        <WikiPageUi page={n.data === "loaded" ? page : null} onSearch={noop} path={PORTS}
                    pageError={n.data === "error" ? READ_FAIL : n.data === "missing" ? "wiki: page not found" : ""}
                    onRetry={noop} onRetrySource={noop}
                    cite={n.cite ? { session: "s1", seq: 4 } : null}
                    source={n.source === "loaded" ? source : null} sourceError={n.source === "error" ? READ_FAIL : ""}
                    onBack={noop} onIndex={noop} onOpenSession={noop} onCite={noop} onCloseSource={noop} onOpenPage={noop}
                    onSave={pnoop} loadHistory={() => Promise.resolve([])}
                    state={pageState(n)} acts={{
                      edit: noop, type: noop, save: noop, cancel: noop, toggleHistory: noop, closeHistory: noop,
                      retryHistory: noop, toggleMenu: noop, menuEdit: noop, menuHistory: noop,
                    }} />);
  }
}

/** The palette app.tsx renders over the wiki: open exactly while the node says so. */
function palette(n: Node): string {
  return renderToStaticMarkup(
    <PaletteView open={n.palette} onClose={noop} rows={[]} commands={[]} mode="all" visited={[]}
      onOpenSession={noop} onOpenWikiPage={noop} onStart={noop} onStartIn={noop}
      q="" setQ={noop} atId={null} setAtId={noop} search={{ hits: [], state: "idle", retry: noop }} pages={[]}
      places={{ folder: null, dirs: [], home: "/home/u" }} field={{ current: null }} opener={{ current: null }} />,
  );
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
/** A button's opening tag by its text, or "". */
const button = (html: string, text: string) => new RegExp(`<button[^>]*>${text}</button>`).exec(html)?.[0] ?? "";
const header = (html: string) => element(html, /<header class="thread-head page-head">/);
const crumb = (html: string) => button(header(html), "Wiki");

// ---- the checks, one per screen ----

/** Loading and failing look the same on every screen: the head stays, the body waits or says why. */
function checkWaiting(n: Node, html: string, what: string) {
  const body = n.route === "page" ? element(html, /<div class="scroll wk-doc">/) : element(html, /<div class="scroll proj-body">/);
  if (n.data === "loading") {
    expect(body).toMatch(/<div class="skeleton" role="status" aria-live="polite" aria-busy="true">/);
    expect(body).toContain(`Loading ${what}…`);
    expect(body).not.toContain("error-note");
  } else if (n.data === "error") {
    expect(body).toContain(`Couldn’t load ${what}`);
    expect(body).toContain(READ_FAIL);
    expect(button(body, "Retry")).not.toBe("");
    expect(body).not.toContain('aria-busy="true"');
  } else {
    throw new Error(`not a waiting state: ${n.data}`);
  }
}

function checkIndex(n: Node, html: string) {
  expect(header(html)).toContain("<h1>Wiki</h1>");
  // The index is the crumb's target: it has none of its own.
  expect(crumb(html)).toBe("");
  if (n.data !== "loaded") return checkWaiting(n, html, "the wiki");
  expect(html).toContain('class="wk-health"');
  expect(count(html, /class="wk-row"/)).toBe(2);
  expect(button(header(html), "Activity")).not.toBe("");
  // The flag the server holds is the count the index shows (the superseded claim is the other).
  expect(html).toContain(n.flag ? ">Review 2 flagged claims</button>" : ">Review 1 flagged claim</button>");
}

function checkReview(n: Node, html: string) {
  expect(header(html)).toContain("<h1>Review</h1>");
  expect(crumb(html)).not.toBe("");
  if (n.data !== "loaded") return checkWaiting(n, html, "review");
  expect(html).toContain("<h2>Not compiled</h2>");
  const items = [...html.matchAll(/<div class="wk-item">/g)].map((m) => element(html.slice(m.index), /<div class="wk-item">/));
  const row = items.find((i) => i.includes(">Uncited</span>")) ?? "";
  const newer = items.find((i) => i.includes(">Superseded</span>")) ?? "";
  // The superseded claim stays flagged throughout: its way to the newer entry is always there.
  expect(button(newer, "Open the newer entry")).not.toBe("");
  // ItemOnlyForAFlag, and its converse: a loaded Review lists the claim exactly while the server flags it.
  expect(row !== "").toBe(n.flag);
  expect(n.item === "none").toBe(!n.flag);
  if (!row) return;
  const mark = button(row, "Mark as inference"), drop = button(row, "Drop the claim");
  expect(mark).not.toBe("");
  expect(drop).not.toBe("");
  expect(button(row, "Search history")).not.toBe("");
  // Both decisions are disabled exactly while one is in flight; after a failure the person tries again.
  expect(mark.includes("disabled")).toBe(n.item === "busy");
  expect(drop.includes("disabled")).toBe(n.item === "busy");
  if (n.item === "failed") expect(row).toContain(`Did not save — ${ACT_FAIL}`);
  else expect(row).not.toContain("Did not save");
}

function checkActivity(n: Node, html: string) {
  expect(header(html)).toContain("<h1>Activity</h1>");
  expect(crumb(html)).not.toBe("");
  if (n.data !== "loaded") return checkWaiting(n, html, "activity");
  expect(html).toContain("wk-stats");
  expect(button(header(html), "Ingest now")).not.toBe("");
  // The note is WikiPage's: what the last Ingest now said, still said on the next visit.
  const notes = [...header(html).matchAll(/<span class="hk-note" role="status">([^<]*)<\/span>/g)].map((m) => m[1]);
  expect(notes).toEqual(n.note ? [noteText(n)] : []);
}

function checkPage(n: Node, html: string) {
  const head = header(html);
  expect(crumb(html)).not.toBe("");
  const doc = element(html, /<div class="scroll wk-doc">/);
  expect(doc).not.toBe("");
  const loaded = n.data === "loaded";

  // ---- the page's own read ----
  if (n.data === "missing") {
    // MissingOnlyForPages holds by construction; a missing page offers a way on, never Retry.
    expect(doc).toContain("This page doesn’t exist");
    expect(button(doc, "Search the wiki")).not.toBe("");
    expect(button(doc, "Back to wiki")).not.toBe("");
    expect(button(doc, "Retry")).toBe("");
  } else if (!loaded) {
    checkWaiting(n, html, "the page");
  } else {
    expect(doc).not.toContain("error-note");
    expect(doc).not.toContain("doesn’t exist");
  }

  // ---- the header's actions: only over a loaded page, hidden while editing ----
  const acts = element(head, /<div class="hk-acts wk-page-acts">/);
  expect(acts !== "").toBe(loaded && n.panel !== "edit");
  const menuBtn = /<button class="wk-x" aria-label="Page actions"[^>]*>/.exec(head)?.[0] ?? "";
  const menu = element(head, /<div class="overflow-menu" role="menu">/);
  if (acts) {
    // Edit and History are the wide header's, the "..." menu the narrow one's: the CSS picks by viewport.
    expect(button(acts, "Edit")).toContain('class="btn wk-wide"');
    const hist = button(acts, "History");
    expect(hist).toContain('class="btn wk-wide"');
    expect(hist).toContain(`aria-expanded="${n.panel === "history"}"`);
    expect(acts).toMatch(/<div class="wk-narrow wk-more">/);
    expect(menuBtn).toContain(`aria-expanded="${n.menu}"`);
    expect(menu !== "").toBe(n.menu);
    if (menu) {
      const items = [...menu.matchAll(/<button role="menuitem"[^>]*>([^<]*)<\/button>/g)].map((m) => m[1]);
      expect(items).toEqual(["Edit", n.panel === "history" ? "Hide history" : "History"]);
    }
  } else {
    expect(menuBtn).toBe("");
    // An open menu is invisible anywhere else: the node must not claim it.
    expect(menu).toBe("");
  }

  // ---- the doc: alert, history, editor ----
  // The last Save's error renders only inside a loaded page (it waits there while the next page loads).
  const alerts = [...doc.matchAll(/<p class="hk2-alert">([^<]*)<\/p>/g)].map((m) => m[1]);
  expect(alerts).toEqual(loaded && n.alert ? [SAVE_FAIL] : []);
  const history = element(doc, /<section class="wk-history[^"]*"/);
  expect(history !== "").toBe(n.panel === "history");
  const area = /<textarea id="wk-body"[^>]*>/.exec(doc)?.[0] ?? "";
  expect(area !== "").toBe(n.panel === "edit");
  // PanelsNeedLoadedPage, SavingOnlyInEditor: a Save button exists only in the editor, disabled exactly while saving.
  const save = /<button class="btn btn-primary"[^>]*>Save<\/button>/.exec(doc)?.[0] ?? "";
  expect(save !== "").toBe(n.panel === "edit");
  if (save) {
    expect(save.includes("disabled")).toBe(n.saving);
    expect(button(doc, "Cancel")).not.toBe("");
    expect(doc).not.toContain('class="wk-titleblock"');
  } else {
    expect(n.saving).toBe(false);
    if (loaded) expect(doc).toContain('class="wk-titleblock"');
  }

  // ---- the cited-entry pane: up when the route has a cite and the page has loaded ----
  const split = /<div class="wk-split" data-source="([01])">/.exec(html)!;
  const open = n.cite && loaded;
  expect(split[1]).toBe(open ? "1" : "0");
  const pane = element(html, /<section class="wk-src" aria-label="Cited entry">/);
  expect(pane !== "").toBe(open);
  if (!pane) return;
  expect(pane).toContain('aria-label="Close the entry"');
  switch (n.source) {
    case "loading":
      expect(pane).toContain("Loading the cited entry…");
      expect(pane).toMatch(/aria-busy="true"/);
      expect(pane).not.toContain("wk-ent");
      break;
    case "error":
      expect(pane).toContain("Couldn’t load the cited entry");
      expect(button(pane, "Retry")).not.toBe("");
      expect(pane).not.toContain("wk-ent");
      break;
    case "loaded":
      expect(count(pane, /class="wk-ent[ "]/)).toBe(2);
      expect(pane).toMatch(/<div class="wk-ent wk-ent-on" aria-current="true">/);
      expect(pane).not.toContain('aria-busy="true"');
      break;
    default:
      throw new Error(`a pane with source ${n.source}`);
  }
  // The chip that opened it reads as pressed.
  expect(doc).toMatch(/<button class="wk-cite wk-cite-on"[^>]*aria-pressed="true"/);
}

function checkPalette(n: Node) {
  const html = palette(n);
  const dialogs = count(html, /role="dialog"/);
  expect(dialogs).toBe(n.palette ? 1 : 0);
  if (!n.palette) return;
  // EscapeOwnedByPalette: the pane's Escape listener stands down for `[role=dialog][open], .pal`;
  // the palette must carry the selector it looks for, and be modal so nothing under it takes the click.
  const box = /<div class="pal" role="dialog" aria-modal="true"[^>]*>/.exec(html);
  expect(box).not.toBeNull();
}

describe("ui_wiki.fizz, every node rendered", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBe(60);
  });

  for (const [key, n] of nodes) {
    test(key, () => {
      // The spec's own invariants, on the node itself.
      expect((n.source !== "none") === (n.route === "page" && n.cite)).toBe(true);
      expect(n.escape_under_dialog).toBe(false);
      if (n.route !== "page") expect([n.panel, n.saving, n.alert, n.menu, n.cite]).toEqual(["none", false, false, false, false]);
      if (n.route !== "review") expect(n.item).toBe("none");

      const html = screen(n);
      // One screen at a time, and no dialog of its own: the palette is the one.
      expect(count(html, /class="thread"/)).toBe(1);
      expect(count(html, /role="dialog"/)).toBe(0);
      // The activity note shows on Activity only; elsewhere WikiPage keeps it unseen.
      if (n.route !== "activity") expect(html).not.toContain(STARTED);
      switch (n.route) {
        case "index": checkIndex(n, html); break;
        case "review": checkReview(n, html); break;
        case "activity": checkActivity(n, html); break;
        case "page": checkPage(n, html); break;
      }
      checkPalette(n);
    });
  }
});
