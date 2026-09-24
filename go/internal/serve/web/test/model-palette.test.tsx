import { expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import type { Command } from "../src/palette";
import type { Row } from "../src/types";
// No DOM under bun: the portals render in place.
const real = await import("react-dom");
mock.module("react-dom", () => ({ ...real, createPortal: (node: unknown) => node }));
(globalThis as { document?: unknown }).document ??= { body: {} };
const { PaletteView } = await import("../src/palette");
const { DialogHost } = await import("../src/dialog");
const { StartingStatus } = await import("../src/loading");

// Every reachable state of go/tests/model/specs/ui_palette.fizz, rendered
// from its fields and checked on the markup. The browser walk
// (tests/web/specs/model/ui_palette.spec.ts) reaches the same nodes in
// minutes; this renders all of them in milliseconds, so a render that
// breaks one node's invariant shows here first.

// ---- the graph's nodes, read off the generated protobuf ----

type Node = {
  route: "list" | "s" | "new"; running: boolean; open: boolean; mode: "all" | "switch" | "new"; aimed: boolean;
  query: "empty" | "text" | "path" | "full" | "nopath"; search: "idle" | "loading" | "done" | "error";
  dirs: "none" | "pending" | "answered"; at: string; dialog: boolean; edited: boolean; focus: string; creating: boolean;
};

// Nodes is `repeated string json = 1`: tag 0x0a, a varint length, the JSON.
function readNodes(file: string): Node[] {
  const b = readFileSync(file);
  const out: Node[] = [];
  for (let i = 0; i < b.length;) {
    if (b[i++] !== 0x0a) throw new Error(`nodes: unexpected tag at ${i - 1}`);
    let n = 0;
    for (let shift = 0; ; shift += 7) { const x = b[i++]; n += (x & 0x7f) * 2 ** shift; if (x < 0x80) break; }
    const node = JSON.parse(b.subarray(i, i + n).toString("utf8"));
    i += n;
    // Only settled states: a fork inside an action would be named after it.
    if (node.name === "yield") out.push(node.roles[0].fields);
  }
  return out;
}

const nodes = readNodes(new URL("../../../../tests/model/testdata/ui_palette/nodes_000000_of_000000.pb", import.meta.url).pathname);

// ---- the spec's projections, as written in ui_palette.fizz ----

function hits(p: Node): string[] {
  if (!p.open) return [];
  if (p.query === "empty") {
    if (p.mode === "new") return ["cmd", "ask", "project"];
    const recent = p.route !== "s" ? ["session"] : [];
    if (p.mode === "switch") return recent;
    const mine = p.route === "s" ? (p.running ? ["this", "stop"] : ["this"]) : [];
    return [...recent, "cmd", ...mine];
  }
  if (p.query === "text") return p.search === "done" ? ["session", "start"] : ["start"];
  if (p.dirs !== "answered") return [];
  return p.query === "nopath" ? ["missing", "start"] : ["dir", "start"];
}

function sel(p: Node): string {
  const h = hits(p);
  if (h.length === 0) return "";
  return h.includes(p.at) ? p.at : h[0];
}

function enter(p: Node): string {
  const s = sel(p);
  if (["dir", "missing", "start"].includes(s)) return "Start";
  return s === "session" ? "Open" : "Run";
}

// ---- the fixture behind the abstract values (the spec's header) ----

const HOME = "/home/u";
const S = "s-000000aaaaaa";
const N = "s-000000bbbbbb"; // the session a create made
const PHRASE = "zqxj"; // said in S's transcript only: no title or label holds it, not even as a subsequence
const QUERY: Record<Node["query"], string> = { empty: "", text: PHRASE, path: "~/wor", full: "~/work/", nopath: "~/nope" };

const at1 = new Date(Date.now() - 3_600_000).toISOString();
const row = (id: string, title: string, over: Partial<Row> = {}): Row => ({
  id, title, cwd: HOME, status: "done", live: false, archived: false, entries: 4, modified: at1, lastAt: at1, ...over,
});

// The palette's commands as app.tsx builds them for this fixture: home is
// known, serve's folder is no checkout, no projects, no wiki, no failures.
function commands(p: Node): Command[] {
  const c = (id: string, label: string, group: string, extra: Partial<Command> = {}): Command => ({ id, label, group, run: () => {}, ...extra });
  const open = p.route !== "list";
  return [
    c("new:here", "New session in home", "Start", { suggest: true, hint: "Folder: ~" }),
    c("new:folder", "New session in a folder…", "Start", { hint: "a git checkout can be edited" }),
    c("help:welcome", "Show the welcome", "Navigation"),
    c("new:project", "New project…", "Start"),
    c("go:sessions", "Sessions", "Navigation"),
    c("go:hooks", "Hooks", "Navigation"),
    ...(open ? [
      c("s:changes", "Review changes", "This session", { suggest: true, hint: "Session edits" }),
      c("s:context", "Inspect context", "This session", { suggest: true, hint: "Context" }),
      c("s:portal", "Open portal", "This session", { hint: "Server in the orb" }),
      c("s:archive", "Archive this session", "This session", { destructive: true }),
      ...(p.running && p.route === "s" ? [c("s:stop", "Stop this turn", "This session", { suggest: true })] : []),
    ] : []),
  ];
}

// A kind's option id, as the palette names it.
function idOf(kind: string, p: Node): string {
  return ({
    session: "s:" + S, cmd: "new:here", ask: "new:folder", project: "new:project", stop: "s:stop",
    dir: "dir:" + HOME + "/work", missing: "dir:" + HOME + "/nope", start: "start:" + QUERY[p.query],
  } as Record<string, string>)[kind] ?? "";
}

// An option id back to its kind (the browser walk's rowKind).
function kindOf(id: string): string {
  if (id === "s:" + S) return "session";
  if (id === "new:here") return "cmd";
  if (id === "new:folder") return "ask";
  if (id === "new:project") return "project";
  if (id === "s:changes" || id === "s:context") return "this";
  if (id === "s:stop") return "stop";
  if (id === "dir:" + HOME + "/nope") return "missing";
  if (id.startsWith("dir:")) return "dir";
  if (id.startsWith("start:")) return "start";
  return "unknown:" + id;
}

// What /api/dirs answered for the typed path (serve/setup.go dirs).
function places(p: Node) {
  const none = { folder: null, dirs: [], home: "" };
  if (p.dirs !== "answered") return none;
  if (p.query === "path") return { folder: { path: HOME + "/wor", exists: false }, dirs: [{ path: HOME + "/work", exists: true }], home: HOME };
  if (p.query === "full") return { folder: { path: HOME + "/work", exists: true }, dirs: [], home: HOME };
  return { folder: { path: HOME + "/nope", exists: false }, dirs: [], home: HOME };
}

const noop = () => {};

function render(p: Node): string {
  const rows = [row(S, "Fix the flaky login test", { status: p.running ? "running" : "done", live: p.running }),
    ...(p.route === "new" ? [row(N, "New session", { empty: true, live: true, entries: 1 })] : [])];
  const found = p.search === "done"
    ? [{ id: S, title: rows[0].title, repo: "", branch: "", hits: 1, lines: [{ seq: 3, kind: "user", text: `the ${PHRASE} phrase` }] }]
    : [];
  const q = QUERY[p.query];
  return renderToStaticMarkup(<>
    <PaletteView open={p.open} onClose={noop} rows={rows} commands={commands(p)} mode={p.mode} visited={[]}
      onOpenSession={noop} onOpenWikiPage={noop} onStart={noop} onStartIn={noop}
      current={p.route === "s" ? S : p.route === "new" ? N : null}
      startIn={p.aimed ? "work" : undefined}
      q={q} setQ={noop} atId={p.at ? idOf(p.at, p) : null} setAtId={noop}
      search={{ hits: found, state: p.search, retry: noop }} pages={[]} places={places(p)}
      field={{ current: null }} opener={{ current: null }} />
    <DialogHost seed={p.dialog ? {
      req: { kind: "text", title: "Start in folder", initial: "~", placeholder: "~/code/your-repo", action: "Start", allowEmpty: false, resolve: noop },
      text: p.edited ? "~/work" : "~",
    } : undefined} />
    {p.creating && <StartingStatus />}
  </>);
}

const name = (p: Node) => Object.entries(p).filter(([k]) => k !== "focus").map(([k, v]) => `${k}=${v}`).join(" ");
const attr = (tag: string, a: string) => new RegExp(`${a}="([^"]*)"`).exec(tag)?.[1];

test("the graph has every state the generator covered", () => {
  expect(nodes.length).toBe(44);
  expect(new Set(nodes.map((n) => JSON.stringify(n))).size).toBe(44);
});

for (const p of nodes) {
  test(`ui_palette: ${name(p)}`, () => {
    const html = render(p);

    // OneModal: exactly the modals the node has up, never two.
    const modals = html.match(/aria-modal="true"/g)?.length ?? 0;
    expect(modals).toBe((p.open ? 1 : 0) + (p.dialog ? 1 : 0));
    expect(modals).toBeLessThanOrEqual(1);
    expect(html.includes('aria-label="Quick access"')).toBe(p.open);

    // A create in flight says so.
    expect(html.includes("Starting a session…")).toBe(p.creating);
    if (p.creating) expect(html).toMatch(/role="status"[^>]*>(<[^>]*>)*[^<]*Starting a session…/);

    // The "Start in folder" dialog: Start waits for an edit, Cancel never does.
    if (p.dialog) {
      expect(html).toContain('<h2 id="dlg-title" class="dlg-title">Start in folder</h2>');
      const start = /<button[^>]*>Start<\/button>/.exec(html)?.[0] ?? "";
      expect(start).not.toBe("");
      expect(start.includes("disabled")).toBe(!p.edited);
      expect(/<button[^>]*>Cancel<\/button>/.exec(html)?.[0]).not.toContain("disabled");
      expect(attr(/<input[^>]*dlg-input[^>]*>/.exec(html)![0], "value")).toBe(p.edited ? "~/work" : "~");
    } else {
      expect(html).not.toContain("dlg-title");
    }

    if (!p.open) {
      expect(html).not.toContain('role="option"');
      return;
    }

    // The field says the mode, holds what was typed, and says where Start lands.
    const input = /<input[^>]*pal-field[^>]*>/.exec(html)![0];
    expect(attr(input, "placeholder")).toBe({ all: "Search sessions or run a command…", switch: "Switch to a session…", new: "Start in a folder, or type a first message…" }[p.mode]);
    expect(attr(input, "value")).toBe(QUERY[p.query]);
    expect(html.includes('class="pal-mode pal-aim"')).toBe(p.aimed);
    expect(attr(input, "aria-describedby")).toBe(p.aimed ? "pal-aim" : undefined);

    // The rows, in order, are the spec's; a node with S's replacement
    // open (route "new") also lists that session's own commands.
    const opts = [...html.matchAll(/<button id="pal-([^"]*)" role="option" aria-selected="(true|false)"/g)];
    // "this" is S's two commands, one kind.
    const kinds = opts.map((m) => kindOf(m[1])).filter((k, i, all) => !(k === "this" && all[i - 1] === "this"));
    const want = p.route === "new" && p.query === "empty" && p.mode === "all" ? [...hits(p), "this"] : hits(p);
    expect(kinds).toEqual(want);
    expect(attr(input, "aria-expanded")).toBe(String(want.length > 0));

    // Enter's row: exactly one selected, the one the combobox points at.
    const on = opts.filter((m) => m[2] === "true");
    if (sel(p)) {
      expect(on.length).toBe(1);
      expect(kindOf(on[0][1])).toBe(sel(p));
      expect(attr(input, "aria-activedescendant")).toBe("pal-" + on[0][1]);
    } else {
      expect(on.length).toBe(0);
      expect(attr(input, "aria-activedescendant")).toBeUndefined();
    }

    // PathNotAPrompt: a typed path's default row is never Start.
    if (["path", "full", "nopath"].includes(p.query) && p.at !== "start") expect(sel(p)).not.toBe("start");

    // EnterSaysWhat, for a node with a row to take, and Tab on a folder row.
    const foot = /<div class="pal-foot">.*?<\/div>/.exec(html)![0];
    if (sel(p)) expect(foot).toContain(`<kbd>↵</kbd> ${enter(p)}</span>`);
    if (sel(p) === "dir") expect(foot).toContain("<kbd>Tab</kbd> Complete");
    if (!["dir", "missing"].includes(sel(p))) expect(foot).not.toContain("<kbd>Tab</kbd>");

    // StatusVisible: rows, Searching…, the failure, No results, or the tip.
    // Said inside a live region, not only somewhere on the page.
    const said = (text: string) => new RegExp(`role="status"[^>]*>(?:(?!</p>|</div>).)*?${text}`).test(html);
    expect(html.includes("Searching…")).toBe(p.search === "loading");
    if (p.search === "loading") expect(said("Searching…")).toBe(true);
    expect(html.includes("Text search failed")).toBe(p.search === "error");
    if (p.search === "error") expect(said("Text search failed")).toBe(true);
    if (p.search === "error") expect(html).toMatch(/<button[^>]*>Retry<\/button>/);
    expect(html.includes(">Mentioned in<")).toBe(p.search === "done");
    expect(html.includes('class="pal-tip"')).toBe(p.query === "empty" && p.mode !== "new");
    const says = opts.length > 0 || said("Searching…") || said("Text search failed") || said("No results for") || html.includes('class="pal-tip"');
    expect(says).toBe(true);
  });
}
