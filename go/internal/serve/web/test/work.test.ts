import { expect, test } from "bun:test";
import { groupTurns } from "../src/render";
import type { Line, Row } from "../src/types";
import {
  agentsFromRows, isNewWork, jobTitle, jobsFromLines, loadReviewed, parseLegacyJob, saveReviewed, subagentsFromTurn,
  workCounts, workElapsedMs, workFingerprint, workIndex, workSummaryText, type Worker,
} from "../src/work";

let seq = 0;
const t0 = Date.parse("2026-09-13T10:00:00Z");
const at = (s = 0) => new Date(t0 + s * 1000).toISOString();
const line = (kind: string, text: string, data?: Record<string, unknown>, s = 0): Line => ({ seq: ++seq, at: at(s), kind, text, data });
const typed = (id: number, event: string, extra: Record<string, unknown> = {}, s = 0) => line("job", "", { id, event, cmd: "bun test", ...extra }, s);
const row = (over: Partial<Row>): Row => ({ id: "p", title: "", cwd: "/", status: "idle", live: true, archived: false, entries: 1, modified: at(), lastAt: at(), ...over });

test("typed job: exit 0 finished, non-zero failed, absent unknown with no output recorded", () => {
  const ws = jobsFromLines([
    typed(1, "started"), typed(1, "finished", { exit: 0 }, 4),
    typed(2, "started"), typed(2, "finished", { exit: 2 }),
    typed(3, "started"), typed(3, "finished"),
  ], "s", true);
  expect(ws.map((w) => [w.id, w.life, w.exit])).toEqual([["1", "finished", 0], ["2", "failed", 2], ["3", "unknown", undefined]]);
  expect(ws[0].ms).toBe(4000);
  expect(ws[1].ms).toBeUndefined(); // same timestamps: omitted, never 0s
  expect(ws[2]).toMatchObject({ exitNote: "Exit not recorded.", outputState: "not-recorded", canStop: false });
});

test("typed job started with no outcome runs only while the session is live", () => {
  expect(jobsFromLines([typed(4, "started", { until: "ready" })], "s", true)[0]).toMatchObject({ life: "running", canStop: true, outputState: "none" });
  expect(jobsFromLines([typed(4, "started")], "s", false)[0]).toMatchObject({ life: "unknown", canStop: false });
});

test("legacy [failed] is failed with no invented exit; [exited N] reads N and the recorded time", () => {
  expect(parseLegacyJob("job 6 [failed] bun test (1s): exit status 1\nCannot find module")).toEqual({ id: 6, life: "failed", cmd: "bun test", ms: 1000, output: "Cannot find module" });
  expect(parseLegacyJob("job 2 [exited 1] make check (1m 5s)\nFAIL x")).toMatchObject({ life: "failed", exit: 1, ms: 65000 });
  expect(parseLegacyJob("job 2 [exited 0] true (0s)\n")).toEqual({ id: 2, life: "finished", exit: 0, cmd: "true", output: "" });
  expect(parseLegacyJob("not a job")).toBeNull();
  const [w] = jobsFromLines([line("job", "job 6 [failed] bun test (1s)\n", { text: "job 6 [failed] bun test (1s)\n" })], "s", true);
  expect(w).toMatchObject({ life: "failed", outputState: "empty", ms: 1000 });
  expect("exit" in w).toBe(false);
});

test("legacy output merges onto typed metadata by id; matched notices are no outcome", () => {
  const ws = jobsFromLines([
    typed(7, "started"),
    line("job", 'job 7 matched "ok" while running: bun test\nok'),
    typed(7, "finished", { exit: 1 }),
    line("job", "job 7 [failed] bun test (3s)\nboom"),
  ], "s", true);
  expect(ws).toHaveLength(1);
  expect(ws[0]).toMatchObject({ life: "failed", exit: 1, output: "boom", outputState: "recorded", ms: 3000, label: "Job 7" });
});

