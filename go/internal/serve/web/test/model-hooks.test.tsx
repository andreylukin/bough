import { describe, expect, mock, test } from "bun:test";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { renderToStaticMarkup } from "react-dom/server";
// No DOM under bun: markdown sanitising is not what these assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { HooksPageView, SourceView } = await import("../src/hooks");
const { ViewNav } = await import("../src/app");
import type { HooksData, SourceState } from "../src/hooks";

// go/tests/model/specs/ui_hooks.fizz at the component level: every
// reachable settled state of the graph is rendered from props built from that node, and the node's
// meaning is asserted on the markup. The browser walk
// (tests/web/specs/model/ui_hooks.spec.ts) drives the same graph through
// a real serve; this one names a broken render in milliseconds and needs
// no Chromium.

type Node = { route: string; list: string; panel: string; file: string; edited: boolean; save: string; dry: string };

// Every settled state of the graph, read from the checked-in graph itself
// (the walks the browser spec takes are derived from the same file).
const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_hooks", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Page#0.")) n[k.slice(7)] = v;
  nodes.set(JSON.stringify(n, Object.keys(n).sort()), n as Node);
}

const EVENT = "pre-code-exec";
const PATH = "/home/u/.bough/hooks/pre-code-exec/guard.js";
const BODY = "// guard: lets every block run\nreturn {};\n";
const EDIT = "// edit 1";
const FILE_FAIL = "File read failed";
const SAVE_FAIL = "Save failed";
const DRY_FAIL = "Dry run failed";
const LIST_FAIL = "List read failed";
const data: HooksData = {
  hooks: [{ id: "h1", off: false, name: "guard.js", event: EVENT, path: PATH, scope: "home", shadowed: false,
    lastFired: null, lastDecision: "", failing: false, error: "" }],
  watchers: [], fires: [], rules: [], plugins: [],
};

/** What Source holds at a node: one note slot and one err slot for both requests. */
function sourceState(n: Node): SourceState {
  let note = "", err = "";
  if (n.file === "error") err = FILE_FAIL;
  if (n.save === "saved") note = "Saved.";
  if (n.save === "error") err = SAVE_FAIL;
  if (n.dry === "result") note = "Dry run finished in 3ms: {}";
  if (n.dry === "error") err = DRY_FAIL;
  return {
    open: n.panel === "open",
    body: n.file === "loaded" ? (n.edited ? BODY + EDIT : BODY) : null,
    note, err,
    saving: n.save === "saving",
    asked: n.file !== "none",
    running: n.dry === "running" ? 1 : 0,
  };
}

/** What HooksPage holds at a node: data once a read answered, err while the last read failed. */
function pageProps(n: Node) {
  const rows = n.list === "loaded" || n.list === "stale";
  return {
    data: rows ? data : null,
    err: n.list === "error" || n.list === "stale" ? LIST_FAIL : "",
    at: Date.UTC(2026, 8, 24, 12),
    // "slow" is Pending's own clock past its timeout: a zero timeout is already past it.
    timeout: n.list === "slow" ? 0 : undefined,
  };
}

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
const button = (html: string, text: string) => {
  const m = new RegExp(`<button[^>]*>${text}</button>`).exec(html);
  return m ? m[0] : "";
};

function checkNav(n: Node) {
  const nav = renderToStaticMarkup(<ViewNav view={n.route === "hooks" ? "hooks" : "projects"} onView={() => {}} />);
  const hooks = /<a[^>]*href="#\/hooks"[^>]*>/.exec(nav)![0];
  // The nav claims the page exactly while it is mounted.
  expect(hooks.includes('aria-current="page"')).toBe(n.route === "hooks");
  expect(count(nav, /aria-current="page"/)).toBe(1);
}

function checkList(n: Node, html: string) {
  expect(html).toContain("<h1>Hooks</h1>");
  expect(count(html, /role="dialog"/)).toBe(0);
  const body = element(html, /<div class="scroll proj-body">/);
  const rows = count(body, /class="hk-toggle"/);
  switch (n.list) {
    case "loading":
      expect(body).toMatch(/<div class="skeleton" role="status" aria-live="polite" aria-busy="true">/);
      expect(body).toContain("Loading hooks…");
      expect(body).not.toContain("error-note");
      expect(rows).toBe(0);
      break;
    case "slow":
    case "error": {
      expect(body).toContain(n.list === "slow" ? "Hooks is taking too long" : "Couldn’t load hooks");
      if (n.list === "error") expect(body).toContain(LIST_FAIL);
      expect(button(body, "Retry")).not.toBe("");
      expect(button(body, "Retry")).not.toContain("disabled");
      expect(body).not.toContain('aria-busy="true"');
      expect(rows).toBe(0);
      break;
    }
    case "loaded":
    case "stale":
      expect(body).toContain('class="hk-chips" role="group" aria-label="Show"');
      expect(rows).toBe(1);
      // The list's own Source: the row mounts it closed and unread.
      expect(body).toContain('aria-expanded="false"');
      expect(body).toContain(">Open definition</button>");
      if (n.list === "stale") {
        const stale = element(body, /<p class="hk-stale" role="status">/);
        expect(stale).toContain("Stale · last updated");
        expect(stale).toContain(LIST_FAIL);
        expect(button(stale, "Retry")).not.toBe("");
      } else {
        expect(body).not.toContain("hk-stale");
      }
      break;
    default:
      throw new Error(`unknown list state ${n.list}`);
  }
}

