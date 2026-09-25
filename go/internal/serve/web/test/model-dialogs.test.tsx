import { describe, expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import type { DialogReq } from "../src/dialog";
import type { Command } from "../src/palette";
import type { Project, Row } from "../src/types";
// No DOM under bun: the palette's portal renders in place.
const real = await import("react-dom");
mock.module("react-dom", () => ({ ...real, createPortal: (node: unknown) => node }));
(globalThis as { document?: unknown }).document ??= { body: {} };
const { Thread, ViewNav, archiveAsk } = await import("../src/app");
const { DialogView, openFocus } = await import("../src/dialog");
const { PaletteView } = await import("../src/palette");
const { ProjectsView } = await import("../src/projects");
const { StartingStatus } = await import("../src/loading");

// go/tests/model/specs/ui_dialogs.fizz at the component level: every
// settled state of the checked-in graph is rendered — the page behind
// (the session's Thread or the Projects page), the one modal up, the
// "Starting a session…" status — from props built from that node, and
// the node's invariants are asserted on the markup. The browser walk
// (tests/web/specs/model/ui_dialogs.spec.ts) drives the same graph
// through a real serve and reads focus off the DOM; static markup has no
// focus, so here the focus words are checked as controls: the one the
// node names exists where the node says and can take focus, the one a
// dialog opens on is the host's own choice (openFocus), and every Tab
// link lands on the next control the markup offers.

type Node = {
  route: "session" | "projects"; dialog: "none" | "archive" | "keys" | "delete" | "new";
  focus: "body" | "page" | "settings" | "field" | "cancel" | "ok"; opener: string;
  typed: "empty" | "wrong" | "slug"; failed: boolean; archived: boolean; project: boolean; creating: boolean;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_dialogs", import.meta.url).pathname);
const byIndex = new Map<number, Node>();
for (const { index, name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Page#0.")) n[k.slice(7)] = v;
  byIndex.set(index, n as Node);
}
const key = (n: Node) => JSON.stringify(n, Object.keys(n).sort());
const nodes = new Map<string, Node>();
for (const n of byIndex.values()) nodes.set(key(n), n);

// ---- the fixture behind the abstract values ----

const SLUG = "alpha";
const PROJECT: Project = { slug: SLUG, name: "Alpha" } as Project;
const TYPED = { empty: "", wrong: "not-it", slug: SLUG } as const;
const REFUSED = `type ${SLUG} to confirm`;
const noop = () => {};

const row = (n: Node): Row => ({ id: "s-000000aaaaaa", title: "Fix the flaky login test", cwd: "/home/u/work", status: "done",
  live: false, archived: n.archived, entries: 4 } as unknown as Row);

// The request the node has up, as its call site asks it: archive from
// app.tsx's own question, delete as projects.tsx's Delete… builds it.
function reqOf(n: Node): DialogReq | null {
  switch (n.dialog) {
    case "archive": {
      const [title, body, opts] = archiveAsk(row(n));
      return { kind: "confirm", title, body, action: opts.action, danger: false, safe: opts.safe, resolve: noop };
    }
    case "keys":
      return { kind: "keys", title: "Keyboard shortcuts", mod: "⌘", resolve: noop };
    case "delete":
      return { kind: "text", title: `Delete “${PROJECT.name}”?`, body: `Type ${SLUG} to confirm.`, initial: "", placeholder: SLUG,
               action: "Delete project", allowEmpty: false, danger: true, onSubmit: async () => {}, resolve: noop };
    default:
      return null;
  }
}

const threadProps = { onSend: async () => null, onAnswer: async () => null, onInterrupt: noop, onArchive: noop, onRename: async () => {},
  onModel: noop, onEffort: noop, onAssign: noop, onBack: noop, onContext: noop, onAck: noop, projects: [], busy: false, jump: null };

function page(n: Node): string {
  return renderToStaticMarkup(n.route === "session"
    ? <Thread row={row(n)} lines={[]} sending={[]} {...threadProps} />
    : <ProjectsView projects={n.project ? [PROJECT] : []} rows={[]} onOpen={noop} onAssign={noop} onAssignMany={async () => []}
                    onCreate={async () => ({ slug: SLUG })} onRename={async () => {}} onDelete={noop} />);
}

const commands: Command[] = [
  { id: "new:here", label: "New session in home", group: "Start", run: noop, suggest: true },
  { id: "new:folder", label: "New session in a folder…", group: "Start", run: noop },
];

function modal(n: Node): string {
  if (n.dialog === "new") {
    return renderToStaticMarkup(
      <PaletteView open onClose={noop} rows={[]} commands={commands} mode="new" visited={[]} onOpenSession={noop} onOpenWikiPage={noop}
        onStart={noop} onStartIn={noop} q="" setQ={noop} atId={null} setAtId={noop} search={{ hits: [], state: "idle", retry: noop }}
        pages={[]} places={{ folder: null, dirs: [], home: "" }} field={{ current: null }} opener={{ current: null }} />);
  }
  const req = reqOf(n);
  if (!req) return "";
  return renderToStaticMarkup(<DialogView req={req} text={TYPED[n.typed]} saving={false} failed={n.failed ? REFUSED : ""}
                                          onText={noop} onSubmit={noop} onDismiss={noop} />);
}

const nav = (n: Node) => renderToStaticMarkup(<ViewNav phone view={n.route === "session" ? "sessions" : "projects"} onView={noop} />);
const status = (n: Node) => (n.creating ? renderToStaticMarkup(<StartingStatus />) : "");

// ---- reading controls off the markup ----

const count = (html: string, re: RegExp) => (html.match(new RegExp(re.source, "g")) ?? []).length;

/** A modal's controls Tab can reach, in document order, named in the spec's focus words. */
function tabbable(html: string): string[] {
  const out: string[] = [];
  for (const m of html.matchAll(/<(input|button)\b[^>]*>(?:([^<]*)<\/button>)?/g)) {
    if (/\sdisabled=""/.test(m[0]) || /tabindex="-1"/.test(m[0])) continue;
    if (m[1] === "input") { out.push("field"); continue; }
    const text = (m[2] ?? "").trim();
    out.push(text === "Cancel" || text === "Close" ? "cancel" : "ok");
  }
  return out;
}

/** The control a focus word names inside the modal, or "" when it has none. */
function control(html: string, focus: string): string {
  if (focus === "field") return /<input\b[^>]*>/.exec(html)?.[0] ?? "";
  if (focus === "cancel") return /<button\b[^>]*>(Cancel|Close)<\/button>/.exec(html)?.[0] ?? "";
  if (focus === "ok") return /<button\b[^>]*class="btn (btn-primary|btn-danger)"[^>]*>[^<]*<\/button>/.exec(html)?.[0] ?? "";
  return "";
}

const SETTINGS = /<button class="more" aria-label="Session settings"[^>]*>/;

/** Where a focus word outside any modal lands on the page behind, or null when it names nothing (body). */
function behind(n: Node, focus: string, pageHtml: string, navHtml: string): string | null {
  if (focus === "settings") return SETTINGS.exec(pageHtml)?.[0] ?? "";
  if (focus === "page") return /<a[^>]*href="[^"]*"[^>]*>/.exec(navHtml)?.[0] ?? "";
  return null;
}

// ---- every node ----

function check(n: Node) {
  const p = page(n), m = modal(n), v = nav(n), s = status(n);
  const all = p + m + v + s;

  // The nav claims the page the node is on, and only that one.
  const on = /<a[^>]*aria-current="page"[^>]*>/.exec(v)?.[0] ?? "";
  expect(on).toContain(n.route === "session" ? 'href="#/"' : 'href="#/projects"');
  expect(count(v, /aria-current="page"/)).toBe(1);

  // The page behind is the route's.
  if (n.route === "session") {
    expect(p).toMatch(SETTINGS);
    // The archived session's note and its Unarchive, exactly while archived.
    const note = /<p class="archived-note composer-note" role="status">.*?<\/p>/.exec(p)?.[0] ?? "";
    expect(note !== "").toBe(n.archived);
    if (n.archived) expect(note).toMatch(/<button class="btn">Unarchive<\/button>/);
  } else {
    expect(p).toContain("Projects");
    // The project lists while it exists, with its … menu; the menu is
    // closed, so Delete… (the delete dialog's opener) is not in the page.
    expect(p.includes(`href="#/projects/${SLUG}"`)).toBe(n.project);
    expect(p.includes(`aria-label="More actions for ${PROJECT.name}"`)).toBe(n.project);
    expect(p).not.toContain('role="menuitem"');
  }

  // Exactly the one modal the node has up.
  expect(count(all, /aria-modal="true"/)).toBe(n.dialog === "none" ? 0 : 1);
  if (n.dialog === "new") expect(m).toContain('aria-label="Quick access"');
  else expect(all).not.toContain('aria-label="Quick access"');
  const title = /<h2 id="dlg-title" class="dlg-title">([^<]*)<\/h2>/.exec(m)?.[1] ?? "";
  expect(title).toBe({ none: "", new: "", archive: "Archive this session?", keys: "Keyboard shortcuts", delete: `Delete “${PROJECT.name}”?` }[n.dialog]);

  // FocusInsideDialog: the focused control is the modal's own and can take focus.
  if (n.dialog === "none") {
    expect(["field", "cancel", "ok"]).not.toContain(n.focus);
    const el = behind(n, n.focus, p, v);
    if (el !== null) expect(el).not.toBe("");
    // Settings is the session page's.
    if (n.focus === "settings") expect(n.route).toBe("session");
  } else {
    const el = control(m, n.focus);
    expect(el).not.toBe("");
    expect(el).not.toMatch(/\sdisabled=""/);
    if (n.dialog !== "new") expect(tabbable(m)).toContain(n.focus);
  }

  // OpenerWhileOpen, and the opener is somewhere focus can go back to:
  // Settings is on the page behind; Delete…'s menu item is gone ("body").
  expect(n.dialog === "none").toBe(n.opener === "none");
  if (n.opener === "settings") expect(p).toMatch(SETTINGS);
  if (n.dialog === "delete") expect(n.opener).toBe("body");

  // NoFocusOnDisabledDelete: Delete project is disabled exactly while the field is empty.
  if (n.dialog === "delete") {
    expect(/<input[^>]*value="([^"]*)"/.exec(m)?.[1]).toBe(TYPED[n.typed]);
    expect(control(m, "ok")).toContain("Delete project");
    expect(/\sdisabled=""/.test(control(m, "ok"))).toBe(n.typed === "empty");
    expect(tabbable(m)).toEqual(n.typed === "empty" ? ["field", "cancel"] : ["field", "cancel", "ok"]);
  }
  // The safe archive confirm: Cancel first, then Archive; the sheet has only Close.
  if (n.dialog === "archive") expect(tabbable(m)).toEqual(["cancel", "ok"]);
  if (n.dialog === "keys") expect(tabbable(m)).toEqual(["cancel"]);

  // FailedOnlyInDelete: "Not saved" is delete's refusal, an alert, and only then.
  expect(all.includes("Not saved:")).toBe(n.failed);
  if (n.failed) {
    expect(n.dialog).toBe("delete");
    expect(m).toContain(`<p id="dlg-err" class="dlg-err" role="alert">Not saved: ${REFUSED}</p>`);
    expect(control(m, "field")).toContain('aria-invalid="true"');
  }

  // DialogsMatchServer: only what can be done is offered.
  if (n.dialog === "archive") { expect(n.route).toBe("session"); expect(p).not.toContain("archived-note"); }
  if (n.dialog === "delete") { expect(n.route).toBe("projects"); expect(n.project).toBe(true); }

  // CreatingHasNoPalette: a create in flight says so, in a live region, and no second palette asks.
  expect(all.includes("Starting a session…")).toBe(n.creating);
  if (n.creating) {
    expect(s).toMatch(/role="status"/);
    expect(n.dialog).not.toBe("new");
  }
}

describe("ui_dialogs.fizz, every node rendered", () => {
  test("the graph has every settled state", () => {
    expect(nodes.size).toBe(176);
  });
  for (const [k, n] of nodes) test(k, () => check(n));
});

// ---- links the markup can answer ----

const links = graph.links.filter((l) => byIndex.has(l.src) && byIndex.has(l.dest));

describe("ui_dialogs.fizz, links", () => {
  // ArchiveOpensOnCancel, and each DialogHost dialog opening where the
  // spec says: the host's own first focus for the request the node has up.
  const HOST = { input: "field", cancel: "cancel", ok: "ok" } as const;
  const opens = links.filter((l) => byIndex.get(l.src)!.dialog === "none" && ["archive", "keys", "delete"].includes(byIndex.get(l.dest)!.dialog));
  test("opening links exist", () => expect(opens.length).toBeGreaterThan(0));
  for (const l of opens) {
    const d = byIndex.get(l.dest)!;
    test(`${l.name}: ${l.src} -> ${l.dest} opens on ${d.focus}`, () => {
      expect(HOST[openFocus(reqOf(d)!)]).toBe(d.focus);
    });
  }

  // useModal's trap: Tab goes to the next control the markup offers, and wraps.
  const tabs = links.filter((l) => l.name === "Page#0.Tab");
  test("Tab links exist", () => expect(tabs.length).toBe(36));
  for (const l of tabs) {
    const a = byIndex.get(l.src)!, b = byIndex.get(l.dest)!;
    test(`Tab: ${l.src} -> ${l.dest} (${a.dialog}, ${a.focus} -> ${b.focus})`, () => {
      const cycle = tabbable(modal(a));
      const i = cycle.indexOf(a.focus);
      expect(i).toBeGreaterThanOrEqual(0);
      expect(cycle[(i + 1) % cycle.length]).toBe(b.focus);
    });
  }

  // FocusRestored: a closing DialogHost dialog gives focus to its opener,
  // which the page it closes onto still has.
  const closes = links.filter((l) => ["archive", "keys", "delete"].includes(byIndex.get(l.src)!.dialog) && byIndex.get(l.dest)!.dialog === "none");
  test("closing links exist", () => expect(closes.length).toBeGreaterThan(0));
  for (const l of closes) {
    const a = byIndex.get(l.src)!, b = byIndex.get(l.dest)!;
    test(`${l.name}: ${l.src} -> ${l.dest} gives focus back to ${a.opener}`, () => {
      expect(b.focus).toBe(a.opener as Node["focus"]);
      const el = behind(b, b.focus, page(b), nav(b));
      if (el !== null) expect(el).not.toBe("");
    });
  }
});
