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
