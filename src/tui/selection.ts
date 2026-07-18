// Mouse drag selection over the conversation viewport. Pure helpers: screen-cell
// anchor/focus → per-row column spans, inverse-video highlight of a styled line,
// and plain-text extraction of the selected region for the clipboard.
import sliceAnsi from "slice-ansi";
import stripAnsi from "strip-ansi";

/** 1-based terminal cell. */
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

export function selRows(sel: Selection): [number, number] {
  const [a, b] = ordered(sel);
  return [a.y, b.y];
}

/**
 * The selected column span on screen row `y` as 0-based [from, to) display
 * columns (`to` = Infinity for "to end of line"), or null when the row is
 * outside the selection. Interior rows select whole lines; the end rows clip
 * to the drag's cells (the release cell is included, like a terminal).
 */
export function rowSpan(sel: Selection, y: number): { from: number; to: number } | null {
  const [a, b] = ordered(sel);
  if (y < a.y || y > b.y) return null;
  const from = y === a.y ? a.x - 1 : 0;
  const to = y === b.y ? b.x : Infinity;
  return from < to ? { from, to } : null;
}

/**
 * `text` (may carry SGR codes) with display columns [from, to) shown in
 * inverse video. The highlighted span drops its own colors — selection reads
 * as one solid band, like a real terminal's.
 */
export function highlightSpan(text: string, from: number, to: number): string {
  const before = sliceAnsi(text, 0, from);
  const mid = stripAnsi(to === Infinity ? sliceAnsi(text, from) : sliceAnsi(text, from, to));
  if (!mid) return text;
  const after = to === Infinity ? "" : sliceAnsi(text, to);
  return `${before}\x1b[7m${mid}\x1b[27m${after}`;
}

/** The plain text of display columns [from, to), trailing whitespace dropped. */
export function extractSpan(text: string, from: number, to: number): string {
  return stripAnsi(to === Infinity ? sliceAnsi(text, from) : sliceAnsi(text, from, to)).trimEnd();
}
