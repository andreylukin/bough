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
import { test } from "bun:test";
import assert from "node:assert/strict";
import {
  extractSpan,
  highlightSpan,
  isEmptySelection,
  rowSpan,
  selectedText,
  selRows,
  selectedCopy,
} from "./selection.ts";

test("a single-row drag clips both ends and includes the release cell", () => {
  const sel = { anchor: { x: 3, y: 5 }, focus: { x: 7, y: 5 } };
  assert.deepEqual(rowSpan(sel, 5), { from: 2, to: 7 });
  assert.equal(rowSpan(sel, 4), null);
  assert.equal(rowSpan(sel, 6), null);
});

test("a backwards drag normalizes to reading order", () => {
  const sel = { anchor: { x: 7, y: 5 }, focus: { x: 3, y: 5 } };
  assert.deepEqual(rowSpan(sel, 5), { from: 2, to: 7 });

  const up = { anchor: { x: 2, y: 8 }, focus: { x: 6, y: 4 } };
  assert.deepEqual(selRows(up), [4, 8]);
  assert.deepEqual(rowSpan(up, 4), { from: 5, to: Infinity }); // first row: x → EOL
  assert.deepEqual(rowSpan(up, 6), { from: 0, to: Infinity }); // interior: whole line
  assert.deepEqual(rowSpan(up, 8), { from: 0, to: 2 }); // last row: up to x, inclusive
});

test("a single cell selects exactly one column", () => {
  const sel = { anchor: { x: 4, y: 2 }, focus: { x: 4, y: 2 } };
  assert.deepEqual(rowSpan(sel, 2), { from: 3, to: 4 });
  assert.equal(isEmptySelection(sel), true);
  assert.equal(isEmptySelection({ anchor: { x: 4, y: 2 }, focus: { x: 5, y: 2 } }), false);
});

test("extractSpan strips SGR codes and clips by display column", () => {
  const styled = "\x1b[1mhello\x1b[0m \x1b[32mworld\x1b[0m";
  assert.equal(extractSpan(styled, 0, Infinity), "hello world");
  assert.equal(extractSpan(styled, 6, 11), "world");
  assert.equal(extractSpan(styled, 0, 5), "hello");
});

test("extractSpan drops trailing whitespace and tolerates spans past EOL", () => {
  assert.equal(extractSpan("hi   ", 0, Infinity), "hi");
  assert.equal(extractSpan("hi", 0, 80), "hi");
  assert.equal(extractSpan("hi", 5, 10), "");
});

test("highlightSpan wraps the span in inverse video", () => {
  assert.equal(highlightSpan("hello", 1, 3), "h\x1b[7mel\x1b[27mlo");
  assert.equal(highlightSpan("hello", 0, Infinity), "\x1b[7mhello\x1b[27m");
});

test("the selected span loses its own colours; the rest keeps them", () => {
  const styled = "\x1b[32mgreen\x1b[0m plain";
  const out = highlightSpan(styled, 6, 11);
  // Selection reads as one solid band, not as inverse-video syntax highlighting.
  assert.ok(out.includes("\x1b[7mplain\x1b[27m"));
  assert.ok(out.includes("green"));
});

test("an empty span is a no-op rather than a zero-width inverse pair", () => {
  assert.equal(highlightSpan("hi", 5, 10), "hi");
});

test("selectedText joins the rows the drag covered, clipped at both ends", () => {
  const rows = ["first line here", "\x1b[1msecond\x1b[0m line", "third line"];
  const at = (y: number) => rows[y - 1] ?? null;
  const sel = { anchor: { x: 7, y: 1 }, focus: { x: 5, y: 3 } };
  assert.equal(selectedText(sel, at), "line here\nsecond line\nthird");
});

test("a row that shows nothing contributes a blank line, not a skipped one", () => {
  // Padding above a short transcript: the gap is part of what was dragged over.
  const at = (y: number) => (y === 2 ? null : `row ${y}`);
  const sel = { anchor: { x: 1, y: 1 }, focus: { x: 5, y: 3 } };
  assert.equal(selectedText(sel, at), "row 1\n\nrow 3");
});

test("a drag ending on empty padding does not paste trailing blank lines", () => {
  const at = (y: number) => (y <= 2 ? `row ${y}` : null);
  const sel = { anchor: { x: 1, y: 1 }, focus: { x: 9, y: 5 } };
  assert.equal(selectedText(sel, at), "row 1\nrow 2");
});

// ---- what the clipboard actually gets ---------------------------------------
// `selectedText` answers "what is on those cells". That is right for one row and
// wrong across a wrap: the window's line breaks are not the text's, and bough's
// `│` block gutter is chrome. `selectedCopy` answers from the source instead.

const row = (text: string, src?: string) => (src === undefined ? { text } : { text, src });

test("a copy across a wrap rejoins the line and drops the gutter", () => {
  // Two screen rows, one source line — exactly what a wrapped code block is.
  const rows = [
    row("  │ const x = await bash(\"a very long", "const x = await bash(\"a very long command\");"),
    row("  │  command\");", "const x = await bash(\"a very long command\");"),
  ];
  const out = selectedCopy(
    { anchor: { x: 1, y: 1 }, focus: { x: 40, y: 2 } },
    (y) => rows[y - 1] ?? null,
  );
  assert.equal(out, "const x = await bash(\"a very long command\");");
  assert.equal(out.includes("│"), false, "the gutter is chrome, not content");
  assert.equal(out.includes("\n"), false, "one source line pastes as one line");
});

