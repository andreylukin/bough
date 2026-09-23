import { afterAll, beforeAll, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Sidebar, dropProject } from "../src/app";
import { ProjectPage, threadDrop, type ThreadGroup } from "../src/project";
import type { ProjectDetail, Row } from "../src/types";

// Sidebar reads window.navigation at mount; a bare object is enough for a static render.
const g = globalThis as unknown as { window?: object };
beforeAll(() => { g.window ??= {}; });
afterAll(() => { delete g.window; });

const at = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();
const row = (id: string, over: Partial<Row> = {}): Row => ({
  id, title: `session ${id}`, cwd: "/w/bough", repo: "/w/bough", status: "idle", live: false, archived: false, entries: 3,
  modified: at(1), lastAt: at(1), mode: "local", ...over,
});

test("a local session drops into another project, not the one it is in", () => {
  const group = [row("a", { project: "web" })];
  expect(dropProject(row("x"), group, "project:web")).toBe("web");
  expect(dropProject(row("x", { project: "api" }), group, "project:web")).toBe("web");
  expect(dropProject(row("x", { project: "web" }), group, "project:web")).toBeUndefined();
});

test("a project session never drops anywhere: it lives in its project's orb", () => {
  const r = row("x", { mode: "project", project: "api" });
  expect(dropProject(r, [row("a", { project: "web" })], "project:web")).toBeUndefined();
  expect(dropProject(r, [row("b")], "/w/bough")).toBeUndefined();
});

test("a folder takes a session out of its project only when it is the folder it would list under", () => {
  const folder = [row("b")];
  expect(dropProject(row("x", { project: "web" }), folder, "/w/bough")).toBe("");
  expect(dropProject(row("x", { project: "web", repo: "/w/other", cwd: "/w/other" }), folder, "/w/bough")).toBeUndefined();
  // Already out of any project: the folder is where it is.
  expect(dropProject(row("x"), folder, "/w/bough")).toBeUndefined();
});

test("the sidebar makes a row draggable only where it can move", () => {
  const rows = [row("loc", { title: "local one" }), row("prj", { title: "project one", mode: "project", project: "web" })];
  const render = (onMove?: (id: string, p: string) => void) => renderToStaticMarkup(
    <Sidebar rows={rows} selected={null} onSelect={() => {}} query="" onQuery={() => {}} showArchived={false} onToggleArchived={() => {}} onMove={onMove} />);
  const html = render(() => {});
  expect(html).toMatch(/data-id="loc" draggable="true"/);
  expect(html).not.toMatch(/data-id="prj" draggable/);
  // Nothing to move them with, nothing drags.
  expect(render()).not.toContain("draggable");
});

test("only Done takes a dropped thread, and only a finish nobody has seen", () => {
  const groups: ThreadGroup[] = ["needs-you", "error", "running", "interrupted", "unseen", "done", "idle", "empty"];
  const unseen = row("u", { status: "done", unseen: true, mode: "project", project: "bough" });
  expect(groups.filter((to) => threadDrop(unseen, to))).toEqual(["done"]);
  // Seeing an error or an interrupt leaves its status: a drop on Done would do nothing.
  for (const r of [row("e", { status: "error" }), row("i", { status: "interrupted" }), row("d", { status: "done" }), row("r", { status: "running", live: true })])
    expect(groups.some((to) => threadDrop(r, to))).toBe(false);
});

test("the project page makes an unseen thread draggable, and nothing else", () => {
  const detail: ProjectDetail = {
    slug: "bough", name: "bough", orbs: [],
    threads: [row("u", { title: "Unseen finish", status: "done", unseen: true, project: "bough" }), row("d", { title: "Seen finish", status: "done", project: "bough" })],
  };
  const noop = () => {};
  const render = (onSeen?: (id: string) => void) => renderToStaticMarkup(
    <ProjectPage detail={detail} files={{}} open="" onOpen={noop} onStopOrb={noop} onRetry={noop} onSave={async () => {}} onMessage={async () => {}} onSeen={onSeen} />);
  const html = render(noop);
  expect(html.match(/draggable="true"/g)?.length).toBe(1);
  expect(html).toMatch(/draggable="true"[^>]*aria-label="Unseen finish/);
  expect(render()).not.toContain("draggable");
});
