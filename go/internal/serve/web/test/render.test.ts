import { expect, test } from "bun:test";
import { splitBareProgram, stripRunFences } from "../src/render";

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

// R2-F: bare tool calls a reply carries outside any fence are program text, never prose.
const flowReply = "\ntools.patch(\"src/math.js\",\n`export function add(a, b) {\n  return a + b;\n}`,\n`export function add(a, b) {\n  return a + b;\n}\n\nexport function subtract(a, b) {\n  return a - b;\n}`)\n\n\n\ntools.write(\"src/main.js\",\n`import { add, subtract } from \"./math.js\";\nconsole.log(add(2, 3));\nconsole.log(subtract(5, 2));\n`)\n\n\n\ntools.patch(\"README.md\",\n`# Demo\n\nA tiny calculator.`,\n`# Demo\n\nA tiny calculator. Supports add(a, b) and subtract(a, b).`)\n\n```js\nconsole.log(tools.bash(\"node src/main.js\"))\n```";

test("bare tools.* calls with multi-line template literals leave no prose", () => {
  const [body, program] = splitBareProgram(stripRunFences(flowReply, []));
  expect(body).toBe("");
  expect(program).toContain("tools.patch(\"README.md\"");
  expect(program).toContain("export function subtract(a, b) {\n  return a - b;\n}`)");
  expect(program).toContain("tools.write(\"src/main.js\"");
});

test("prose + tools.patch(...) + prose keeps both prose paragraphs and moves only the program", () => {
  const text = "I'll add the function.\n\ntools.patch(\"src/math.js\", `a`, `a\n\nb`)\nawait tools.bash(\"node src/main.js\")\n\nThat should print `3`, then I'll check `tools.patch` output.";
  const [body, program] = splitBareProgram(text);
  expect(body).toBe("I'll add the function.\n\nThat should print `3`, then I'll check `tools.patch` output.");
  expect(program).toBe("tools.patch(\"src/math.js\", `a`, `a\n\nb`)\nawait tools.bash(\"node src/main.js\")");
});

test("program runs between several prose paragraphs are all collected", () => {
  const text = "First.\n\nconst r = tools.bash(\"ls\")\n\nMiddle words.\n\nconsole.log(tools.write(\"x\", `y`))\n\nEnd.";
  const [body, program] = splitBareProgram(text);
  expect(body).toBe("First.\n\nMiddle words.\n\nEnd.");
  expect(program).toBe("const r = tools.bash(\"ls\")\n\nconsole.log(tools.write(\"x\", `y`))");
});

test("prose that only mentions code, or fenced code, stays prose", () => {
  const text = "Use `tools.patch(path, old, new)` to edit.\n\n```js\nconst x = 1\n```";
  expect(splitBareProgram(text)).toEqual([text, ""]);
  expect(splitBareProgram("const words are not code here.")).toEqual(["const words are not code here.", ""]);
});

test("a bracket inside a line comment does not swallow the prose after the run", () => {
  const text = "Intro.\n\nawait tools.bash(\"ls\") // step 1 (list\n\nAll done.";
  expect(splitBareProgram(text)).toEqual(["Intro.\n\nAll done.", "await tools.bash(\"ls\") // step 1 (list"]);
});
