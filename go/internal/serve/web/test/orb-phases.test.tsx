import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ModeChip, OrbPhases, phaseDuration } from "../src/mode";
import { ProjectOrb } from "../src/orb";
import type { OrbDetail, OrbState, Project, Row } from "../src/types";

const noop = () => {};
const at = (s: number) => new Date(Date.UTC(2026, 8, 17, 10, 0, s)).toISOString();

test("PHASES: the header chip opens a popover of named, timed start phases", () => {
  const row = { id: "s1", cwd: "/", status: "idle", title: "T", mode: "project", orb: { project: "web", status: "building" } } as unknown as Row;
  const html = renderToStaticMarkup(<ModeChip row={row} name="Web" phases />);
  expect(html).toContain("<details");
  expect(html).toContain("<summary");
  expect(html).toContain("Web · Building");
  // Sidebar rows sit inside a button: no disclosure there.
  expect(renderToStaticMarkup(<ModeChip row={row} bare />)).not.toContain("<summary");
});

test("PHASES: each phase shows its word, state and time; a failure names its error", () => {
  const orb = {
    session: "s1", project: "web", status: "failed", phase: "resume.sh", updatedAt: at(40), container: "bough-orb-s1",
    phases: [
      { name: "sync", startedAt: at(0), endedAt: at(1) },
      { name: "build", startedAt: at(1), endedAt: at(31) },
      { name: "resume.sh", startedAt: at(31), endedAt: at(35), error: "resume.sh: exit status 3" },
    ],
  } as OrbState;
  const html = renderToStaticMarkup(<OrbPhases orb={orb} now={Date.parse(at(50))} />);
  for (const w of ["Sync repos", "Build image", "resume.sh", "30s", "exit status 3", "bough-orb-s1"]) expect(html).toContain(w);
  // Steps not reached are listed as pending, so the whole start reads in order.
  expect(html).toContain("Ready");
  expect(phaseDuration({ name: "build", startedAt: at(0) }, Date.parse(at(75)))).toBe("1m 15s");
});

test("PHASES: a build log from an older image is labelled, never read as this image's state", () => {
  const detail = {
    files: { "project.yml": "" }, runtime: { name: "apple", available: true }, orb: { image: "bough-orb/web:new", built: true },
    build: { tag: "bough-orb/web:old", state: "failed" }, orbs: [],
  } as unknown as OrbDetail;
  const html = renderToStaticMarkup(<ProjectOrb project={{ id: "p1", name: "Web", slug: "web" } as Project} detail={detail} log="x"
    onAttach={noop} onDetach={noop} onSave={async () => {}} onBuild={noop} onStopOrb={noop} />);
  expect(html).toContain("older image · failed");
});
