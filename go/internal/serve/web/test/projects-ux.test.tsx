import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectsView } from "../src/projects";
import type { Project, Row } from "../src/types";

// UX pass 2026-09-21: the section head counts the repos its project.yml
// declares, like the page head; New project is the page's primary action;
// the empty-session fold folds like a section; a project session's note
// sits in the same box as the Move pill so the columns never shift.
const noop = () => {};
const view = (projects: Project[], rows: Row[]) => renderToStaticMarkup(
  <ProjectsView projects={projects} rows={rows} onOpen={noop} onAssign={noop} onAssignMany={async () => []}
                onCreate={async () => ({ slug: "p" })} onRename={async () => {}} onDelete={noop} />);
const web = { slug: "web", name: "Web", orb: { slug: "web", image: "", built: true, repos: ["shop", "api"] } } as unknown as Project;
const row = (id: string, extra: Partial<Row> = {}) =>
  ({ id, project: "web", title: "t " + id, status: "done", modified: "2026-09-20T00:00:00Z", ...extra }) as Row;

test("the section head and the page head agree on the repo count", () => {
  const html = view([web], [row("a", { repo: "/r/shop" })]);
  expect(html).toContain("1 project · 2 repos");
  expect(html).toContain("1 session · 2 repos");
});

test("New project is the header's primary button", () => {
  expect(view([web], [row("a")])).toMatch(/<button class="btn btn-primary"[^>]*>New project…<\/button>/);
});

test("empty sessions fold behind one chevron line", () => {
  const html = view([web], [row("a"), row("b", { empty: true }), row("c", { empty: true })]);
  expect(html).toMatch(/<button[^>]*class="link proj-empty"[^>]*aria-expanded="false"[^>]*><svg class="proj-chev"/);
  expect(html).toContain("2 empty sessions");
  expect(html).not.toContain("Show 2 empty");
});

test("a project session's fixed note sits in the same .proj-move box as the Move pill", () => {
  const html = view([web], [row("a", { mode: "project" })]);
  expect(html).toMatch(/<div class="proj-move"[^>]*><span class="proj-fixed">In Web<\/span><\/div>/);
});
