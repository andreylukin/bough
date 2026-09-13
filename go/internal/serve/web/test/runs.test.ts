import { expect, test } from "bun:test";
import { finishedJobs, lastTestRun } from "../src/runs";
import type { Line } from "../src/types";

let seq = 0;
const at = "2026-09-13T10:00:00Z";
const code = (cmd: string): Line => ({ seq: ++seq, at, kind: "code", text: `await tools.bash(${JSON.stringify(cmd)})` });
const result = (exit?: number): Line => ({ seq: ++seq, at, kind: "result", text: "", data: exit === undefined ? {} : { exit } });
const done = (): Line => ({ seq: ++seq, at, kind: "done", text: "" });
const prompt = (t: string): Line => ({ seq: ++seq, at, kind: "prompt", text: t });

test("a failure stays after a later turn that ran no tests", () => {
  const r = lastTestRun([code("cd go && go test ./..."), result(1), done(), prompt("PING"), code("echo PONG"), result(0), done()], false);
  expect(r?.state).toBe("failed");
});

test("a passing rerun replaces the failure", () => {
  const r = lastTestRun([code("go test ./..."), result(1), code("go test ./..."), result(0), done()], false);
  expect(r?.state).toBe("passed");
});

test("a result pairs with its own call, not a later one", () => {
  const r = lastTestRun([code("bun test"), { seq: ++seq, at, kind: "stream", text: "" }, result(2), code("ls"), result(0)], false);
  expect(r).toMatchObject({ state: "failed", exit: 2 });
});

test("a test call without a result is running while the turn runs, else not recorded", () => {
  const lines = [code("pytest"), result(0), code("pytest -x")];
  expect(lastTestRun(lines, true)?.state).toBe("running");
  expect(lastTestRun(lines, false)?.state).toBe("unrecorded");
  expect(lastTestRun([code("pytest"), result()], false)?.state).toBe("unrecorded");
});

test("finished jobs read exit, command, time and output", () => {
  const j = finishedJobs([{ seq: 1, at, kind: "job", text: "job 2 [exited 1] make check (4s)\nFAIL x" }, { seq: 2, at, kind: "job", text: "job 1 matched \"ok\" while running: sleep 9\nok" }]);
  expect(j).toEqual([{ id: 2, exit: "exited 1", cmd: "make check", took: "4s", output: "FAIL x", seq: 1, at }]);
});
