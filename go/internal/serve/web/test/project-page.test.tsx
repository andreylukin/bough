import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectPage, groupThreads, homeThreads, latestThread, lineCount, threadCounts, threadGroup, threadNote } from "../src/project";
import type { OrbFile, ProjectDetail, Row } from "../src/types";

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

test("the groups are in urgency order and an empty one is not rendered", () => {
  const html = page();
  const order = ["Needs you", "Error", "Running", "Interrupted", "Idle"].map((g) => html.indexOf(`prj-group-label">${g}<`));
  expect(order.every((i) => i >= 0)).toBe(true);
  expect(order).toEqual([...order].sort((a, b) => a - b));
  // Nothing is queued, so no group says so, and no group is rendered empty.
  expect(html).not.toContain("Queued");
  expect(html).not.toContain('prj-group-count">0<');
});

test("a status is a dot, never a pill, in the threads column; the one pill is the title bar's", () => {
  const html = page();
  expect(html.split("prj-dot").length - 1).toBeGreaterThan(2);
  expect(html.split("prj-pill").length - 1).toBe(1);
  // The word still reaches a screen reader, through the row's label.
  expect(html).toContain("Waiting for you");
});

test("the main thread is pinned with its slug, above the groups, not as a peer row", () => {
  const html = page();
  expect(html).toContain("Main thread");
  expect(html.indexOf("prj-main-thread")).toBeLessThan(html.indexOf("prj-group-head"));
  expect(html).toContain(">bough<");
});

test("no threads: one line and no group headers at all", () => {
  const html = page({ detail: { ...detail, threads: [] } });
  expect(html).toContain("No threads yet. Ask the main thread to start work, or create one.");
  expect(html).not.toContain("prj-group-head");
  expect(html).toContain("prj-main-thread");
});

test("no main thread: the page is one sentence and a composer", () => {
  const html = page({ detail: { ...detail, main: undefined, mainOrb: undefined, threads: [] }, conversation: undefined });
  expect(html).toContain("No main thread yet. Send a message to start one.");
  expect(html).toContain("prj-first-box");
});

test("MEMORY.md is named by its filename and counted in lines, and nothing calls it memory", () => {
  const html = page();
  expect(html).toContain("MEMORY.md");
  expect(html).toContain("3 lines");
  expect(html).toContain("Prepended to every session in this project. Nothing writes this but you and the agent.");
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
  expect(page()).toContain("Each thread runs in a container of its own.");
  const down = page({ detail: { ...detail, mainOrb: { ...detail.mainOrb!, status: "stopped", up: false } } });
  expect(down).toContain("Main thread’s orb is stopped. It starts when you message the project.");
  const never = page({ detail: { ...detail, mainOrb: undefined } });
  expect(never).toContain("No orb yet. It starts when you first message the project.");
});

test("a definition that does not parse keeps its page and points at the editor", () => {
  const html = page({ detail: { ...detail, error: "yaml: line 3: did not find expected key" } });
  expect(html).toContain("yaml: line 3: did not find expected key");
  expect(html).toContain("Edit project.yml");
});

test("an opened thread swaps the conversation and offers the way back", () => {
  const html = page({ open: "t2", conversation: <div className="thread">thread</div> });
  expect(html).toContain("‹ Main thread");
  expect(html).toContain("Migration order");
});

test("threadGroup reads the one status vocabulary", () => {
  expect(threadGroup(row("x", "x", { status: "needs-you" }))).toBe("needs-you");
  expect(threadGroup(row("x", "x", { status: "done", ask: { id: "a", text: "?", options: [], seq: 1 } }))).toBe("needs-you");
  expect(threadGroup(row("x", "x", { status: "done", testsFailed: true, lastAt: at(1) }))).toBe("error");
  expect(threadGroup(row("x", "x", { status: "queued" }))).toBe("running");
  expect(threadGroup(row("x", "x", { status: "interrupted" }))).toBe("interrupted");
  expect(threadGroup(row("x", "x", { status: "stopped" }))).toBe("idle");
});

test("groupThreads drops empty groups and keeps the urgency order", () => {
  const gs = groupThreads(threads);
  expect(gs.map((g) => g.group)).toEqual(["needs-you", "error", "running", "interrupted", "idle"]);
  expect(gs[4].rows.length).toBe(2);
  expect(groupThreads([]).length).toBe(0);
  expect(groupThreads([row("z", "z")]).map((g) => g.group)).toEqual(["idle"]);
});

test("a thread's second line says what it is waiting on, not its id", () => {
  expect(threadNote(threads[1])).toBe("Staging first?");
  expect(threadNote(row("x", "x", { trouble: "tests failed" }))).toBe("tests failed");
  expect(threadNote(row("x", "x"))).toBe("");
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
  expect(html).toContain("Empty. Write what every session in this project should know: what it is, where things live, decisions already made.");
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
  const html = page();
  // The toggle for the thread list, the scrim that dismisses a drawer,
  // and a Close inside each one. Which of them is visible is the
  // stylesheet's business.
  expect(html).toContain("prj-threads-btn");
  expect(html).toContain("prj-panel-btn");
  expect(html).toContain("prj-scrim");
  expect(html.split("prj-drawer-close").length - 1).toBe(2);
});

// The main thread is the project's home: it opens with the project's
// state, above its own transcript. A thread opened in its place is just
// that thread's conversation.
test("main opens with the status strip, the latest update and the moving threads, above the conversation", () => {
  const html = page();
  expect(html).toContain("prj-home");
  expect(html.indexOf("prj-home")).toBeLessThan(html.indexOf(">conversation<"));
  expect(html).toContain('data-group="needs-you"');
  expect(html).toContain("1</b><span>need you");
  expect(html).toContain("1</b><span>error");
  // Interrupted and idle both count; nothing is a group of zero but idle.
  expect(html).toContain('data-group="interrupted"');
  expect(html).toContain("2</b><span>idle");
  // The latest update is the freshest thread with something to say: the
  // question. The error an hour later has no text of its own.
  expect(html).toContain("prj-latest");
  expect(html).toContain("prj-latest-text\">Staging first?<");
  expect(html).not.toContain("Archived. A message reopens it.");
});

test("an opened thread has no home, and an archived main says so", () => {
  expect(page({ open: "t2", conversation: <div className="thread">thread</div> })).not.toContain("prj-home");
  expect(page({ detail: { ...detail, mainArchived: true } })).toContain("Archived. A message reopens it.");
  expect(page({ detail: { ...detail, threads: [] } })).not.toContain("prj-home");
});

test("threadCounts, latestThread and homeThreads read the same vocabulary as the column", () => {
  expect(threadCounts(threads)).toEqual({ "needs-you": 1, error: 1, running: 1, interrupted: 1, idle: 2 });
  expect(latestThread(threads)?.id).toBe("t2");
  expect(latestThread([row("q", "quiet")])).toBeUndefined();
  // Everything moving or stuck, then the freshest idle ones; the cap never cuts a live thread.
  const many = [...threads, ...Array.from({ length: 10 }, (_, i) => row(`i${i}`, `idle ${i}`, { status: "done" }))];
  const shown = homeThreads(many);
  expect(shown.length).toBe(6);
  expect(shown.slice(0, 4).every((r) => threadGroup(r) !== "idle")).toBe(true);
  const live = Array.from({ length: 9 }, (_, i) => row(`r${i}`, `run ${i}`, { status: "running" }));
  expect(homeThreads(live).length).toBe(9);
});
