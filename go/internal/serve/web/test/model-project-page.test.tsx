import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { DialogView } from "../src/dialog";
import { humanError } from "../src/loading";
import { stopOrbQuestion } from "../src/orb";
import { ProjectPage, type PageInitial } from "../src/project";
import type { OrbFile, ProjectDetail, Row } from "../src/types";

// go/tests/model/specs/ui_project-page.fizz at the component level: every
// settled state of the checked-in graph is rendered from props built from
// that node, and the node's meaning — which column, drawer, rail, panel
// and dialog are up, what the composer and the MEMORY.md editor say, the
// orb strip's words — is asserted on the markup. The browser walk
// (tests/web/specs/model/ui_project-page.spec.ts) drives the same graph
// through a real serve at seconds a node; this names a broken render in
// milliseconds and needs no Chromium.
//
// The page's own UI state (the viewport class, the panel, the drawer, the
// rail, the Done fold, the composer's and the editor's) lives in useState;
// ProjectPage's `initial` is what it starts from, and a static render is
// exactly that start.

type Node = {
  detail: string; files: string; main: boolean; view: string; thread: string; done_folded: boolean;
  orb: string; composer: string; wide: boolean; pref: string; panel: boolean; drawer: boolean;
  rail: boolean; mem: string; dialog: boolean;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_project-page", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Page#0.")) n[k.slice(7)] = v;
  nodes.set(JSON.stringify(n, Object.keys(n).sort()), n as Node);
}

const SLUG = "alpha";
const MAIN = "main-1";
const THREAD = "t1";
const THREAD_TITLE = "Port the PTY suite";
const MEMORY = "Use the PTY suite.\n";
const DRAFT = MEMORY + "Edit 1\n";
const TYPED = "Start the port";
const SEND_FAIL = "main could not start";
const SAVE_FAIL = "MEMORY.md: write failed";
const LOAD_FAIL = "serve: the project could not be read";
const FILES_FAIL = "serve: the definition files could not be read";
const at = new Date(Date.UTC(2026, 8, 24, 12)).toISOString();

function row(id: string, title: string, over: Partial<Row> = {}): Row {
  return { id, title, cwd: "/w", status: "idle", live: false, archived: false, entries: 3, modified: at, lastAt: at,
           mode: "project", project: SLUG, ...over };
}

const THREAD_ROW: Record<string, Partial<Row> | null> = {
  none: null,
  running: { status: "running", live: true },
  unseen: { status: "done", unseen: true },
  done: { status: "done" },
};

function detailOf(n: Node): ProjectDetail {
  const t = THREAD_ROW[n.thread];
  if (t === undefined) throw new Error(`unknown thread state ${n.thread}`);
  const orb = n.orb === "none" ? undefined : {
    session: MAIN, project: SLUG, status: n.orb as "starting" | "running" | "stopped", up: n.orb === "running", updatedAt: at,
  };
  return {
    slug: SLUG, name: "Alpha", main: n.main ? MAIN : undefined, mainOrb: orb, orbs: orb ? [orb] : [],
    threads: t ? [row(THREAD, THREAD_TITLE, t)] : [],
  };
}

/** What the page's own state is at the node: ProjectPage's, the composer's and the editor's. */
function initialOf(n: Node): PageInitial {
  const dirty = n.mem === "dirty" || n.mem === "saving" || n.mem === "failed";
  return {
    tight: !n.wide,
    panel: n.panel,
    drawer: n.drawer,
    threadsFolded: n.rail,
    folded: n.done_folded ? ["done", "idle", "empty"] : ["idle", "empty"],
    composer: {
      text: n.composer === "empty" ? "" : TYPED,
      busy: n.composer === "sending",
      err: n.composer === "failed" ? SEND_FAIL : "",
    },
    editor: {
      drafts: dirty ? { "MEMORY.md": DRAFT } : {},
      saving: n.mem === "saving",
      saveErr: n.mem === "failed" ? SAVE_FAIL : "",
      said: n.mem === "saved" ? "MEMORY.md saved." : "",
    },
  };
}

const noop = () => {};

