/**
 * Drag selection over the transcript viewport: screen cells in, column spans and
 * plain text out.
 *
 * THE INVARIANT THIS HOLDS: **selection is arithmetic on display columns, never on
 * string indices.** Every transcript row can carry SGR colour, an OSC 8 hyperlink
 * or a wide CJK glyph, so `text[i]` and "the cell under the mouse" are different
 * things — slicing by index would highlight the wrong run and copy escape bytes
 * into the user's clipboard. `slice-ansi` does the column arithmetic and
 * `strip-ansi` does the extraction, and everything in this file is a pure function
 * of `(selection, row, text)` with no terminal, no clock and no React (plan §7).
 *
 * SECOND — **a selection is normalized, never assumed forward.** Dragging up and
 * left is as ordinary as dragging down and right, and every export here reads
 * through `ordered()`, so no caller has to remember which of anchor/focus came
 * first.
 *
 * THIRD — **the release cell is inside the selection.** A terminal includes the
 * cell you let go on; excluding it makes a one-character selection impossible to
 * express and a careful drag always one short.
 *
 * The highlighted span deliberately DROPS its own colours: selection reads as one
 * solid inverse band, as it does in a real terminal, rather than as inverse-video
 * syntax highlighting.
 */
import sliceAnsi from "slice-ansi";
import stripAnsi from "strip-ansi";

/** A 1-based terminal cell, as a mouse report gives it. */
export interface Point {
  x: number;
  y: number;
}

export interface Selection {
  anchor: Point;
  focus: Point;
}

/** Anchor/focus in reading order (top-left first). */
function ordered({ anchor, focus }: Selection): [Point, Point] {
  return anchor.y < focus.y || (anchor.y === focus.y && anchor.x <= focus.x)
    ? [anchor, focus]
    : [focus, anchor];
}

/** The inclusive screen-row range the selection covers, normalized. */
export function selRows(sel: Selection): [number, number] {
  const [a, b] = ordered(sel);
  return [a.y, b.y];
}

/** Does the selection cover more than a single cell? A click is not a drag. */
export function isEmptySelection(sel: Selection): boolean {
  return sel.anchor.x === sel.focus.x && sel.anchor.y === sel.focus.y;
}

/**
 * The selected column span on screen row `y` as 0-based `[from, to)` display
 * columns (`to === Infinity` means "to end of line"), or null when the row falls
 * outside the selection. Interior rows take the whole line; the end rows clip to
 * the drag's cells.
 */
export function rowSpan(sel: Selection, y: number): { from: number; to: number } | null {
  const [a, b] = ordered(sel);
  if (y < a.y || y > b.y) return null;
  const from = y === a.y ? a.x - 1 : 0;
  const to = y === b.y ? b.x : Infinity;
  return from < to ? { from, to } : null;
}

/**
 * `text` (which may carry SGR codes) with display columns `[from, to)` in inverse
 * video, the highlighted run stripped of its own colours.
 */
export function highlightSpan(text: string, from: number, to: number): string {
  const before = sliceAnsi(text, 0, from);
  const mid = stripAnsi(to === Infinity ? sliceAnsi(text, from) : sliceAnsi(text, from, to));
  // Nothing under the span (a drag past end-of-line): leave the row alone rather
  // than emit a zero-width inverse pair that some terminals render as a blip.
  if (!mid) return text;
  const after = to === Infinity ? "" : sliceAnsi(text, to);
  return `${before}\x1b[7m${mid}\x1b[27m${after}`;
}

/** The plain text of display columns `[from, to)`, trailing whitespace dropped. */
export function extractSpan(text: string, from: number, to: number): string {
  return stripAnsi(to === Infinity ? sliceAnsi(text, from) : sliceAnsi(text, from, to)).trimEnd();
}

/**
 * Does this span cover everything the row actually SHOWS?
 *
 * Both edges are measured against the content, not the cells: the left one against
 * the chrome `rowContent` strips (a drag from column 0 and one from just after the
 * `│` gutter both hold the whole line), the right one against the painted width
 * with its trailing padding gone (a drag that ran past the end of a short row holds
 * it, and the bottom row of a drag never reports `Infinity`).
 */
