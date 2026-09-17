import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectOrb, removeOrbQuestion } from "../src/orb";
import type { OrbDetail, OrbRemovePlan, Project } from "../src/types";

const noop = () => {};
const detail = (orbs: unknown[]) => ({
  files: { "project.yml": "" }, runtime: { name: "apple", available: true }, orb: { image: "i", built: true }, build: { tag: "i", state: "ok" }, orbs,
} as unknown as OrbDetail);
const render = (orbs: unknown[]) => renderToStaticMarkup(<ProjectOrb project={{ id: "p1", name: "Web", slug: "web" } as Project}
  detail={detail(orbs)} log="" onAttach={noop} onDetach={noop} onSave={async () => {}} onBuild={noop} onStopOrb={noop} onRemoveOrb={noop} />);

test("B1: Remove orb is offered for a stopped or failed-start orb, not a running one", () => {
  expect(render([{ session: "s1", status: "failed" }])).toContain(">Remove…<");
  expect(render([{ session: "s1", status: "stopped" }])).toContain(">Remove…<");
  expect(render([{ session: "s1", status: "running", up: true }])).not.toContain(">Remove…<");
});

test("B1: the confirm lists what is deleted and what is kept, with the disk it frees", () => {
  const plan: OrbRemovePlan = {
    session: "s1", project: "web", status: "failed", container: "bough-orb-s1", dir: "/h/.bough/orbs/s1",
    worktrees: ["/h/.bough/orbs/s1/web"], bytes: 5 * 1024 * 1024,
    branches: [{ repo: "web", gitDir: "/r/web", branch: "bough/s1", delete: false, reason: "has commits not merged or pushed" }],
  };
  const q = removeOrbQuestion(plan);
  expect(q.title).toBe("Remove this orb?");
  expect(q.body).toContain("Deletes:");
  expect(q.body).toContain("container bough-orb-s1");
  expect(q.body).toContain("worktree /h/.bough/orbs/s1/web");
  expect(q.body).toContain("5.0 MB");
  expect(q.body).toContain("Keeps:");
  expect(q.body).toContain("branch bough/s1 in web (has commits not merged or pushed)");
  const bare = removeOrbQuestion({ ...plan, container: undefined, worktrees: [], branches: [], bytes: 12 });
  expect(bare.body).not.toContain("container");
  expect(bare.body).toContain("Keeps:\n• the session's history");
});

test("B1: uncommitted changes replace the remove confirm with a refusal naming the worktrees", () => {
  const q = removeOrbQuestion({ session: "s1", project: "web", status: "stopped", dir: "/d", worktrees: ["/d/web"], dirty: ["/d/web"], bytes: 1, branches: [] });
  expect(q.title).toBe("Uncommitted changes");
  expect(q.body).toContain("• /d/web");
  expect(q.body).not.toContain("Deletes:");
});

test("B1: the confirm body keeps its line breaks, mirrored in design/bough.css", () => {
  for (const f of ["../dist/index.html", "../design/bough.css"]) {
    const css = readFileSync(new URL(f, import.meta.url), "utf8");
    expect(css).toMatch(/\/\* ORB-B1 \*\/[\s\S]*\.dlg-body\{white-space:pre-line\}[\s\S]*\/\* \/ORB-B1 \*\//);
  }
});