function render(n: Node): string {
  const loaded = n.detail === "loaded";
  const open = n.view === "home" ? "" : n.view === "main" ? MAIN : THREAD;
  const files: Partial<Record<OrbFile, string>> | undefined = n.files === "loaded" ? { "MEMORY.md": MEMORY } : undefined;
  const q = stopOrbQuestion([{ id: 1, cmd: "make test", started: at } as never])!;
  return renderToStaticMarkup(
    <>
      <ProjectPage detail={loaded ? detailOf(n) : undefined} files={files} error={n.detail === "error" ? LOAD_FAIL : ""}
                   filesError={n.files === "error" ? FILES_FAIL : ""} open={open}
                   mainRow={n.main ? row(MAIN, "Main thread") : undefined}
                   conversation={open ? <div className="thread">conversation</div> : undefined}
                   onOpen={noop} onBack={noop} onStopOrb={noop} onRetry={noop} onSeen={noop}
                   onSave={async () => {}} onMessage={async () => {}} initial={initialOf(n)} />
      {n.dialog && <DialogView req={{ kind: "confirm", title: q.title, body: q.body, action: "Stop orb", danger: true, safe: false, resolve: noop }}
                               text="" saving={false} failed="" onText={noop} onSubmit={noop} onDismiss={noop} />}
    </>,
  );
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
const button = (html: string, text: string) => new RegExp(`<button[^>]*>${text}</button>`).exec(html)?.[0] ?? "";
const textOf = (html: string) => html.replace(/<[^>]*>/g, "").trim();

// The spec's own projections, for the assertions that are about the node rather than the markup.
const panelWanted = (pref: string, view: string) => (pref !== "" ? pref === "1" : view === "home");
const columnShown = (n: Node) => n.view !== "home" && (n.wide ? !n.rail : n.drawer);
const GROUP: Record<string, string> = { running: "Running", unseen: "Finished — unseen", done: "Done" };

/** The spec's invariants, on the node itself: the markup checks below lean on them. */
function checkNode(n: Node) {
  if (n.detail !== "loaded") {
    // NothingBeforeDetail.
    expect([n.view, n.drawer, n.mem, n.dialog, n.composer]).toEqual(["home", false, "clean", false, "empty"]);
  }
  expect(n.drawer && n.panel).toBe(false); // OneDrawer
  if (n.drawer) expect([n.wide, n.view === "home"]).toEqual([false, false]); // DrawerOnlyWhereItIsOne
  if (n.view === "main") expect(n.main).toBe(true); // OpenHasSomething
  if (n.view === "thread") expect(n.thread).not.toBe("none");
  if (n.view === "thread") expect(n.thread).not.toBe("unseen"); // UnseenNeverOnScreen
  if (n.composer !== "empty") expect(n.view).toBe("home"); // ComposerOnHome
  if (n.mem !== "clean") expect([n.detail, n.panel, n.files]).toEqual(["loaded", true, "loaded"]); // EditorNeedsPanel
  if (n.wide) expect(n.panel).toBe(panelWanted(n.pref, n.view)); // PanelFollowsChoice
  if (n.orb !== "none") expect(n.main).toBe(true); // StripKnowsMain
}

function checkBefore(n: Node, html: string) {
  // StatusVisible / NothingBeforeDetail: the status line and nothing else.
  expect(html).not.toContain('class="prj"');
  expect(html).not.toContain("<textarea");
  expect(html).not.toContain("<aside");
  expect(count(html, /role="dialog"/)).toBe(0);
  if (n.detail === "loading") {
    expect(html).toMatch(/role="status"[^>]*aria-busy="true"/);
    expect(html).toContain("Loading project…");
    expect(html).not.toContain("error-note");
  } else {
    const note = element(html, /<div class="error-note[^"]*" role="alert">/);
    expect(note).toContain("Couldn’t read this project");
    expect(note).toContain(humanError(LOAD_FAIL));
    expect(textOf(element(note, /<div class="state-actions">/))).toBe("Retry");
  }
}

function checkBar(n: Node, html: string) {
  expect(html).toContain('<h1 class="prj-name" title="Alpha">Alpha</h1>');
  // StatusVisible: the counts on the home, the crumb beside a conversation.
  const crumb = textOf(element(html, /<p class="prj-crumb">/));
  if (n.view === "home") {
    const threads = n.thread === "none" ? 0 : 1;
    expect(crumb).toContain(`${SLUG}· `);
    expect(crumb).toEndWith(`${threads} ${threads === 1 ? "thread" : "threads"}`);
    expect(html).not.toContain("prj-back");
  } else {
    // OpenHasSomething: the crumb names what is open, never the placeholder.
    expect(crumb).toBe(`‹ All threads· ${n.view === "main" ? "Main thread" : THREAD_TITLE}`);
  }
  // The Threads button is drawn beside a conversation (CSS shows it only at tight) and says the drawer.
  const threadsBtn = /<button[^>]*class="btn btn-sm prj-threads-btn"[^>]*>/.exec(html)?.[0];
  if (n.view === "home") expect(threadsBtn).toBeUndefined();
  else expect(threadsBtn).toContain(`aria-expanded="${n.drawer}"`);
  expect(/<button[^>]*class="btn btn-sm prj-panel-btn"[^>]*>/.exec(html)![0]).toContain(`aria-expanded="${n.panel}"`);
  // One scrim behind whichever drawer is up (CSS draws it only at tight).
  expect(html.includes('class="prj-scrim"')).toBe(n.panel || n.drawer);
}

function checkThreads(n: Node, html: string) {
  const railDrawn = n.view !== "home" && n.wide && n.rail;
  const colDrawn = n.view !== "home" && !railDrawn;
  expect(count(html, /<aside class="prj-threads prj-threads-folded"/)).toBe(railDrawn ? 1 : 0);
  const colTag = /<aside class="prj-threads" aria-label="Threads"[^>]*>/.exec(html)?.[0];
  expect(Boolean(colTag)).toBe(colDrawn);
  // The drawer is the column wearing data-open; OneDrawer on the markup.
  expect(Boolean(colTag?.includes("data-open"))).toBe(n.drawer);
  expect(html.includes("data-open") && html.includes('class="prj-panel"')).toBe(false);
  // A column on screen: at wide unless folded, at tight only while its drawer is up.
  expect(colDrawn && (n.wide || n.drawer)).toBe(columnShown(n));

  // The list the thread's row can be in: the home's queue, or the column.
  const list = n.view === "home" ? element(html, /<div class="prj-queue">/)
    : colDrawn ? element(html, /<div class="scroll prj-threads-list">/) : "";
  if (n.view === "home") expect(list).not.toBe("");
  // The pinned main: its row on the home, the Main thread button in the column.
  if (list) expect(list.includes(n.view === "home" ? "prj-main-row" : 'class="prj-main-thread')).toBe(n.main);
  const rows = count(list, /<button[^>]*class="prj-thread(?! prj-main-row)[^"]*"/);
  const labels = [...list.matchAll(/<span class="prj-group-label">([^<]*)<\/span>/g)].map((m) => m[1]);
  if (!list) return;
  if (n.thread === "none") {
    expect(labels).toEqual([]);
    expect(rows).toBe(0);
    expect(list).toContain("No threads yet.");
    return;
  }
  expect(labels).toEqual([GROUP[n.thread]]);
  // The column's Done group folds; the home never folds a group of one row.
  const folded = n.view !== "home" && n.thread === "done" && n.done_folded;
  expect(rows).toBe(folded ? 0 : 1);
  if (n.view !== "home") {
    const head = /<button type="button" class="prj-group-head"[^>]*>/.exec(list)![0];
    // Only Done and the resting groups start folded; running and unseen never do.
    expect(head).toContain(`aria-expanded="${!(n.thread === "done" && n.done_folded)}"`);
  }
  if (rows) {
    const r = /<button[^>]*class="prj-thread(?! prj-main-row)[^"]*"[^>]*>/.exec(list)![0];
    // The open thread is the row marked current, and never says it is unseen.
    expect(r.includes('aria-current="true"')).toBe(n.view === "thread");
    expect(r.includes("not seen yet")).toBe(n.thread === "unseen");
    // Only an unseen finish drags (onto Done).
    expect(r.includes('draggable="true"')).toBe(n.thread === "unseen");
  }
}

