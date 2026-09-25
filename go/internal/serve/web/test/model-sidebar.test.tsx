import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { Sidebar, type SidebarSeed } from "../src/app";
import type { Project, Row, Status } from "../src/types";

// go/tests/model/specs/ui_sidebar.fizz at the component level: every
// settled state of the checked-in graph is rendered as the Sidebar App
// would mount it in that state, the state is read back off the markup
// the way the browser walk (tests/web/specs/model/ui_sidebar.spec.ts)
// reads it off the DOM, and the spec's invariants are asserted on what
// was read. The walk takes ~half a second a node through a real serve;
// this names a broken render in milliseconds.
//
// What static markup cannot hold, the walk owns: focus (inField), the
// route and the pane (App's), and a phone's thread pane hiding the list
// (App's data-pane and CSS; the Sidebar itself renders the same).

type Node = {
  viewport: string; pane: string; route: string; closed: boolean; search: string; inField: boolean;
  rows: string; a: string; unseen: boolean; trouble: boolean; project: string; proj: boolean;
  groupFolded: boolean; arch: string; archFromFilter: boolean; move: string; card: boolean;
  side: string; says: string; aRow: string; dot: boolean; seenBtn: boolean;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_sidebar", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Sidebar#0.")) n[k.slice(10)] = v;
  nodes.set(JSON.stringify(n, Object.keys(n).sort()), n as Node);
}

// The world the walk builds: A in a folder, B filed into p, Z archived.
// Neither query matches B or Z; "alpha" matches A.
const WORK = "/w/work";
const HIT = "alpha", MISS = "zzqx";
const at = new Date(Date.now() - 60_000).toISOString();
const base = { cwd: WORK, repo: "", live: false, archived: false, entries: 3, modified: at, lastAt: at, mode: "local" } as const;
const projects: Project[] = [{ slug: "p", name: "p" }];
const DROP_P = "recent:project:p";

function rowsOf(n: Node): Row[] {
  const rows: Row[] = [];
  if (n.a !== "none") {
    rows.push({
      ...base, id: "A", title: "alpha task",
      status: n.a === "failed" ? "error" : (n.a as Status), live: n.a === "running",
      unseen: n.unseen || undefined, trouble: n.trouble ? "failed" : undefined,
      project: n.project || undefined, turns: 1,
    });
  }
  if (n.proj) rows.push({ ...base, id: "B", title: "bravo", status: "done", project: "p", turns: 1 });
  // Z is in the rows once Archived's read answered.
  if (n.arch === "open" || n.arch === "folded") rows.push({ ...base, id: "Z", title: "zulu", status: "done", archived: true, turns: 1 });
  // App hands the Sidebar the rows its query matches (visible).
  const q = n.search === "hit" ? HIT : n.search === "miss" ? MISS : "";
  return q ? rows.filter((r) => r.title.includes(q)) : rows;
}

/** The Sidebar as App mounts it at this node, rendered with the page's saved flags and media. */
function render(n: Node): string {
  const query = n.search === "hit" ? HIT : n.search === "miss" ? MISS : "";
  const archLoading = n.arch === "loading" || n.arch === "failed";
  const loadErr = n.rows === "unavailable" || n.rows === "delayed" || n.arch === "failed" ? "list read failed" : null;
  const seed: SidebarSeed = {
    searching: n.search === "open",
    // The first read's notice waits 200 ms; the walk's clock is past it.
    slow: n.rows === "loading",
    card: n.card ? { id: "A", top: 8, left: 8 } : null,
    dragging: n.move === "dragging" || n.move === "over" ? "A" : null,
    dropAt: n.move === "over" ? DROP_P : null,
    archFolded: n.arch === "folded",
  };
  const onScreen = n.route === "session" && (n.viewport === "desktop" || n.pane === "thread");
  const store: Record<string, string> = {
    "bough:side-closed": n.closed ? "1" : "0",
    "bough:ws-folded": JSON.stringify(n.groupFolded ? [`recent:${WORK}`] : []),
  };
  const g = globalThis as Record<string, unknown>;
  const saved = { window: g.window, localStorage: g.localStorage };
  g.window = { matchMedia: (q: string) => ({ matches: q === "(max-width:720px)" && n.viewport === "phone" }) };
  g.localStorage = { getItem: (k: string) => store[k] ?? null, setItem: () => {} };
  try {
    return renderToStaticMarkup(
      <Sidebar rows={rowsOf(n)} projects={projects} selected={n.route === "session" ? "A" : null}
               viewing={onScreen ? "A" : ""} active={n.viewport === "desktop" || n.pane === "list"}
               onSelect={noop} query={query} onQuery={noop}
               showArchived={n.arch !== "off"} onToggleArchived={noop}
               archivedState={n.arch === "loading" ? "loading" : n.arch === "failed" ? "failed" : "ready"}
               view={n.route === "page" ? "hooks" : "sessions"} onView={noop} onNew={noop} onAck={noop} onMove={noop}
               loadedAt={n.rows === "loading" || n.rows === "unavailable" ? null : Date.now() - 60_000}
               // Until Archived has loaded, a failed read is its read (App passes no loadErr then).
               loadErr={archLoading ? null : loadErr} onRetry={noop} onRetryArchived={noop} seed={seed} />,
    );
  } finally {
    Object.assign(g, saved);
    for (const k of ["window", "localStorage"] as const) if (saved[k] === undefined) delete g[k];
  }
}
const noop = () => {};

