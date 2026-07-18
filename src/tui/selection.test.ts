import { assertEquals } from "jsr:@std/assert@1";
import { extractSpan, highlightSpan, rowSpan, selRows } from "./selection.ts";

Deno.test("rowSpan: single-row drag clips both ends, release cell included", () => {
  const sel = { anchor: { x: 3, y: 5 }, focus: { x: 7, y: 5 } };
  assertEquals(rowSpan(sel, 5), { from: 2, to: 7 });
  assertEquals(rowSpan(sel, 4), null);
  assertEquals(rowSpan(sel, 6), null);
});

Deno.test("rowSpan: backwards drag normalizes to reading order", () => {
  const sel = { anchor: { x: 7, y: 5 }, focus: { x: 3, y: 5 } };
  assertEquals(rowSpan(sel, 5), { from: 2, to: 7 });
  const up = { anchor: { x: 2, y: 8 }, focus: { x: 6, y: 4 } };
  assertEquals(selRows(up), [4, 8]);
  assertEquals(rowSpan(up, 4), { from: 5, to: Infinity }); // first row: from x to EOL
  assertEquals(rowSpan(up, 6), { from: 0, to: Infinity }); // interior: whole line
  assertEquals(rowSpan(up, 8), { from: 0, to: 2 }); // last row: up to x inclusive
});

Deno.test("rowSpan: single cell selects one column", () => {
  const sel = { anchor: { x: 4, y: 2 }, focus: { x: 4, y: 2 } };
  assertEquals(rowSpan(sel, 2), { from: 3, to: 4 });
});

Deno.test("extractSpan: strips SGR codes and clips display columns", () => {
  const styled = "\x1b[1mhello\x1b[0m \x1b[32mworld\x1b[0m";
  assertEquals(extractSpan(styled, 0, Infinity), "hello world");
  assertEquals(extractSpan(styled, 6, 11), "world");
  assertEquals(extractSpan(styled, 0, 5), "hello");
});

Deno.test("extractSpan: drops trailing whitespace, tolerates spans past EOL", () => {
  assertEquals(extractSpan("hi   ", 0, Infinity), "hi");
  assertEquals(extractSpan("hi", 0, 80), "hi");
  assertEquals(extractSpan("hi", 5, 10), "");
});

Deno.test("highlightSpan: wraps the span in inverse video, plain text", () => {
  assertEquals(highlightSpan("hello", 1, 3), "h\x1b[7mel\x1b[27mlo");
  assertEquals(highlightSpan("hello", 0, Infinity), "\x1b[7mhello\x1b[27m");
});

Deno.test("highlightSpan: selected span loses its own colors, rest keeps them", () => {
  const styled = "\x1b[32mgreen\x1b[0m plain";
  const out = highlightSpan(styled, 6, 11);
  // The highlighted "plain" carries no color codes, only inverse.
  assertEquals(out.includes("\x1b[7mplain\x1b[27m"), true);
  assertEquals(out.includes("green"), true);
});

Deno.test("highlightSpan: empty span is a no-op", () => {
  assertEquals(highlightSpan("hi", 5, 10), "hi");
});