function checkComposer(n: Node, html: string) {
  const first = element(html, /<div class="prj-first">/);
  // ComposerOnHome: the composer, its text and its error are the home's.
  if (n.view !== "home") {
    expect(first).toBe("");
    return;
  }
  const box = /<textarea[^>]*class="prj-first-box"[^>]*>([^<]*)<\/textarea>/.exec(first)!;
  const send = /<button[^>]*class="btn btn-primary"[^>]*>([^<]*)<\/button>/.exec(first)!;
  const err = /<p class="err prj-first-err" role="alert">([^<]*)<\/p>/.exec(first);
  expect(box[1]).toBe(n.composer === "empty" ? "" : TYPED);
  expect(box[0].includes("disabled")).toBe(n.composer === "sending");
  expect(send[1]).toBe(n.composer === "sending" ? "Sending…" : "Send");
  expect(send[0].includes("disabled")).toBe(n.composer === "empty" || n.composer === "sending");
  expect(err?.[1]).toBe(n.composer === "failed" ? humanError(SEND_FAIL) : undefined);
  expect(first.includes("No main thread yet.")).toBe(!n.main);
}

function checkPanel(n: Node, html: string) {
  const panel = element(html, /<aside class="prj-panel" aria-label="Project">/);
  expect(panel !== "").toBe(n.panel);
  if (!panel) return;

  // The strip: the spec's strip(); a starting orb is called stopped (KNOWN GAPS 1).
  const strip = element(panel, /<p class="prj-orb-line[^"]*">/);
  const words = textOf(strip);
  switch (n.orb) {
    case "none":
      expect(words).toStartWith(n.main ? "Main thread has no orb yet." : "No orb yet.");
      break;
    case "running":
      expect(words).toStartWith("Main thread’s orb —");
      break;
    case "starting":
    case "stopped":
      expect(words).toStartWith("Main thread’s orb is stopped.");
      break;
    default:
      throw new Error(`unknown orb state ${n.orb}`);
  }
  // Stop only while the orb is up.
  expect(button(strip, "Stop") !== "").toBe(n.orb === "running");

  const files = element(panel, /<details class="prj-files"[^>]*>/);
  const editor = /<textarea[^>]*class="field mono orb-editor"[^>]*>([^<]*)<\/textarea>/.exec(files);
  switch (n.files) {
    case "loading":
      expect(files).toMatch(/role="status"[^>]*aria-busy="true"/);
      expect(files).toContain("Loading files…");
      expect(editor).toBeNull();
      break;
    case "error": {
      const e = element(files, /<div class="pending pending-inline pending-err" role="alert">/);
      expect(e).toContain(humanError(FILES_FAIL));
      expect(button(e, "Retry")).not.toBe("");
      expect(editor).toBeNull();
      break;
    }
    case "loaded":
      expect(editor).not.toBeNull();
      break;
    default:
      throw new Error(`unknown files state ${n.files}`);
  }
  if (!editor) return;

  // The MEMORY.md tab is the one open; a draft wears the dot.
  expect(editor[0]).toContain('aria-label="MEMORY.md"');
  const dirty = n.mem === "dirty" || n.mem === "saving" || n.mem === "failed";
  expect(editor[1]).toBe(dirty ? DRAFT : MEMORY);
  const tab = /<button role="tab" aria-selected="true"[^>]*>([^<]*)<\/button>/.exec(files)![1];
  expect(tab).toBe(dirty ? "MEMORY.md •" : "MEMORY.md");
  const save = [...files.matchAll(/<button class="btn"[^>]*>(Save|Saving…)<\/button>/g)];
  expect(save.length).toBe(1);
  expect(save[0][1]).toBe(n.mem === "saving" ? "Saving…" : "Save");
  // Save is there to press only with a draft and nothing in flight.
  expect(save[0][0].includes("disabled")).toBe(!(n.mem === "dirty" || n.mem === "failed"));
  const err = /<p class="err orb-save-err" role="alert">([^<]*)<\/p>/.exec(files);
  expect(err?.[1]).toBe(n.mem === "failed" ? SAVE_FAIL : undefined);
  const said = /<p class="file-said" role="status">([^<]*)<\/p>/.exec(files);
  expect(said?.[1]).toBe(n.mem === "saved" ? "MEMORY.md saved." : undefined);
}

function checkDialog(n: Node, html: string) {
  const dialogs = count(html, /role="dialog"|role="alertdialog"/);
  expect(dialogs).toBe(n.dialog ? 1 : 0);
  if (!n.dialog) return;
  // Asked from the strip's Stop, so the panel is up; the orb may have
  // exited behind the modal since (OrbExit), and the modal stays.
  expect(n.panel).toBe(true);
  expect(["running", "stopped"]).toContain(n.orb);
  expect(html).toContain("Stop the orb?");
  expect(html).toContain("make test");
  expect(html).toMatch(/<button[^>]*>Stop orb<\/button>/);
}

describe("ui_project-page.fizz, every node rendered", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBeGreaterThan(100);
  });

  for (const [key, n] of nodes) {
    test(key, () => {
      checkNode(n);
      const html = render(n);
      if (n.detail !== "loaded") {
        checkBefore(n, html);
        return;
      }
      checkBar(n, html);
      checkThreads(n, html);
      checkComposer(n, html);
      checkPanel(n, html);
      checkDialog(n, html);
    });
  }
});