test("a note carrying the whole command titles the job past its comment and set -e", () => {
  const script = "# rebuild the web bundle\nset -euo pipefail\ncd web\nbun run typecheck";
  const [w] = jobsFromLines([line("job", "job 3 [exited 0] # rebuild the web bundle … (2s)\nok", { cmd: script })], "s", true);
  expect(w.cmd).toBe(script);
  expect(jobTitle(w.cmd!, w.id)).toBe("bun run typecheck");
  expect(jobTitle("# only a comment …", 4)).toBe("Job 4");
  expect(jobTitle("set -e …", 5)).toBe("set -e");
});

test("row jobs list a running job; an ended record wins over it", () => {
  const ws = jobsFromLines([line("job", "job 1 [exited 0] ls (1s)\nx")], "s", true, [{ id: 1, cmd: "ls", started: at() }, { id: 9, cmd: "sleep 9", started: at() }]);
  expect(ws.map((w) => w.life)).toEqual(["finished", "running"]);
  expect(ws[1]).toMatchObject({ cmd: "sleep 9", startedAt: at() });
  expect(ws[1].ms).toBeUndefined();
});

const sub = (kind: string, worker: string, text = "", data: Record<string, unknown> = {}, s = 0) => line(`sub:${kind}`, text, { worker, ...data }, s);

test("subagents: finished with step errors, failed, running, ended run unknown", () => {
  const body = [
    line("input", "go"),
    sub("start", "1", "read api"), sub("start", "2", "fix"), sub("start", "3", "idle lane"),
    sub("code", "1", "tools.read('api.go')"), sub("error", "1", "ENOENT"),
    sub("result", "1", "[the 2 code block(s) after this one were not run: one per reply]"),
    sub("done", "1", "", { status: "ok", steps: 9, text: "summary" }, 205),
    sub("done", "2", "", { status: "error", steps: 1 }),
  ];
  const [open] = groupTurns(body);
  const ws = subagentsFromTurn(open, "s", true);
  expect(ws.map((w) => w.life)).toEqual(["finished", "failed", "running"]);
  expect(ws[0]).toMatchObject({ stepErrors: 1, error: "ENOENT", notRun: 2, result: "summary", steps: 9, recordedSteps: 1, ms: 205000, label: "Subagent 1" });
  expect(ws[0].key).toBe(`s:subagent:${ws[0].subrunSeq}:1:${ws[0].subrunSeq}`);
  expect(ws[2].ms).toBeUndefined();
  // The same records in a turn that ended: the lane without sub:done has no outcome.
  const [ended] = groupTurns([...body, line("done", "")]);
  expect(subagentsFromTurn(ended, "s", true)[2].life).toBe("unknown");
  // A live flag alone never makes a run: an ended parent is unknown too.
  expect(subagentsFromTurn(open, "s", false)[2].life).toBe("unknown");
});

test("agents: queued only from the child's own record; children override rows", () => {
  const parent = row({ id: "p", agents: { running: 1, queued: 3, total: 4 } });
  const rows = [
    row({ id: "c1", spawnedBy: "p", status: "running", title: "Research" }),
    row({ id: "c2", spawnedBy: "p", status: "queued", queued: true, title: "" }),
    row({ id: "c3", spawnedBy: "p", status: "idle", live: false, title: "Done one" }),
    row({ id: "x", spawnedBy: "other", status: "running" }),
  ];
  const ws = agentsFromRows(parent, rows, [row({ id: "c1", spawnedBy: "p", status: "error", live: false, title: "Research" })]);
  expect(ws.map((w) => [w.id, w.life, w.label])).toEqual([["c1", "failed", "Research"], ["c2", "queued", "Queued agent"], ["c3", "finished", "Done one"]]);
  expect(ws.filter((w) => w.life === "queued")).toHaveLength(1); // agents.queued=3 is not per-child evidence
  expect(agentsFromRows(parent, [row({ id: "c9", spawnedBy: "p", status: "running", live: true })])[0]).toMatchObject({ canStop: true, session: "c9", outputState: "none" });
});

