import { expect, test } from "bun:test";
const { groupTools } = await import("../src/render");
import type { Line } from "../src/types";

// On the engine a reply's calls run in parallel: a run_js still running
// while the person answers the ask beside it, steers, or the model's
// request fails. What lands meanwhile is recorded between the program
// and its result, and must not orphan the program (it read "Running"
// for good, its result shown apart).
const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const js = "tools.bash('make test')";
const code: Line = { seq: 2, at: at(0), kind: "code", text: js };
const ask: Line = { seq: 3, at: at(0), kind: "ask", text: "Which colour?", data: { id: "ask-1", call: "toolu_a" } };
const steer: Line = { seq: 4, at: at(1), kind: "input", text: "msg-1", data: { steer: true } };
const err: Line = { seq: 5, at: at(1), kind: "error", text: "provider boom" };
const answer: Line = { seq: 6, at: at(2), kind: "ask/answer", text: "red", data: { id: "ask-1" } };
const askEnd: Line = { seq: 7, at: at(2), kind: "call", text: "Which colour?", data: { id: "toolu_a", tool: "ask", output: "red" } };
const result: Line = { seq: 8, at: at(3), kind: "result", text: "ok", data: { code: js, ms: 3000 } };

const items = (ls: Line[]) => ls.map((l) => ({ kind: "line" as const, seq: l.seq, line: l }));

test("EP: a program keeps its result across what landed while it ran", () => {
  const out = groupTools(items([code, ask, steer, err, answer, askEnd, result]), [js]);
  const tools = out.filter((it) => it.kind === "tools");
  expect(tools.length).toBe(1);
  const lines = tools[0].kind === "tools" ? tools[0].lines : [];
  expect(lines.map((l) => l.seq)).toEqual([2, 7, 8]);
  // What landed meanwhile still shows, in its order, after the work.
  expect(out.filter((it) => it.kind === "line").map((it) => it.seq)).toEqual([4, 5, 6]);
});

test("EP: with no result to come, nothing is held back", () => {
  const out = groupTools(items([code, ask, answer, askEnd]), [js]);
  expect(out.map((it) => it.kind)).toEqual(["tools", "line", "tools"]);
});
