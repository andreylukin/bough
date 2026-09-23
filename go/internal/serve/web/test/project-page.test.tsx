import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectPage, countsLine, groupThreads, lineCount, threadCounts, threadGroup } from "../src/project";
import { rowNote } from "../src/status";
import type { OrbFile, OrbState, ProjectDetail, Row } from "../src/types";

const at = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();

function row(id: string, title: string, over: Partial<Row> = {}): Row {
  return {
    id, title, cwd: "/w/bough", status: "idle", live: false, archived: false, entries: 3,
    modified: at(2), lastAt: at(2), mode: "project", project: "bough", ...over,
  };
}

const threads: Row[] = [
  row("t1", "Port the PTY suite", { status: "running", live: true }),
  row("t2", "Migration order", { status: "needs-you", ask: { id: "a", text: "Staging first?", options: [], seq: 4 } }),
  row("t3", "SIGPIPE on closed stdout", { status: "error", lastAt: at(1) }),
  row("t4", "Rename the scratch dir", { status: "interrupted" }),
  row("t5", "Old spike", { status: "done" }),
  row("t6", "Second spike", { status: "stopped" }),
];

const detail: ProjectDetail = {
  slug: "bough", name: "Control room", main: "main-1",
  mainOrb: { session: "main-1", project: "bough", status: "running", up: true, updatedAt: at(2) },
  orbs: [{ session: "main-1", project: "bough", status: "running", up: true, updatedAt: at(2) }],
  threads,
};

const files: Partial<Record<OrbFile, string>> = { "MEMORY.md": "one\ntwo\nthree\n" };

const noop = () => {};
const page = (over: Partial<Parameters<typeof ProjectPage>[0]> = {}) =>
  renderToStaticMarkup(
    <ProjectPage detail={detail} files={files} open="" onOpen={noop} onBack={noop} onStopOrb={noop} onRetry={noop}
                 onSave={async () => {}} onMessage={async () => {}}
                 mainRow={row("main-1", "Main thread", { status: "running", live: true })}
                 conversation={<div className="thread">conversation</div>} {...over} />,
  );

// The column: beside a conversation, the way between threads.
const column = (over: Partial<Parameters<typeof ProjectPage>[0]> = {}) => page({ open: "t1", conversation: <div className="thread">conversation</div>, ...over });

test("the groups are in urgency order and an empty one is not rendered", () => {
  const html = column();
  const order = ["Needs you", "Error", "Running", "Interrupted", "Done", "Idle"].map((g) => html.indexOf(`prj-group-label">${g}<`));
  expect(order.every((i) => i >= 0)).toBe(true);
  expect(order).toEqual([...order].sort((a, b) => a - b));
  // Nothing is queued, so no group says so, and no group is rendered empty.
  expect(html).not.toContain("Queued");
  expect(html).not.toContain('prj-group-count">0<');
});

test("a status is the sidebar's row mark, never a pill, in the threads column; the conversation's header wears the pill", () => {
  const html = column();
  // The sidebar's 16px mark column, on every thread row; glyphs only for the states a list marks.
  expect(html.split('class="row-mark"').length - 1).toBeGreaterThan(2);
  expect(html).toContain('width="16"');
  expect(html).not.toContain("prj-dot");
  expect(html).not.toContain("prj-pill");
  // The word still reaches a screen reader, through the row's label.
  expect(html).toContain("Waiting for you");
});

test("the main thread is pinned with its state, above the groups, not as a peer row", () => {
  const html = column();
  expect(html).toContain("Main thread");
  expect(html.indexOf("prj-main-thread")).toBeLessThan(html.indexOf("prj-group-head"));
  // Its state, not the slug: the title bar already names the project.
  expect(html).toContain('prj-main-slug">Running<');
  expect(html).not.toContain(">bough<");
});

test("no threads: one line and no group headers at all", () => {
  const html = column({ detail: { ...detail, threads: [] } });
  expect(html).toContain("No threads yet. Ask the main thread to start work, or create one.");
  expect(html).not.toContain("prj-group-head");
  expect(html).toContain("prj-main-thread");
});

test("no main thread: the home says the first message starts one, and pins no main row", () => {
  const html = page({ detail: { ...detail, main: undefined, mainOrb: undefined, threads: [] }, conversation: undefined });
  expect(html).toContain("No main thread yet. The first message starts one.");
  expect(html).toContain("prj-first-box");
  expect(html).not.toContain("prj-main-row");
});

test("MEMORY.md is named by its filename and counted in lines, and nothing calls it memory", () => {
  const html = page();
  expect(html).toContain("MEMORY.md");
  expect(html).toContain("3 lines");
  expect(html).toContain("Prepended to every session in this project.");
  // The tab already names the file; the meta line is the count alone.
  expect(html).not.toContain('<span class="mono">MEMORY.md</span> ·');
  expect(html.toLowerCase()).not.toContain(">memory<");
  expect(html).not.toContain("Memories");
});