/** The markup of one element, found by an opening-tag pattern, through its matching close. */
function element(html: string, open: RegExp, from = 0): { text: string; end: number } | null {
  const re0 = new RegExp(open.source, "g");
  re0.lastIndex = from;
  const m = re0.exec(html);
  if (!m) return null;
  const tag = /^<(\w+)/.exec(m[0])![1];
  let depth = 0;
  const re = new RegExp(`<(/?)${tag}\\b[^>]*?(/?)>`, "g");
  re.lastIndex = m.index;
  for (let t = re.exec(html); t; t = re.exec(html)) {
    if (t[2]) continue;
    depth += t[1] ? -1 : 1;
    if (depth === 0) return { text: html.slice(m.index, re.lastIndex), end: re.lastIndex };
  }
  return { text: html.slice(m.index), end: html.length };
}
function all(html: string, open: RegExp): string[] {
  const out: string[] = [];
  for (let e = element(html, open); e; e = element(html, open, e.end)) out.push(e.text);
  return out;
}
const text = (html: string) => html.replace(/<[^>]*>/g, "").replace(/&#x27;/g, "'").replace(/&quot;/g, '"').replace(/&amp;/g, "&");

/** The state the markup shows, read as the browser walk's readUiState reads the DOM. */
function read(html: string) {
  const side = html.includes('class="sidebar sidebar-closed"') ? "rail" : "full";
  const closed = /<button class="side-icon side-collapse"[^>]*aria-expanded="false"/.test(html);
  const q = /<input id="q"[^>]*value="([^"]*)"/.exec(html);
  const search = !q ? "off" : q[1] === "" ? "open" : q[1] === HIT ? "hit" : q[1] === MISS ? "miss" : `unknown: ${q[1]}`;
  const fresh = text(element(html, /<p class="side-fresh"[^>]*>/)?.text ?? "");
  const rows = fresh.startsWith("Sessions unavailable") ? "unavailable" : fresh.startsWith("Loading sessions") ? "loading"
    : fresh.startsWith("Updates delayed") ? "delayed" : fresh ? `unknown: ${fresh}` : "ready";
  const tree = element(html, /<div class="scroll" role="tree"[^>]*>/)?.text ?? "";
  const needs = element(tree, /<div class="needs">/)?.text ?? "";
  // The recent groups: the tree's own .ws children, before the sections.
  const secs = all(tree, /<div class="sec">/);
  let recent = tree.replace(needs, "");
  for (const s of secs) recent = recent.replace(s, "");
  const wraps = (h: string) => all(h, /<div class="row-wrap[^"]*">/).filter((w) => w.includes('data-id="A"'));
  const pinned = wraps(needs), grouped = wraps(recent);
  const folderHead = /<button class="ws-head" role="treeitem" aria-expanded="(true|false)"(?! data-project)/.exec(recent);
  const aRow = pinned.length ? "pinned" : grouped.length ? "group" : folderHead?.[1] === "false" ? "folded" : "none";
  const drawn = [...pinned, ...grouped];
  const aBtn = drawn.map((w) => /<button role="treeitem"[^>]*>/.exec(w)![0]);
  const arch = (() => {
    const sec = secs.find((s) => s.includes('aria-controls="sec-archived"'));
    if (!sec) return "off";
    const fold = /<button type="button" class="sec-fold"[^>]*>/.exec(sec)![0];
    const counted = sec.includes('class="num sec-count"');
    const body = element(sec, /<div class="sec-body"[^>]*>/)?.text ?? "";
    if (/class="pending/.test(body)) return "loading";
    if (body.includes("Couldn’t load archived")) return "failed";
    if (counted) return fold.includes('aria-expanded="true"') ? "open" : "folded";
    return fold.includes('aria-expanded="false"') ? "off" : "unknown: open without a count";
  })();
  const archCount = /<span class="num sec-count"><span class="visually-hidden">, <\/span>(\d+)<\/span>/.exec(tree)?.[1];
  const anything = /<button [^>]*class="(row|ws-head)[" ]/.test(tree) || (archCount !== undefined && archCount !== "0");
  const none = text(/<p class="list-none">(.*?)<\/p>/.exec(tree.replace(secs.join(""), ""))?.[1] ?? "");
  const says = side !== "full" ? side : rows !== "ready" ? rows : anything ? "rows"
    : arch === "loading" || arch === "failed" ? "archived"
    : none.startsWith("No sessions match") ? "nomatch" : none === "No sessions yet." ? "empty" : `unknown: ${none}`;
  return {
    side, closed, search, rows, aRow, arch, says,
    dot: drawn.some((w) => w.includes('class="unseen-dot"')),
    seenBtn: drawn.some((w) => w.includes('class="btn row-ack"')),
    card: html.includes('id="row-card"') && aBtn.some((b) => b.includes('aria-describedby="row-card"')),
    dropOffered: /<div class="ws" data-drop="(ok|over)"/.test(html),
    over: html.includes('<div class="ws" data-drop="over">'),
    hint: /<p class="ws-drop-hint">([^<]*)<\/p>/.exec(html)?.[1] ?? "",
    include: html.includes('class="sec-include"'),
    field: Boolean(q),
  };
}