// One paragraph, wrapped across three rows — what `push()` produces for prose, and
// the shape the source substitution was over-reaching on.
const PARA = "the quick brown fox jumps over the lazy dog and keeps going";
const paraRows = [
  row("the quick brown fox jumps", PARA),
  row("over the lazy dog and", PARA),
  row("keeps going", PARA),
];

test("a drag INSIDE a wrapped line copies the drag, not the whole line", () => {
  // The bug, reported from a real terminal: selecting a phrase in a message pasted
  // the entire message. Every wrapped row carries the whole logical line as its
  // source, so answering any two-row drag from the source hands back the paragraph.
  const out = selectedCopy(
    { anchor: { x: 5, y: 1 }, focus: { x: 9, y: 2 } },
    (y) => paraRows[y - 1] ?? null,
  );
  assert.equal(out, "quick brown fox jumps\nover the");
  assert.equal(out.includes("keeps going"), false, "a row the drag never reached");
  assert.equal(out.includes("the quick"), false, "text before where the drag started");
});

test("a drag that starts mid-paragraph does not reach back for the rows above", () => {
  // Rows 2-3 covered edge to edge — but row 1 is the same source and outside the
  // drag, so the source is not the answer to this selection either.
  const out = selectedCopy(
    { anchor: { x: 1, y: 2 }, focus: { x: 40, y: 3 } },
    (y) => paraRows[y - 1] ?? null,
  );
  assert.equal(out, "over the lazy dog and\nkeeps going");
  assert.equal(out.includes("quick brown"), false, "row 1 was never selected");
});

test("a drag holding EVERY row of a source still rejoins it", () => {
  // The behaviour the clipping must not cost: the whole paragraph, selected whole,
  // still pastes as one un-wrapped line rather than as the terminal's three.
  assert.equal(
    selectedCopy(
      { anchor: { x: 1, y: 1 }, focus: { x: 40, y: 3 } },
      (y) => paraRows[y - 1] ?? null,
    ),
    PARA,
  );
});

test("distinct source lines stay on distinct lines", () => {
  // The inverse guard: dedupe must not collapse two real lines into one.
  const rows = [row("  │ first()", "first()"), row("  │ second()", "second()")];
  assert.equal(
    selectedCopy({ anchor: { x: 1, y: 1 }, focus: { x: 20, y: 2 } }, (y) => rows[y - 1] ?? null),
    "first()\nsecond()",
  );
});

test("a row with no source falls back to its cells, minus the gutter", () => {
  // The panel, the rail and the composer have no `src` — only what is painted.
  const rows = [row("  │ painted only"), row("plain row")];
  assert.equal(
    selectedCopy({ anchor: { x: 1, y: 1 }, focus: { x: 20, y: 2 } }, (y) => rows[y - 1] ?? null),
    "  painted only\nplain row",
  );
});

test("a single-row drag stays EXACT — the span, not the source line", () => {
  // Dragging across part of one line means that part. Yielding the whole source
  // would make a deliberate partial selection impossible to express.
  const rows = [row("hello wide world", "hello wide world and much more beyond the edge")];
  assert.equal(
    selectedCopy({ anchor: { x: 7, y: 1 }, focus: { x: 10, y: 1 } }, (y) => rows[y - 1] ?? null),
    "wide",
  );
});

test("a gap the selection crossed pastes as a blank line", () => {
  const rows: ({ text: string; src?: string } | null)[] = [row("top", "top"), null, row("bottom", "bottom")];
  assert.equal(
    selectedCopy({ anchor: { x: 1, y: 1 }, focus: { x: 10, y: 3 } }, (y) => rows[y - 1] ?? null),
    "top\n\nbottom",
  );
});

test("a highlighted source is stripped — no escape bytes reach the clipboard", () => {
  // `src` is the line as LAID OUT, and a code block is syntax-highlighted before it
  // is wrapped. Emitting it verbatim put raw SGR in the buffer — caught by driving
  // a real copy, not by this file, which is why it is pinned here now.
  const styled = "\x1b[38;5;140mconst\x1b[39m x = 1";
  const rows = [
    { text: "  \x1b[2m│\x1b[22m " + styled, src: styled },
    { text: "  next", src: "next" },
  ];
  const out = selectedCopy(
    { anchor: { x: 1, y: 1 }, focus: { x: 30, y: 2 } },
    (y) => rows[y - 1] ?? null,
  );
  assert.equal(out, "const x = 1\nnext");
  assert.equal(/\x1b/.test(out), false, `escapes leaked: ${JSON.stringify(out)}`);
});

test("a fence contributes nothing — opener, label and closer alike", () => {
  // `╭ code` / `╰` frame a raised block. Neither is content: the opener's label
  // names the language to a reader and is a stray word in a paste, and a trailing
  // `╰` pasting as a blank line is the tell that the chrome was only half-removed.
  const rows = [
    { text: "  ╭ code", src: "╭ code" },
    { text: "  │ body()", src: "body()" },
    { text: "  ╰", src: "╰" },
  ];
  assert.equal(
    selectedCopy({ anchor: { x: 1, y: 1 }, focus: { x: 20, y: 3 } }, (y) => rows[y - 1] ?? null),
    "body()",
  );
});
