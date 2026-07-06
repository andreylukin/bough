import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { reconcileEdit } from "./apply.ts";

/** A completer scripted with one raw reply per attempt. Records its calls. */
function scripted(replies: string[]) {
  const calls: { user: string; temperature: number }[] = [];
  const complete = (_s: string, user: string, temperature: number) => {
    calls.push({ user, temperature });
    return Promise.resolve(replies[Math.min(calls.length - 1, replies.length - 1)]);
  };
  return { complete, calls };
}

const range = (start_line: number, end_line: number) => JSON.stringify({ start_line, end_line });

const OLD = [
  "function greet(name) {",
  '  console.log("hello " + name);',
  "  return name.length;",
  "}",
].join("\n");

// The file drifted: indentation and quote style changed.
const DRIFTED = [
  "function greet(name) {",
  "    console.log(\"hello \" + name);",
  "    return name.length;",
  "}",
].join("\n");

// Lines 1..7: header, greet (2-5), const, trailing empty.
const FILE = `// header\n${DRIFTED}\nconst x = 1;\n`;

Deno.test("reconcileEdit splices the replacement at an accepted range", async () => {
  const { complete, calls } = scripted([range(2, 5)]);
  const out = await reconcileEdit(FILE, OLD, "REPLACED", complete);
  assertEquals(out, "// header\nREPLACED\nconst x = 1;\n");
  assertEquals(calls.length, 1);
  assertStringIncludes(calls[0].user, "FAILED SEARCH TEXT:");
  assertStringIncludes(calls[0].user, "2: function greet(name) {");
});

Deno.test("reconcileEdit retries once, then gives up", async () => {
  const { complete, calls } = scripted([
    range(0, 0), // model says no match — resample
    range(6, 6), // 'const x = 1;' — boundary line not in the search text
  ]);
  assertEquals(await reconcileEdit(FILE, OLD, "r", complete), null);
  assertEquals(calls.length, 2);
});

Deno.test("reconcileEdit rejects a range whose boundary drags in unrelated code", async () => {
  // Range 1..5 starts on '// header', which the search text never mentioned.
  const { complete } = scripted([range(1, 5)]);
  assertEquals(await reconcileEdit(FILE, OLD, "r", complete), null);
});

Deno.test("reconcileEdit rejects out-of-bounds and inverted ranges", async () => {
  for (const bad of [range(0, 3), range(5, 2), range(2, 99)]) {
    const { complete } = scripted([bad]);
    assertEquals(await reconcileEdit(FILE, OLD, "r", complete), null);
  }
});

Deno.test("reconcileEdit rejects a region that already equals the replacement", async () => {
  const { complete } = scripted([range(2, 5)]);
  assertEquals(await reconcileEdit(FILE, OLD, DRIFTED, complete), null);
});

Deno.test("reconcileEdit rejects regions outside the length bounds", async () => {
  // A one-line region for a four-line search text: below the /3 floor.
  const file = `function greet(name) {\nconst x = 1;\n`;
  const { complete } = scripted([range(1, 1)]);
  assertEquals(await reconcileEdit(file, OLD, "r", complete), null);
});

Deno.test("reconcileEdit skips tiny search texts without calling the worker", async () => {
  const { complete, calls } = scripted([range(2, 5)]);
  assertEquals(await reconcileEdit(FILE, "zzz", "r", complete), null);
  assertEquals(calls.length, 0);
});

Deno.test("reconcileEdit returns null when the completer throws (worker down)", async () => {
  const out = await reconcileEdit(FILE, OLD, "r", () => Promise.reject(new Error("down")));
  assertEquals(out, null);
});

Deno.test("reconcileEdit returns null on unparseable replies", async () => {
  const { complete } = scripted(["not json at all"]);
  assertEquals(await reconcileEdit(FILE, OLD, "r", complete), null);
});

Deno.test("big files send a numbered window with true line numbers", async () => {
  const padTop = Array.from({ length: 2000 }, (_, i) => `// filler top ${i}`).join("\n");
  const padBot = Array.from({ length: 2000 }, (_, i) => `// filler bottom ${i}`).join("\n");
  const big = `${padTop}\n${DRIFTED}\n${padBot}\n`;
  // greet spans lines 2001..2004 in the full file.
  const { complete, calls } = scripted([range(2001, 2004)]);
  const out = await reconcileEdit(big, OLD, "REPLACED", complete);
  assertEquals(out, `${padTop}\nREPLACED\n${padBot}\n`);
  assertStringIncludes(calls[0].user, "2001: function greet(name) {");
  const fileSection = calls[0].user.split("FAILED SEARCH TEXT:")[0];
  assertEquals(fileSection.length < big.length, true);
});

Deno.test("big files with no anchoring line give up without calling the worker", async () => {
  const big = Array.from({ length: 4000 }, (_, i) => `// filler ${i}`).join("\n");
  const { complete, calls } = scripted([range(1, 4)]);
  assertEquals(await reconcileEdit(big, OLD, "r", complete), null);
  assertEquals(calls.length, 0);
});
