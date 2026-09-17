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

// R2-G: a program's edits, counted from the calls themselves.
import { callEdits, diffRows } from "../src/changes";

const flows = 'console.log(tools.patch("src/math.js",\n`export function add(a, b) {\n  return a + b;\n}`,\n`export function add(a, b) {\n  return a + b;\n}\n\nexport function subtract(a, b) {\n  return a - b;\n}`))\n\ntools.write("src/main.js",\n`import { add, subtract } from "./math.js";\nconsole.log(add(2, 3));\nconsole.log(subtract(5, 2));\n`)\n\nconsole.log(tools.patch("README.md",\n`# Demo\n\nA tiny calculator.`,\n`# Demo\n\nA tiny calculator. Supports add(a, b) and subtract(a, b).`))\n\nconsole.log(tools.bash("node src/main.js"))\n';

test("every patched and written file is counted, a write as all additions", () => {
  const e = callEdits(flows);
  expect(e.map((f) => [f.path, f.add, f.del])).toEqual([["src/math.js", 4, 0], ["src/main.js", 3, 0], ["README.md", 1, 1]]);
});

test("a diff never opens on a blank line", () => {
  for (const f of callEdits(flows)) {
    const first = diffRows(f.hunks).find((l) => l.kind !== "hunk")!;
    expect(first.text.slice(1).trim()).not.toBe("");
  }
  // A git hunk too: the blank context line above the change is dropped, numbers kept.
  const rows = diffRows(parseHunks("@@ -1,3 +1,3 @@\n # Demo\n \n-A tiny calculator.\n+A tiny calculator. Supports add.\n"));
  expect(rows[0]).toMatchObject({ kind: "ctx", text: " # Demo", old: 1, new: 1 });
  expect(rows.filter((l) => l.kind !== "hunk").map((l) => l.text)).toEqual([" # Demo", " ", "-A tiny calculator.", "+A tiny calculator. Supports add."].slice(0, 4));
});

test("an added block that starts blank is slid so it starts with code", () => {
  const [math] = callEdits(flows);
  const adds = diffRows(math.hunks).filter((l) => l.kind === "add");
  expect(adds[0].text).not.toBe("+");
  expect(adds.length).toBe(4);
});

test("context is cut to two lines around each change", () => {
  const old = Array.from({ length: 12 }, (_, i) => `l${i + 1}`).join("\n");
  const rows = diffRows(parseHunks(`@@ -1,12 +1,12 @@\n${old.split("\n").map((l, i) => (i === 5 ? `-${l}\n+L6` : ` ${l}`)).join("\n")}\n`));
  expect(rows.map((l) => l.text)).toEqual([" l4", " l5", "-l6", "+L6", " l7", " l8"]);
  expect(rows[0]).toMatchObject({ old: 4, new: 4 });
});
import { parseHunks } from "../src/changes";