test("workIndex merges jobs, subagents and agents", () => {
  const lines = [line("input", "x"), typed(1, "started"), sub("start", "1", "t"), sub("done", "1", "", { status: "ok" })];
  const ws = workIndex({ session: "p", lines, turns: groupTurns(lines), row: row({ id: "p" }), rows: [row({ id: "c", spawnedBy: "p", status: "queued" })], live: true });
  expect(ws.map((w) => `${w.kind}:${w.life}`)).toEqual(["job:running", "subagent:finished", "agent:queued"]);
});

const counts = (lives: string[], newResults = 0) => ({ ...workCounts(lives.map((life) => ({ life }) as Worker)), newResults });

test("summary wording while active and after everything ended", () => {
  expect(workSummaryText(counts(["running", "running", "queued", "failed"])).primary).toBe("Work · 2 running · 1 queued · 1 failed");
  expect(workSummaryText(counts(["running", "unknown"], 1)).primary).toBe("Work · 1 running · 1 unknown · 1 new result");
  expect(workSummaryText(counts(Array(7).fill("finished"))).primary).toBe("Work · 7 finished");
  expect(workSummaryText(counts([...Array(7).fill("finished"), "failed"])).primary).toBe("Work · 8 workers · 1 failed");
  expect(workSummaryText(counts(["stopped", "stopped", "stopped"])).primary).toBe("Work · 3 stopped");
  expect(workSummaryText(counts([]), { loading: true })).toEqual({ primary: "Work · Loading…", aria: "Work, Loading…" });
  expect(workSummaryText(counts([]), { unavailable: true }).primary).toBe("Work · Unavailable");
  expect(workSummaryText(counts(["running"]), { paused: true }).aria).toBe("Work, 1 running, Updates paused");
});

test("narrow summary is the count with only failures as a badge", () => {
  expect(workSummaryText(counts(["running", "running", "queued", "failed"]), { narrow: true })).toMatchObject({ primary: "Work 4", secondary: "1 failed" });
  expect(workSummaryText(counts(["finished", "unknown", "stopped"], 1), { narrow: true })).toEqual({ primary: "Work 3", aria: "Work, 3 workers, 1 unknown, 1 stopped, 1 new result" });
  expect(workSummaryText(counts(["finished", "finished"]), { narrow: true })).toEqual({ primary: "Work 2", aria: "Work, 2 finished" });
});

test("counts use the isNew predicate", () => {
  const ws = [{ life: "finished", kind: "job" }, { life: "failed", kind: "job" }] as Worker[];
  expect(workCounts(ws, (w) => w.life === "finished")).toMatchObject({ finished: 1, failed: 1, newResults: 1, total: 2 });
});

const memory = () => { const m = new Map<string, string>(); return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m }; };

test("review survives reload, pins a fingerprint, and a replay with the same key dedupes", () => {
  const [job] = jobsFromLines([line("job", "job 3 [exited 0] ls (1s)\nout", undefined, 60)], "s", true);
  const s = memory();
  saveReviewed("s", { [job.key]: workFingerprint(job) }, s);
  expect([...s.m.keys()]).toEqual(["bough:work-reviewed:s"]);
  const reloaded = loadReviewed("s", s);
  expect(reloaded[job.key]).toBe(workFingerprint(job));
  // A replay of the same record is the same worker and stays reviewed.
  const [replay] = jobsFromLines([line("job", "job 3 [exited 0] ls (1s)\nout", undefined, 60), line("job", "job 3 [exited 0] ls (1s)\nout", undefined, 60)], "s", true);
  expect(replay.key).toBe(job.key);
  expect(isNewWork(replay, t0, reloaded)).toBe(false);
  // A different terminal outcome is not the reviewed one.
  expect(workFingerprint({ ...job, life: "failed", exit: 1 })).not.toBe(workFingerprint(job));
  expect(loadReviewed("s", { getItem: () => "not json", setItem: () => {} })).toEqual({});
  expect(loadReviewed("s", null)).toEqual({});
});