function checkSource(n: Node, html: string) {
  expect(count(html, /role="dialog"/)).toBe(0);
  const toggle = /<button class="hk-toggle"[^>]*>([^<]*)<\/button>/.exec(html)!;
  expect(toggle[0]).toContain(`aria-expanded="${n.panel === "open"}"`);
  expect(toggle[1]).toBe(n.panel === "open" ? "Hide definition" : "Open definition");
  const controls = /aria-controls="([^"]+)"/.exec(toggle[0])![1];
  const panelTag = /<div class="hk-panel"[^>]*>/.exec(html)![0];
  expect(panelTag).toContain(`id="${controls}"`);
  expect(panelTag.includes('hidden=""')).toBe(n.panel === "closed");
  const panel = element(html, /<div class="hk-panel"[^>]*>/);
  const inner = panel.slice(panelTag.length, -"</div>".length);
  const alerts = count(inner, /role="alert"/);
  const textarea = /<textarea[^>]*>([^<]*)<\/textarea>/.exec(inner);

  // OpenPanelNeverBlank: an open panel always says something.
  if (n.panel === "open") expect(inner.trim()).not.toBe("");

  switch (n.file) {
    case "none":
      expect(inner).toBe("");
      break;
    case "loading":
      expect(inner).toMatch(/role="status"[^>]*aria-busy="true"/);
      expect(inner).toContain("Loading file…");
      expect(textarea).toBeNull();
      expect(alerts).toBe(0);
      break;
    case "error": {
      const err = element(inner, /<p class="err hk-loaderr" role="alert">/);
      expect(err).toContain(`Could not open the file — ${FILE_FAIL}`);
      expect(button(err, "Retry")).not.toBe("");
      expect(textarea).toBeNull();
      expect(alerts).toBe(1);
      break;
    }
    case "loaded":
      expect(textarea).not.toBeNull();
      break;
    default:
      throw new Error(`unknown file state ${n.file}`);
  }
  if (n.file !== "loaded") {
    // ActionsNeedText: no Save, no Dry run, no verdict without the text.
    expect(inner).not.toContain("hk-acts");
    expect(inner).not.toContain("hk-note");
    return;
  }

  const saving = n.save === "saving";
  expect(textarea![0].includes("disabled")).toBe(saving);
  expect(textarea![1].endsWith(EDIT)).toBe(n.edited);
  expect(textarea![0]).toMatch(/id="([^"]+)"/);
  const areaId = /id="([^"]+)"/.exec(textarea![0])![1];
  expect(inner).toContain(`<label class="visually-hidden" for="${areaId}">File contents</label>`);

  const save = /<button class="btn btn-primary"[^>]*>([^<]*)<\/button>/.exec(inner)!;
  expect(save[1]).toBe(saving ? "Saving…" : "Save");
  expect(save[0].includes("disabled")).toBe(saving);

  // Dry run is never disabled (KNOWN GAPS); a request in flight says so.
  const dry = /<button class="btn"[^>]*>([^<]*)<\/button>/.exec(inner)!;
  expect(dry[0]).not.toContain("disabled");
  expect(dry[0].includes('aria-busy="true"')).toBe(n.dry === "running");
  expect(dry[1]).toBe(n.dry === "running" ? "Running…" : "Dry run");

  // One note slot: Saved. or the dry run's result, never both (OneNoteOneAlert).
  const notes = [...inner.matchAll(/<span class="hk-note" role="status">([^<]*)<\/span>/g)].map((m) => m[1]);
  expect(notes).toEqual(n.save === "saved" ? ["Saved."] : n.dry === "result" ? ["Dry run finished in 3ms: {}"] : []);
  // SavedMeansClean.
  if (n.save === "saved") expect(n.edited).toBe(false);

  // One err slot: the save's failure or the dry run's, never both.
  const errs = [...inner.matchAll(/<p class="err" role="alert">([^<]*)<\/p>/g)].map((m) => m[1]);
  expect(errs).toEqual(n.save === "error" ? [SAVE_FAIL] : n.dry === "error" ? [DRY_FAIL] : []);
  expect(alerts).toBe(errs.length);
}

const noop = () => {};

describe("ui_hooks.fizz, every node rendered", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBeGreaterThan(1);
  });

  for (const [key, n] of nodes) {
    test(key, () => {
      checkNav(n);
      if (n.route === "away") {
        // AwayHoldsNothing: HooksPage is unmounted, so the node is its initial state.
        expect(n).toEqual({ route: "away", list: "loading", panel: "closed", file: "none", edited: false, save: "idle", dry: "idle" });
        return;
      }
      // SourceNeedsRows: without data there is no row, so Source is closed and unread.
      if (n.list !== "loaded" && n.list !== "stale") expect([n.panel, n.file]).toEqual(["closed", "none"]);
      checkList(n, renderToStaticMarkup(<HooksPageView {...pageProps(n)} onRetry={noop} />));
      checkSource(n, renderToStaticMarkup(
        <SourceView id="src" path={PATH} definition state={sourceState(n)}
                    onToggle={noop} onRead={noop} onEdit={noop} onSave={noop} onDryRun={noop} />,
      ));
    });
  }
});
