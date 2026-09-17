import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Thread } from "../src/app";
import { ProjectOrb, stopOrbQuestion } from "../src/orb";
import type { Line, OrbDetail, Project, Row } from "../src/types";
import { jobsFromLines, parseLegacyJob, workCounts } from "../src/work";

const noop = () => {};
const lines = [{ seq: 1, kind: "input", text: "hi", at: "2026-09-16T00:00:00Z" }] as unknown as Line[];
const props = {
  onSend: async () => null, onAnswer: async () => null, onInterrupt: noop, onArchive: noop, onRename: async () => {},
  onModel: noop, onEffort: noop, onAssign: noop, onBack: noop, onContext: noop, onAck: noop, onStopOrb: noop,
  projects: [], busy: false, jump: null,
};
const row = (orb: Row["orb"]) => ({ id: "s1", cwd: "/", status: "idle", title: "T", mode: "project", orb } as unknown as Row);

test("A4: Stop orb shows whenever the container runs, a failed setup included", () => {
  expect(renderToStaticMarkup(<Thread row={row({ project: "p", status: "failed", up: true })} lines={lines} {...props} />)).toContain(">Stop orb<");
  expect(renderToStaticMarkup(<Thread row={row({ project: "p", status: "failed" })} lines={lines} {...props} />)).not.toContain(">Stop orb<");
  expect(renderToStaticMarkup(<Thread row={row({ project: "p", status: "running", up: true })} lines={lines} {...props} />)).toContain(">Stop orb<");

  const detail = {
    files: { "project.yml": "" }, runtime: { name: "apple", available: true }, orb: { image: "i", built: true }, build: { tag: "i", state: "ok" },
    orbs: [{ session: "2026-09-13-abcdef", status: "failed", up: true, error: "resume.sh: exit 3" }],
  } as unknown as OrbDetail;
  expect(renderToStaticMarkup(<ProjectOrb project={{ id: "p1", name: "Web", slug: "web" } as Project} detail={detail} log=""
    onAttach={noop} onDetach={noop} onSave={async () => {}} onBuild={noop} onStopOrb={noop} />)).toContain("Stop orb");
});

test("A4: Stop asks only when jobs run, and names them", () => {
  expect(stopOrbQuestion([])).toBeNull();
  expect(stopOrbQuestion(undefined)).toBeNull();
  const q = stopOrbQuestion([{ id: 3, cmd: "bun run dev\n--watch", started: "" }, { id: 4, cmd: "make test", started: "" }]);
  expect(q?.title).toBe("Stop the orb?");
  expect(q?.body).toContain("2 running jobs stop too");
  expect(q?.body).toContain("bun run dev");
  expect(q?.body).toContain("make test");
  expect(q?.body).not.toContain("--watch");
});

test("A4: a job killed by Stop orb reads stopped, never failed", () => {
  const ws = jobsFromLines([
    { seq: 1, at: "2026-09-16T00:00:00Z", kind: "job", text: "", data: { id: 1, event: "started", cmd: "sleep 60" } },
    { seq: 2, at: "2026-09-16T00:00:05Z", kind: "job", text: "", data: { id: 1, event: "finished", cmd: "sleep 60", exit: 137, stopped: true } },
  ] as unknown as Line[], "s", true);
  expect(ws[0].life).toBe("stopped");
  expect(workCounts(ws).failed).toBe(0);
  expect(parseLegacyJob("job 1 [stopped with the orb] sleep 60 (5s)")?.life).toBe("stopped");
});
