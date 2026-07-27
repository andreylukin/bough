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
 * The whole selection as clipboard text.
 *
 * `rowAt` maps a screen row to the styled line rendered there, returning null for
 * a row that shows nothing — padding above a short transcript, a row past the last
 * line. Those rows contribute an empty line rather than being skipped, because a
 * selection that spans a gap should paste with the gap in it.
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
