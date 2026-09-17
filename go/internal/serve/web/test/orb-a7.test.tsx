import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ModeChip } from "../src/mode";
import { ProjectOrb } from "../src/orb";
import { ProjectsView } from "../src/projects";
import type { OrbDetail, Project, Row } from "../src/types";

const noop = () => {};
const detail = (orbs: unknown[]) => ({
  files: { "project.yml": "repos: []\n" }, runtime: { name: "apple", available: true },
  orb: { image: "bough-orb/web:abc", built: true }, build: { tag: "", state: "" }, orbs,
}) as unknown as OrbDetail;
const orbRow = (project: Project, d: OrbDetail, titles = {}) => renderToStaticMarkup(
  <ProjectOrb project={project} detail={d} log="" titles={titles} onAttach={noop} onDetach={noop}
              onSave={async () => {}} onBuild={noop} onStopOrb={noop} />);
const web = { id: "p1", name: "Web", slug: "web" } as Project;

test("A7: each Container cell shows its own container's tail, not the shared prefix", () => {
  const html = orbRow(web, detail([
    { session: "01a0af2a-1111-7000-8000-aaaaaa111111", container: "bough-orb-01a0af2a-1111-7000-8000-aaaaaa111111", status: "stopped" },
    { session: "01a0af2a-2222-7000-8000-bbbbbb222222", container: "bough-orb-01a0af2a-2222-7000-8000-bbbbbb222222", status: "stopped" },
  ]));
  expect(html).not.toContain(">bough-orb-01<");
  expect(html).toContain("111111");
  expect(html).toContain("222222");
});

test("A7: an archived session keeps its title in the orb list", () => {
  const html = orbRow(web, detail([{ session: "01a0af2a-3333", title: "Debug Local Server Connectivity", status: "stopped" }]));
  expect(html).toContain("Debug Local Server Connectivity");
  expect(html).not.toContain("Untitled session");
});

test("A7: the orb blurb no longer says local sessions only read", () => {
  expect(orbRow(web, detail([]))).not.toContain("only read");
  expect(orbRow({ id: "p2", name: "Bare" } as Project, detail([]))).not.toContain("only read");
});

test("A7: the sidebar's orb chip names the orb and never wears the turn's running tone", () => {
  const r = { id: "s", mode: "project", status: "done", orb: { project: "web", status: "running", up: true } } as Row;
  const html = renderToStaticMarkup(<ModeChip row={r} bare />);
  expect(html).toContain("Orb running");
  expect(html).not.toContain("mode-running");
});

const view = (projects: Project[]) => renderToStaticMarkup(
  <ProjectsView projects={projects} rows={[]} onOpen={noop} onAssign={noop} onAssignMany={async () => []}
                onCreate={async () => ({ id: "p" })} onRename={async () => {}} onDelete={noop} onNewSession={noop} />);

test("A7: a project with an orb has a visible Orb button and New session on its row", () => {
  const html = view([{ ...web, orb: { slug: "web", image: "", built: true, repos: ["shop"] } }]);
  expect(html).toMatch(/<button[^>]*proj-orb-btn[^>]*>Orb<\/button>/);
  expect(html).toMatch(/<button[^>]*>New session<\/button>/);
  // A label without an orb gets Add orb and no New session.
  const bare = view([{ id: "p2", name: "Bare" } as Project]);
  expect(bare).toContain("Add orb");
  expect(bare).not.toContain("New session");
});

test("A7: the header counts the repos a project.yml names", () => {
  const html = view([{ ...web, orb: { slug: "web", image: "", built: true, repos: ["shop", "api"] } }]);
  expect(html).toContain("1 project · 2 repos");
});
