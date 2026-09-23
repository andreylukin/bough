import { afterAll, beforeAll, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Sidebar } from "../src/app";
import { GROUP_LABEL, groupThreads, projectOf } from "../src/project";
import type { Row } from "../src/types";

// The sidebar's project group and the project page list the same threads:
// main's children, a background run and an empty thread used to be folded
// into main's row, moved to Background or dropped by the sidebar alone.
const g = globalThis as unknown as { window?: object };
beforeAll(() => { g.window ??= {}; });
afterAll(() => { delete g.window; });

const at = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();
const row = (id: string, title: string, over: Partial<Row> = {}): Row => ({
  id, title, cwd: "/w/bough", repo: "/w/bough", status: "done", live: false, archived: false, entries: 3,
  modified: at(2), lastAt: at(2), mode: "project", project: "bough", ...over,
} as Row);

const rows: Row[] = [
  row("main", "Main thread", { status: "done" }),
  row("t-run", "Unreal harness", { status: "running", live: true, spawnedBy: "main", lastAt: at(1) }),
  row("t-bg", "Open PR summary", { status: "running", live: true, background: true, spawnedBy: "main" }),
  row("t-empty", "", { status: "idle", empty: true, entries: 0, lastAt: at(3) }),
  row("t-cli", "Local service startup", { status: "done", lastAt: at(5) }),
  row("t-old", "Secure key prompt", { status: "done", lastAt: at(24 * 9) }),
  // Not the project's: a local child of main stays under main, as any agent does.
  row("k-local", "Map routes", { status: "running", live: true, spawnedBy: "main", mode: "local", project: undefined }),
];

const sidebar = () => renderToStaticMarkup(<Sidebar rows={rows} projects={[{ slug: "bough", name: "Bough" }]} selected={null}
  onSelect={() => {}} query="" onQuery={() => {}} showArchived={false} onToggleArchived={() => {}} />);

// Every row of the project's group, folds opened: what the sidebar lists under it.
function groupIds(html: string): string[] {
  const start = html.indexOf('data-project="bough"');
  const rest = html.slice(start);
  const end = rest.indexOf('class="ws"', 1);
  return [...(end > 0 ? rest.slice(0, end) : rest).matchAll(/data-id="([^"]+)"/g)].map((m) => m[1]);
}

test("the sidebar's project group holds every thread the page lists, main's children and empty ones included", () => {
  // The page's set: serve's projectDetail, which files by the same field; main pinned apart.
  const page = rows.filter((r) => projectOf(r) === "bough" && r.id !== "main").map((r) => r.id).sort();
  const html = sidebar();
  const listed = groupIds(html);
  // The empty and the old thread fold under the group's foot, as the page folds Empty and Done.
  expect(html).toMatch(/class="ws-older"[^>]*>.*?2 more</);
  expect(listed).not.toContain("t-empty");
  expect(listed).not.toContain("t-old");
  // A running thread main started is a row of the project, not a count on main.
  expect(listed).toContain("t-run");
  expect(listed).toContain("t-bg");
  // No Background section: the only background run is the project's.
  expect(html).not.toContain(">Background<");
  expect([...listed, "t-empty", "t-old"].filter((id) => id !== "main").sort()).toEqual(page);
  // A local agent is still folded into its parent.
  expect(listed).not.toContain("k-local");
});

test("the project group is ordered as the page orders its threads", () => {
  const listed = groupIds(sidebar()).filter((id) => id !== "main");
  const page = groupThreads(rows.filter((r) => projectOf(r) === "bough" && r.id !== "main")).flatMap((g) => g.rows.map((r) => r.id));
  expect(listed).toEqual(page.filter((id) => listed.includes(id)));
  expect(GROUP_LABEL.running).toBe("Running");
});

test("an archived session is no project's thread", () => {
  expect(projectOf(row("a", "x", { archived: true }))).toBeUndefined();
  expect(projectOf(row("l", "x", { project: undefined }))).toBeUndefined();
  expect(projectOf(row("p", "x"))).toBe("bough");
});

// An errored thread is the page's Error group; it used to leave the
// project's group for Needs you, so the two lists disagreed on it.
test("an errored project thread stays in its project's group and is pinned in Needs you too", () => {
  const withErr = [...rows, row("t-err", "Omni demo plan", { status: "error", lastAt: at(1) })];
  const html = renderToStaticMarkup(<Sidebar rows={withErr} projects={[{ slug: "bough", name: "Bough" }]} selected={null}
    onSelect={() => {}} query="" onQuery={() => {}} showArchived={false} onToggleArchived={() => {}} />);
  expect(groupIds(html)).toContain("t-err");
  expect(html.slice(html.indexOf('id="needs-you"'), html.indexOf('data-project="bough"'))).toContain('data-id="t-err"');
  expect(html).not.toContain("ws-lifted");
});
