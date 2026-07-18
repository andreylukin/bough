import { assertEquals } from "jsr:@std/assert@1";
import stripAnsi from "strip-ansi";
import { findMatches, markLine, markSpan } from "./search.ts";

Deno.test("findMatches: case-insensitive, across styled lines, multiple per line", () => {
  const lines = [
    { text: "plain Foo here" },
    { text: "\x1b[2mfoo and FOO again\x1b[0m" },
    { text: "nothing" },
  ];
  const m = findMatches(lines, "foo");
  assertEquals(m, [
    { line: 0, col: 6, len: 3 },
    { line: 1, col: 0, len: 3 },
    { line: 1, col: 8, len: 3 },
  ]);
});

Deno.test("findMatches: empty query matches nothing", () => {
  assertEquals(findMatches([{ text: "abc" }], ""), []);
});

Deno.test("markSpan: current match is inverse and colorless", () => {
  const marked = markSpan("say \x1b[36mhello\x1b[39m there", 4, 9, true);
  assertEquals(marked.includes("\x1b[7mhello\x1b[27m"), true);
  assertEquals(stripAnsi(marked), "say hello there");
});

Deno.test("markSpan: other matches underline, keeping their colors", () => {
  const marked = markSpan("say \x1b[36mhello\x1b[39m there", 4, 9, false);
  assertEquals(marked.includes("\x1b[4m"), true);
  assertEquals(marked.includes("\x1b[36m"), true);
  assertEquals(stripAnsi(marked), "say hello there");
});

Deno.test("markLine: marks every span on the line, current one inverse", () => {
  const matches = findMatches([{ text: "foo bar foo" }], "foo");
  const marked = markLine("foo bar foo", matches, 0, 1);
  // First foo underlined, second (current) inverse; text unchanged when stripped.
  assertEquals(marked, "\x1b[4mfoo\x1b[24m bar \x1b[7mfoo\x1b[27m");
});