function coversRow(text: string, span: { from: number; to: number }): boolean {
  const { offset } = rowContent(text);
  if (span.from > offset) return false;
  if (span.to === Infinity) return true;
  return span.to >= stripAnsi(text).replace(RIGHT_BORDER, "").trimEnd().length;
}

/** What a row can offer a copy: what is painted, and the raw source behind it. */
export interface CopyRow {
  /** The styled row as it appears on screen. */
  text: string;
  /** The unwrapped source this row was laid out from, when the caller knows it. */
  src?: string;
}

/**
 * Leading block chrome: the `│` gutter and the `╭`/`╰` fence a raised block is
 * drawn in. None of it was in the source, and all of it is worse than useless in a
 * paste — `│ const x = 1` does not run.
 */
const CHROME = /^(\s*)[│╭╰]\s?/;

/**
 * The panel's RIGHT border, with the padding that reaches it.
 *
 * The left one was stripped from the start and this one was not, so every row
 * copied out of a panel ended in a stray `│` — most visibly on the mcp tab's
 * authorization URL, which is the one thing there anybody copies.
 */
const RIGHT_BORDER = /\s*[│╮╯]\s*$/;

/**
 * A fence row: `╭ ts` opening a block or `╰` closing it.
 *
 * Both are chrome for the whole of their width — the opener's label included. It
 * names the block's language to a READER; pasted into a file it is a stray word on
 * a line of its own, which is worse than not saying it.
 */
const FENCE_ONLY = /^\s*[╭╰]/;

/**
 * A painted row reduced to its content: no border either side, no padding.
 *
 * `offset` is how many columns the strip removed from the LEFT, so a caller
 * holding a mouse column can translate it into this string. Without it a click in
 * a panel would hit-test one or two characters off, which on a URL is the
 * difference between opening it and opening nothing.
 */
export function rowContent(text: string): { content: string; offset: number } {
  const plain = stripAnsi(text).replace(RIGHT_BORDER, "");
  const body = plain.replace(CHROME, "$1");
  return { content: body.trimEnd(), offset: plain.length - body.length };
}

/**
 * A source line as it should reach the clipboard.
 *
 * `src` is the line the row was LAID OUT from, which is not the same as the line
 * the user wrote: a code block is syntax-highlighted before it is wrapped, so the
 * source still carries SGR. Pasting that puts escape bytes in the buffer — the
 * exact failure `extractSpan` exists to avoid on the other path.
 */
function cleanSource(src: string): string | null {
  const lines = stripAnsi(src).split("\n");
  const out: string[] = [];
  let droppedChrome = false;
  for (const line of lines) {
    // A fence is chrome down to its last cell and pastes as a stray glyph or a
    // phantom blank line. A genuinely EMPTY source line is not the same thing and
    // survives, which is why this tests for the fence rather than for emptiness.
    if (FENCE_ONLY.test(line)) {
      droppedChrome = true;
      continue;
    }
    out.push(line.replace(CHROME, "$1").trimEnd());
  }
  while (out.length && out[out.length - 1] === "") out.pop();
  // NOTHING TO CONTRIBUTE vs A BLANK LINE. A source that was all fence yields
  // null and is skipped; a source that was genuinely empty yields "" and pastes
  // as the blank line it is.
  if (out.length === 0) return droppedChrome ? null : "";
  return out.join("\n");
}