test("a long MEMORY.md says what it costs, in amber past 200 lines and red past 400", () => {
  const long = page({ files: { "MEMORY.md": Array.from({ length: 240 }, (_, i) => `l${i}`).join("\n") } });
  expect(long).toContain("240 lines");
  expect(long).toContain("long — injected every turn");
  expect(long).toContain("ctx-lines-long");
  expect(long).not.toContain("ctx-lines-max");
  const huge = page({ files: { "MEMORY.md": Array.from({ length: 401 }, (_, i) => `l${i}`).join("\n") } });
  expect(huge).toContain("ctx-lines-max");
});

test("the orb line describes the main thread's container and never claims to stop the threads", () => {
  expect(page()).toContain("Main thread’s orb");
  expect(page()).toContain('title="Each thread runs in a container of its own."');
  // A failed thread orb is said on the fold line, not hidden under a count.
  const failed = page({ detail: { ...detail, orbs: [...detail.orbs, { session: "t9", project: "bough", status: "failed", up: false } as OrbState] } });
  expect(failed).toContain("1 failed");
  const down = page({ detail: { ...detail, mainOrb: { ...detail.mainOrb!, status: "stopped", up: false } } });
  expect(down).toContain("Main thread’s orb is stopped. It starts when you message the project.");
  // A project that has been messaged has threads: the line is about main alone.
  const never = page({ detail: { ...detail, mainOrb: undefined } });
  expect(never).toContain("Main thread has no orb yet.");
  expect(never).not.toContain("No orb yet. It starts when you first message the project.");
  const fresh = page({ detail: { ...detail, mainOrb: undefined, main: "", threads: [] } });
  expect(fresh).toContain("No orb yet. It starts when you first message the project.");
});

test("a definition that does not parse keeps its page and points at the editor", () => {
  const html = page({ detail: { ...detail, error: "yaml: line 3: did not find expected key" } });
  expect(html).toContain("yaml: line 3: did not find expected key");
  expect(html).toContain("Edit project.yml");
});

test("an opened thread swaps the conversation and offers the way back to the project", () => {
  const html = page({ open: "t2", conversation: <div className="thread">thread</div> });
  expect(html).toContain("‹ All threads");
  expect(html).not.toContain("‹ Control room");
  expect(html).toContain("Migration order");
  expect(html).not.toContain("prj-home");
  // Main opened is a conversation like any other, named as main.
  const main = page({ open: "main-1", conversation: <div className="thread">main</div> });
  expect(main).toContain("‹ All threads");
  expect(main).toContain("· Main thread");
  expect(main).toContain('prj-main-thread is-on');
});

test("threadGroup reads the one status vocabulary", () => {
  expect(threadGroup(row("x", "x", { status: "needs-you" }))).toBe("needs-you");
  expect(threadGroup(row("x", "x", { status: "done", ask: { id: "a", text: "?", options: [], seq: 1 } }))).toBe("needs-you");
  expect(threadGroup(row("x", "x", { status: "done", testsFailed: true, lastAt: at(1) }))).toBe("error");
  expect(threadGroup(row("x", "x", { status: "queued" }))).toBe("running");
  expect(threadGroup(row("x", "x", { status: "interrupted" }))).toBe("interrupted");
  expect(threadGroup(row("x", "x", { status: "stopped" }))).toBe("idle");
  expect(threadGroup(row("x", "x", { status: "done" }))).toBe("done");
  expect(threadGroup(row("x", "x", { status: "idle", empty: true }))).toBe("empty");
});

test("groupThreads drops empty groups and keeps the urgency order", () => {
  const gs = groupThreads(threads);
  expect(gs.map((g) => g.group)).toEqual(["needs-you", "error", "running", "interrupted", "done", "idle"]);
  expect(gs[4].rows.length).toBe(1);
  expect(groupThreads([]).length).toBe(0);
  expect(groupThreads([row("z", "z")]).map((g) => g.group)).toEqual(["idle"]);
});

// A thread's second line is the sidebar's: its failure or its wait, and
// nothing for a plain running or done row. The page used to show the
// agent's summary there and the sidebar did not, so one thread read two ways.
test("a thread's second line is the sidebar's, and twins carry the sidebar's id tail", () => {
  expect(rowNote(row("x", "x", { trouble: "tests failed" })).label).toBe("Tests failed");
  expect(rowNote(threads[1]).plain).toBe(false);
  expect(rowNote(row("x", "x", { status: "done" })).label).toBe("");
  const html = page({ detail: { ...detail, threads: [
    row("01a0d068-126d-7000-8000-00000038b900", "echo: Running log", { status: "done" }),
    row("01a0d068-2277-7000-8000-0000003c1297", "echo: Running log", { status: "done" }),
    row("t3", "SIGPIPE on closed stdout", { status: "error", lastAt: at(1) }),
    row("t7", "Orb thread", { status: "done", orb: { session: "t7", project: "bough", status: "running", up: true, updatedAt: at(2) } }),
  ] } });
  // The tail, not the UUIDv7 time prefix every sibling shares.
  expect(html).toContain(">38b900<");
  expect(html).toContain(">3c1297<");
  expect(html).not.toContain(">01a0d0");
  expect(html).toContain(">Failed<");
  expect(html).toContain("Orb running");
});

