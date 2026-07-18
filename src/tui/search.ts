// Transcript search (^s): pure helpers over the pre-wrapped viewport lines.
// Matching is case-insensitive over the ANSI-stripped text; marks re-style the
// SGR line by display column (same slice-ansi discipline as selection.ts).
import sliceAnsi from "slice-ansi";
import stripAnsi from "strip-ansi";

export interface SearchMatch {
  /** Index into the lines array. */
  line: number;
  /** 0-based display column where the match starts. */
  col: number;
  len: number;
}

/** All matches of `query` across the lines, top to bottom, left to right. */
export function findMatches(lines: { text: string }[], query: string): SearchMatch[] {
  if (!query) return [];
  const q = query.toLowerCase();
  const out: SearchMatch[] = [];
  for (let i = 0; i < lines.length; i++) {
    const plain = stripAnsi(lines[i].text).toLowerCase();
    let at = plain.indexOf(q);
    while (at >= 0) {
      out.push({ line: i, col: at, len: q.length });
      at = plain.indexOf(q, at + q.length);
    }
  }
  return out;
}

/**
 * `text` with one match span re-styled: the current match reads inverse (a
 * solid band, own colors dropped), other matches underline in place. Spans on
 * one line must be marked right-to-left so earlier columns stay valid — the
 * inserted codes are zero-width but sliceAnsi's slicing is cheapest unshifted.
 */
export function markSpan(text: string, from: number, to: number, current: boolean): string {
  const before = sliceAnsi(text, 0, from);
  const midRaw = sliceAnsi(text, from, to);
  if (!stripAnsi(midRaw)) return text;
  const after = sliceAnsi(text, to);
  const mid = current ? `\x1b[7m${stripAnsi(midRaw)}\x1b[27m` : `\x1b[4m${midRaw}\x1b[24m`;
  return `${before}${mid}${after}`;
}

/** Mark every match on one rendered line; `currentIdx` is the match index (into
 * `matches`) that reads as the current one. */
export function markLine(
  text: string,
  matches: SearchMatch[],
  lineIdx: number,
  currentIdx: number,
): string {
  let out = text;
  // Right-to-left so a mark never shifts the columns of the spans before it.
  for (let i = matches.length - 1; i >= 0; i--) {
    const m = matches[i];
    if (m.line !== lineIdx) continue;
    out = markSpan(out, m.col, m.col + m.len, i === currentIdx);
  }
  return out;
}
