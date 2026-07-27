/**
 * Tests for drag selection.
 *
 * Everything here is arithmetic on DISPLAY COLUMNS over strings that carry SGR
 * escapes, which is the only reason this module exists: `text.slice()` on a
 * coloured transcript row highlights the wrong run and copies escape bytes into
 * the clipboard. So the fixtures are styled on purpose, and the assertions are on
 * the plain text that comes back out.
 *
 * No terminal is involved, and none is needed — that is the task's acceptance
 * criterion. `node:assert/strict`, because jsr.io is unreachable here.
 */
import assert from "node:assert/strict";
import {
  extractSpan,
  highlightSpan,
  isEmptySelection,
  rowSpan,
  selectedText,
  selRows,
} from "./selection.ts";

Deno.test("a single-row drag clips both ends and includes the release cell", () => {
  const sel = { anchor: { x: 3, y: 5 }, focus: { x: 7, y: 5 } };
  assert.deepEqual(rowSpan(sel, 5), { from: 2, to: 7 });
  assert.equal(rowSpan(sel, 4), null);
  assert.equal(rowSpan(sel, 6), null);
});

Deno.test("a backwards drag normalizes to reading order", () => {
  const sel = { anchor: { x: 7, y: 5 }, focus: { x: 3, y: 5 } };
  assert.deepEqual(rowSpan(sel, 5), { from: 2, to: 7 });

  const up = { anchor: { x: 2, y: 8 }, focus: { x: 6, y: 4 } };
  assert.deepEqual(selRows(up), [4, 8]);
  assert.deepEqual(rowSpan(up, 4), { from: 5, to: Infinity }); // first row: x → EOL
  assert.deepEqual(rowSpan(up, 6), { from: 0, to: Infinity }); // interior: whole line
  assert.deepEqual(rowSpan(up, 8), { from: 0, to: 2 }); // last row: up to x, inclusive
});

Deno.test("a single cell selects exactly one column", () => {
  const sel = { anchor: { x: 4, y: 2 }, focus: { x: 4, y: 2 } };
  assert.deepEqual(rowSpan(sel, 2), { from: 3, to: 4 });
  assert.equal(isEmptySelection(sel), true);
  assert.equal(isEmptySelection({ anchor: { x: 4, y: 2 }, focus: { x: 5, y: 2 } }), false);
});

Deno.test("extractSpan strips SGR codes and clips by display column", () => {
  const styled = "\x1b[1mhello\x1b[0m \x1b[32mworld\x1b[0m";
  assert.equal(extractSpan(styled, 0, Infinity), "hello world");
  assert.equal(extractSpan(styled, 6, 11), "world");
  assert.equal(extractSpan(styled, 0, 5), "hello");
});

Deno.test("extractSpan drops trailing whitespace and tolerates spans past EOL", () => {
  assert.equal(extractSpan("hi   ", 0, Infinity), "hi");
  assert.equal(extractSpan("hi", 0, 80), "hi");
  assert.equal(extractSpan("hi", 5, 10), "");
});

Deno.test("highlightSpan wraps the span in inverse video", () => {
  assert.equal(highlightSpan("hello", 1, 3), "h\x1b[7mel\x1b[27mlo");
  assert.equal(highlightSpan("hello", 0, Infinity), "\x1b[7mhello\x1b[27m");
});

Deno.test("the selected span loses its own colours; the rest keeps them", () => {
  const styled = "\x1b[32mgreen\x1b[0m plain";
  const out = highlightSpan(styled, 6, 11);
  // Selection reads as one solid band, not as inverse-video syntax highlighting.
  assert.ok(out.includes("\x1b[7mplain\x1b[27m"));
  assert.ok(out.includes("green"));
});

Deno.test("an empty span is a no-op rather than a zero-width inverse pair", () => {
  assert.equal(highlightSpan("hi", 5, 10), "hi");
});

Deno.test("selectedText joins the rows the drag covered, clipped at both ends", () => {
  const rows = ["first line here", "\x1b[1msecond\x1b[0m line", "third line"];
  const at = (y: number) => rows[y - 1] ?? null;
  const sel = { anchor: { x: 7, y: 1 }, focus: { x: 5, y: 3 } };
  assert.equal(selectedText(sel, at), "line here\nsecond line\nthird");
});

Deno.test("a row that shows nothing contributes a blank line, not a skipped one", () => {
  // Padding above a short transcript: the gap is part of what was dragged over.
  const at = (y: number) => (y === 2 ? null : `row ${y}`);
  const sel = { anchor: { x: 1, y: 1 }, focus: { x: 5, y: 3 } };
  assert.equal(selectedText(sel, at), "row 1\n\nrow 3");
});

Deno.test("a drag ending on empty padding does not paste trailing blank lines", () => {
  const at = (y: number) => (y <= 2 ? `row ${y}` : null);
  const sel = { anchor: { x: 1, y: 1 }, focus: { x: 9, y: 5 } };
  assert.equal(selectedText(sel, at), "row 1\nrow 2");
});