test("lines are counted the way an editor counts them", () => {
  expect(lineCount("")).toBe(0);
  expect(lineCount("one")).toBe(1);
  expect(lineCount("one\n")).toBe(1);
  expect(lineCount("one\ntwo")).toBe(2);
  expect(lineCount("one\ntwo\n")).toBe(2);
  expect(lineCount("\n")).toBe(1);
});

test("an absent MEMORY.md says what to write in the pane, not in a dialog", () => {
  const html = page({ files: {} });
  expect(html).toContain("Write what every session in this project should know: what it is, where things live, decisions already made.");
  expect(html).toContain("0 lines");
  expect(html).not.toContain("role=\"dialog\"");
});

test("a Files read that failed says so and offers the retry, instead of spinning forever", () => {
  const html = page({ files: undefined, filesError: "orb: read MEMORY.md: connection reset" });
  expect(html).toContain("Couldn’t load files.");
  expect(html).toContain("Retry");
  expect(html).not.toContain("Loading files…");
});

test("Files still spins while it is only loading", () => {
  const html = page({ files: undefined });
  expect(html).toContain("Loading files…");
  expect(html).not.toContain("Retry");
});

// Under 1080px the panel, and under 860px the thread list, become
// drawers over the conversation. Hidden outright they took MEMORY.md
// and every thread with them, while the title bar's toggle stayed live
// and inert.
test("both side columns keep a way back at narrow widths", () => {
  // Beside a conversation the panel opens closed unless it was left open: remember it open, so its drawer is drawn.
  const g = globalThis as { localStorage?: unknown };
  const prev = g.localStorage;
  g.localStorage = { getItem: (k: string) => (k === "bough:prj-panel" ? "1" : null), setItem: () => {} };
  let html = "";
  try { html = column(); } finally { g.localStorage = prev; }
  // The toggle for the thread list, the scrim that dismisses a drawer,
  // and a Close inside each one. Which of them is visible is the
  // stylesheet's business.
  expect(html).toContain("prj-threads-btn");
  expect(html).toContain("prj-panel-btn");
  expect(html).toContain("prj-scrim");
  expect(html.split("prj-drawer-close").length - 1).toBe(2);
  // As a drawer the panel names itself: a lone Close over the Orbs caption said nothing.
  expect(html).toMatch(/<div class="prj-drawer-head"><h2>Project<\/h2><button[^>]*prj-drawer-close/);
});

// The home indexes the threads; it is not one of them. The composer is
// first, main is pinned as the thread it talks to, and the rest sit under
// their state. No conversation is rendered on the home, and no column.
test("the home is the composer, then main, then the queue; no transcript and no second column", () => {
  const html = page();
  expect(html).toContain("prj-home");
  expect(html).not.toContain(">conversation<");
  expect(html).not.toContain("prj-threads-list");
  expect(html.indexOf("prj-first-box")).toBeLessThan(html.indexOf("prj-main-row"));
  expect(html.indexOf("prj-main-row")).toBeLessThan(html.indexOf("prj-group-head"));
  // Main by its own name, as the sidebar lists it, tagged as the main thread.
  expect(html).toMatch(/prj-main-row[\s\S]*?Main thread[\s\S]*?· Main thread/);
  // The header line reads the queue's own numbers.
  expect(html).toContain("1 needs you · 1 error · 1 running · 6 threads");
  // Done and idle are on the page up to a cap; nothing is folded away for six threads.
  expect(html).toContain("Old spike");
  expect(html).toContain("Second spike");
  expect(html).not.toContain("Archived. A message reopens it.");
  expect(page({ detail: { ...detail, mainArchived: true } })).toContain("Archived. A message reopens it.");
});

test("done threads past eight fold behind one line, empty ones fold entirely, the rest never do", () => {
  const many = [...threads, ...Array.from({ length: 12 }, (_, i) => row(`i${i}`, `done ${i}`, { status: "done" })),
                row("e1", "Untitled", { empty: true }), row("e2", "Untitled", { empty: true })];
  const html = page({ detail: { ...detail, threads: many } });
  expect(html).toContain("Show all 13");
  expect(html).toContain("Show all 2");
  // 4 live + 8 of 13 done + 1 idle + 0 of 2 empty
  expect(html.split('class="prj-thread"').length - 1).toBe(4 + 8 + 1);
  expect(html.indexOf('data-group="idle"')).toBeLessThan(html.indexOf('data-group="empty"'));
});

test("threadCounts and countsLine read the same vocabulary as the column", () => {
  expect(threadCounts(threads)).toEqual({ "needs-you": 1, error: 1, running: 1, interrupted: 1, unseen: 0, done: 1, idle: 1, empty: 0 });
  expect(countsLine([])).toBe("0 threads");
  expect(countsLine([row("a", "a", { status: "error" }), row("b", "b")])).toBe("1 error · 2 threads");
});
