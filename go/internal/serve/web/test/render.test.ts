import { expect, test } from "bun:test";
import { stripRunFences } from "../src/render";

const fence = (body: string) => "```js\n" + body + "\n```";

test("a reply's unrun program fences are dropped, not shown as raw code", () => {
  const ran = 'console.log(tools.bash("gh pr view 7754"));';
  const text = [fence(ran), fence('console.log(tools.bash("pwd"));'), fence("console.log(tools.jobs());"), "Blocked: gh is missing."].join("\n\n\n");
  expect(stripRunFences(text, [ran])).toBe("Blocked: gh is missing.");
});

test("a fence that calls no tools is prose and stays", () => {
  const text = "Run this:\n\n" + fence("const x = 1;") + "\n\nDone.";
  expect(stripRunFences(text, [])).toBe(text);
});

import { execNote, resultLabel } from "../src/render";
import { toolCallLabel } from "../src/code";

test("execNote reads both loop notes", () => {
  expect(execNote("out\n\n[2 further code block(s) dropped — a reply runs at most 1 blocks]")).toEqual({ notRun: 2, reason: "a reply runs at most 1 blocks" });
  expect(execNote("[the 3 code block(s) after this one in your reply were not run: this block failed.]")).toEqual({ notRun: 3, reason: "this block failed" });
  expect(execNote("Result [1, 2]")).toBeNull();
});

test("toolCallLabel names job calls only", () => {
  expect(toolCallLabel("console.log(tools.jobWait(177, 15))")).toBe("Waiting for Job 177 · limit 15s");
  expect(toolCallLabel("tools.job(177)")).toBe("Read output from Job 177");
  expect(toolCallLabel("tools.jobs()")).toBe("Listed jobs");
  expect(toolCallLabel('tools.bash("ls")')).toBeNull();
});

test("resultLabel never leaks payload", () => {
  expect(resultLabel(3)).toBe("Subagent results · 3");
  expect(resultLabel()).toBe("Result");
});