/**
 * The selection as text worth pasting.
 *
 * DIFFERENT FROM `selectedText`, deliberately. That one answers "what is on those
 * cells", which is right for a single-row drag and wrong the moment a selection
 * crosses a wrap: the window's line breaks are not the text's, so a copied code
 * block came out broken at the column the terminal happened to be, with bough's
 * `│` block gutter down the left of every continuation row. Neither was ever in
 * the source.
 *
 * So a MULTI-ROW selection is answered from the source instead. Rows carry the raw
 * line they were wrapped from (`VLine.src`), and a run of consecutive rows sharing
 * one source yields that source ONCE — which is what un-wraps the block and drops
 * the gutter in the same step. A row with no source falls back to its cells, minus
 * the gutter.
 *
 * ONLY WHEN THE DRAG COVERS THAT WHOLE SOURCE, and this is the part the first
 * version got wrong. `push()` gives every wrapped row the whole logical line as its
 * source, so a paragraph is one source across five rows — and answering any
 * two-row drag inside it from the source pasted the ENTIRE paragraph. Selecting a
 * phrase and getting the message back is not a copy, it is a different feature.
 *
 * A source is therefore substituted only when the selection holds all of it: every
 * one of its rows spanned edge to edge, and no row of it left outside the drag
 * (checked by looking one row past each end for the same source). Anything else is
 * answered from the cells the user actually dragged over — which keeps the window's
 * line breaks, exactly as every terminal's own selection does.
 *
 * A single-row selection stays exact for the same reason: dragging across part of
 * one line means that part, not the paragraph it belongs to.
 */
export function selectedCopy(
  sel: Selection,
  rowAt: (y: number) => CopyRow | null,
): string {
  const [top, bottom] = selRows(sel);
  if (top === bottom) {
    const span = rowSpan(sel, top);
    const row = rowAt(top);
    if (!span || !row) return "";
    return extractSpan(row.text, span.from, span.to).replace(RIGHT_BORDER, "").replace(
      CHROME,
      "$1",
    );
  }
  // The sources this drag does NOT hold in full. Two ways to fail: a row of the
  // source that the drag clipped, or a row of it that the drag never reached —
  // which is why the rows one past each end are consulted.
  const clipped = new Set<string>();
  const edge = (inside: number, outside: number) => {
    const row = rowAt(inside);
    if (row?.src !== undefined && rowAt(outside)?.src === row.src) clipped.add(row.src);
  };
  edge(top, top - 1);
  edge(bottom, bottom + 1);
  for (let y = top; y <= bottom; y++) {
    const span = rowSpan(sel, y);
    const row = rowAt(y);
    if (span && row?.src !== undefined && !coversRow(row.text, span)) clipped.add(row.src);
  }

  const out: string[] = [];
  let lastSource: string | null = null;
  for (let y = top; y <= bottom; y++) {
    const span = rowSpan(sel, y);
    if (!span) continue;
    const row = rowAt(y);
    if (row === null) {
      // A gap the selection crossed — padding above a short transcript. It pastes
      // as a blank line, because a selection that spans a gap should keep it.
      out.push("");
      lastSource = null;
      continue;
    }
    if (row.src !== undefined && !clipped.has(row.src)) {
      // One source, however many rows it was wrapped across.
      if (row.src !== lastSource) {
        const clean = cleanSource(row.src);
        if (clean !== null) out.push(clean);
      }
      lastSource = row.src;
      continue;
    }
    lastSource = null;
    // No source to consult — the panel, the rail, the composer. A row that is
    // nothing but chrome is still dropped.
    if (FENCE_ONLY.test(stripAnsi(row.text))) continue;
    out.push(
      extractSpan(row.text, span.from, span.to).replace(RIGHT_BORDER, "").replace(CHROME, "$1"),
    );
  }
  return out.join("\n");
}

/**
 * The whole selection as the CELLS hold it, one line per screen row.
 *
 * `rowAt` maps a screen row to the styled line rendered there, returning null for a
 * row that shows nothing. Those rows contribute an empty line rather than being
 * skipped, because a selection that spans a gap should paste with the gap in it.
 *
 * `selectedCopy` is what the clipboard gets; this is the literal reading, kept for
 * callers that want exactly what is on screen.
 */
export function selectedText(
  sel: Selection,
  rowAt: (y: number) => string | null,
): string {
  const [top, bottom] = selRows(sel);
  const out: string[] = [];
  for (let y = top; y <= bottom; y++) {
    const span = rowSpan(sel, y);
    if (!span) continue;
    const line = rowAt(y);
    out.push(line === null ? "" : extractSpan(line, span.from, span.to));
  }
  // A drag that ends on empty padding should not paste trailing blank lines.
  while (out.length > 1 && out[out.length - 1] === "") out.pop();
  return out.join("\n");
}