describe("ui_sidebar.fizz, every node rendered", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBeGreaterThan(100);
  });

  for (const [key, n] of nodes) {
    test(key, () => {
      const html = render(n);
      const got = read(html);
      // A phone never folds to the rail: its list is the pane, and the
      // pane (App's data-pane) is what hides it on the thread.
      expect(got.side).toBe(n.side === "hidden" ? "full" : n.side);
      expect(got.closed).toBe(n.closed);
      if (n.side === "hidden") return;
      expect({ aRow: got.aRow, dot: got.dot, seenBtn: got.seenBtn, card: got.card, says: got.says })
        .toEqual({ aRow: n.aRow, dot: n.dot, seenBtn: n.seenBtn, card: n.card, says: n.says });
      if (n.side === "full") {
        expect({ search: got.search, rows: got.rows, arch: got.arch }).toEqual({ search: n.search, rows: n.rows, arch: n.arch });
      }

      // FieldFocusNeedsField: focus in the field needs the field drawn.
      if (n.inField) expect(got.field).toBe(true);
      // CardOverDrawnRow.
      if (got.card) { expect(["group", "pinned"]).toContain(got.aRow); expect(got.dropOffered).toBe(false); }
      // NoDotOnOpenRowOrFailure.
      if (got.dot) { expect(n.route).not.toBe("session"); expect(n.a).toBe("done"); }
      // SeenOnlyOnTrouble.
      if (got.seenBtn) { expect(n.trouble).toBe(true); expect(n.a).toBe("failed"); }
      // EmptyMeansNothing: the empty line only over an empty tree.
      if (got.says === "empty") expect(html).not.toMatch(/<button [^>]*class="(row|ws-head)[" ]/);
      // FailurePinned: a failure is pinned whenever the list shows rows unfiltered.
      if (got.side === "full" && ["ready", "delayed"].includes(got.rows) && ["off", "open"].includes(got.search) && n.a === "failed") {
        expect(got.aRow).toBe("pinned");
      }
      // DropOnlyWhereItLands: the drop hint names p, over p's group, for a local A.
      if (got.over) { expect(got.hint).toBe("Move to p"); expect(n.project).toBe(""); expect(n.proj).toBe(true); }
      expect(got.over).toBe(n.move === "over");
      // A drag offers p's group only while one is under way and p has a group.
      expect(got.dropOffered).toBe((n.move === "dragging" || n.move === "over") && n.proj && n.project === "");
      // IncludeLastsWithFilter: "Include" is offered only under a query, and
      // Archived included from one is shown only while that query holds.
      if (got.include) expect(["hit", "miss"]).toContain(n.search);
      if (n.archFromFilter) expect(["hit", "miss"]).toContain(got.search);
      // ListSaysSomething.
      if (got.side === "full") expect(["loading", "unavailable", "delayed", "empty", "nomatch", "rows", "archived"]).toContain(got.says);
    });
  }
});