test("history present on first load is never new; later successful output is", () => {
  const [job] = jobsFromLines([line("job", "job 3 [exited 0] ls (1s)\nout", undefined, 60)], "s", true);
  expect(isNewWork(job, t0 + 120_000, {})).toBe(false);
  expect(isNewWork(job, t0, {})).toBe(true);
  const [empty] = jobsFromLines([line("job", "job 4 [exited 0] true (1s)\n", undefined, 60)], "s", true);
  expect(isNewWork(empty, t0, {})).toBe(false);
  const [failed] = jobsFromLines([line("job", "job 5 [failed] x (1s)\nerr", undefined, 60)], "s", true);
  expect(isNewWork(failed, t0, {})).toBe(false);
});

test("job titles skip comments and the notice's ellipsis", async () => {
  const { jobTitle } = await import("../src/work");
  expect(jobTitle("# …", 4)).toBe("Job 4");
  expect(jobTitle("…", 4)).toBe("Job 4");
  expect(jobTitle("# build it\n\nmake check\nmake lint", 4)).toBe("make check");
  expect(jobTitle("bun test …", 4)).toBe("bun test");
});

test("job summary lines drop tree glyphs and shorten paths", async () => {
  const { jobSummaryLine } = await import("../src/work");
  expect(jobSummaryLine("error: boom\n└──")?.text).toBe("error: boom");
  expect(jobSummaryLine("├── src\n│   └── missing.ts")?.text).toBe("missing.ts");
  const s = jobSummaryLine("open /home/dev/.bough/scratch/0a1b2c/run/out.log: no such file");
  expect(s).toEqual({ text: "open …/out.log: no such file", full: "open /home/dev/.bough/scratch/0a1b2c/run/out.log: no such file" });
  expect(jobSummaryLine("\n──\n")).toBeNull();
});

test("workElapsed: a recorded duration, else a running worker's time since it started, else nothing", () => {
  const w = { life: "running", startedAt: at(0) } as Worker;
  expect(workElapsedMs(w, t0 + 65_000)).toBe(65_000);
  expect(workElapsedMs({ ...w, ms: 4000 }, t0 + 65_000)).toBe(4000);
  expect(workElapsedMs({ ...w, life: "finished" }, t0 + 65_000)).toBeUndefined();
  expect(workElapsedMs({ life: "running" } as Worker, t0)).toBeUndefined();
  expect(workElapsedMs(w, t0 - 5000)).toBe(0);
});

test("a job that finished while the agent was idle keeps its row: the wake-up turn's note is its outcome", () => {
  // serve drops typed job entries from the transcript; with no loop note the wake-up input is the only record.
  const wake = line("input", "[background job] A command you started in the background has finished while you were idle.\n\njob 2 [exited 0] sleep 150 && echo b (2m30s)\nb");
  const ws = jobsFromLines([wake], "s", true);
  expect(ws).toHaveLength(1);
  expect(ws[0]).toMatchObject({ id: "2", life: "finished", exit: 0, cmd: "sleep 150 && echo b", ms: 150000, output: "b" });
  expect(jobsFromLines([line("input", "job 3 [exited 0] typed by a person (1s)")], "s", true)).toHaveLength(0);
});

test("a failed agent row carries serve's reason", () => {
  const [w] = agentsFromRows(row({ id: "p" }), [], [row({ id: "c", spawnedBy: "p", status: "error", live: false, error: "llm-anthropic: 401 Unauthorized" })]);
  expect(w).toMatchObject({ life: "failed", error: "llm-anthropic: 401 Unauthorized" });
});
