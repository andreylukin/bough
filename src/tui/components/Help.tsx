// The `?` overlay: keybindings rendered from KEYMAP (keys.ts) so the docs are
// always the bindings that actually run.
//
// Layout: sections are laid out in TWO columns on a normal terminal, because a
// single column ran to ~60 rows and nothing about "scroll through five screens
// of keys" is quick reference. Blocks (a header plus its rows) are kept whole
// and split between the columns to balance height, so the whole map fits one
// screen at 100x30. Narrow terminals fall back to one column, and either way
// the list windows behind a scroll offset if it still doesn't fit — Ink's
// clipping of an overflowing background Box merges rows into garbage, so
// overflow must never happen, and truncation dropped real content.
import stringWidth from "string-width";
import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import { KEYMAP } from "../keys.ts";

// Border (2) + paddingX (2) columns around the list; rows are indented 2 more.
const CHROME_W = 4;

// Below this the two columns would leave ~30 usable columns each, which forces
// every description to wrap and costs more rows than one column spends.
const MIN_TWO_COL = 92;

// Gutter between the two columns.
const GAP = 3;

// The keys everyone needs, pinned above the list so they never scroll away —
// quitting used to be the LAST line of a 60-row overlay, i.e. the first thing a
// stuck newcomer looks for was the hardest to find (user-testing).
const ESSENTIALS = "enter send · esc interrupt · ^x ^x stop all · ^c ^c quit";

type Row =
  | { header: string }
  | { key: string; desc: string; muted?: boolean }
  | { cont: string; muted?: boolean } // wrapped continuation of the previous row
  | { prose: string } // a "won't do" line: no key column
  | { blank: true };

/** Greedy word-wrap of `text` into lines of at most `w` columns. */
function wrapText(text: string, w: number): string[] {
  const out: string[] = [];
  let line = "";
  for (const word of text.split(" ")) {
    if (line && stringWidth(line) + 1 + stringWidth(word) > w) {
      out.push(line);
      line = word;
    } else line = line ? `${line} ${word}` : word;
  }
  out.push(line);
  return out;
}

/** Pad to `w` DISPLAY columns — key names are full of ⌘/⌥/⌫, whose code-unit
 * length is not their width, so padEnd misaligns the description gutter. */
function pad(text: string, w: number): string {
  return text + " ".repeat(Math.max(0, w - stringWidth(text)));
}

/** One section as a block of rows, kept whole so a header never splits from its
 * keys across the column break. `keyW` is that block's own key-column width. */
function block(sec: typeof KEYMAP[number], colW: number, keyW: number): Row[] {
  const rows: Row[] = [{ header: sec.section }];
  for (const [key, desc] of sec.keys) {
    if (sec.limits) {
      for (const line of wrapText(desc, colW - 2)) rows.push({ prose: line });
      continue;
    }
    const [first, ...rest] = wrapText(desc, Math.max(8, colW - 2 - keyW));
    rows.push({ key, desc: first, muted: sec.unavailable });
    for (const cont of rest) rows.push({ cont, muted: sec.unavailable });
  }
  return rows;
}

const keyWidth = (sec: typeof KEYMAP[number]): number =>
  Math.max(...sec.keys.map(([k]) => stringWidth(k))) + 2;

/** Columns of rows: one entry per column, each already blank-separated. */
function columns(width: number, spaced: boolean): { cols: Row[][]; colW: number } {
  const two = width >= MIN_TWO_COL;
  const colW = two ? Math.floor((width - CHROME_W - GAP) / 2) : width - CHROME_W;
  // A shared key column across every section wastes the width of the widest
  // ("workflows") on all of them; per-section sizing keeps the gutter tight.
  const blocks = KEYMAP.map((sec) => block(sec, colW, keyWidth(sec)));
  if (!two) {
    const flat: Row[] = [];
    for (const b of blocks) {
      if (spaced && flat.length) flat.push({ blank: true });
      flat.push(...b);
    }
    return { cols: [flat], colW };
  }
  // Blocks stay whole and in order, so a header never splits from its keys.
  // Try every split point and keep the one with the shortest tallest column —
  // the greedy "fill past half" version stranded a whole section below the fold
  // when one block happened to straddle the midpoint.
  const sep = spaced ? 1 : 0;
  const h = (bs: Row[][]) =>
    bs.reduce((n, b) => n + b.length, 0) + Math.max(0, bs.length - 1) * sep;
  let best = 1;
  for (let k = 1; k < blocks.length; k++) {
    const cur = Math.max(h(blocks.slice(0, k)), h(blocks.slice(k)));
    if (cur < Math.max(h(blocks.slice(0, best)), h(blocks.slice(best)))) best = k;
  }
  const build = (bs: Row[][]) => {
    const col: Row[] = [];
    for (const b of bs) {
      if (spaced && col.length) col.push({ blank: true });
      col.push(...b);
    }
    return col;
  };
  return { cols: [build(blocks.slice(0, best)), build(blocks.slice(best))], colW };
}

/** List rows that fit: 2 border rows + the title + the essentials line + the
 * status bar are the chrome around them. */
const availRows = (rows: number): number => Math.max(4, rows - 5);

function layout(rows: number, width: number) {
  const avail = availRows(rows);
  // Section spacing is the first thing to go when it would cost a scroll: a map
  // you can see all of beats a prettier one you have to page through.
  const spaced = columns(width, true);
  const fits = Math.max(...spaced.cols.map((c) => c.length)) <= avail;
  const { cols, colW } = fits ? spaced : columns(width, false);
  const height = Math.max(...cols.map((c) => c.length));
  // Only give up a row to the "↓ N more" hint when there is actually more: the
  // unconditional reserve made a map that exactly filled the screen scroll by
  // one, which is the most annoying amount of scrolling there is.
  const maxScroll = height <= avail ? 0 : height - (avail - 1);
  return { cols, colW, height, maxScroll };
}

/** Furthest the overlay scrolls at this size — App clamps its offset with it. */
export const helpMaxScroll = (rows: number, width: number): number => layout(rows, width).maxScroll;

/** A row's painted content plus its display width, so the next column can be
 * padded to start in the right place. */
function cell(row: Row | undefined, keyW: number): { node: React.ReactNode; w: number } {
  if (!row || "blank" in row) return { node: null, w: 0 };
  if ("header" in row) {
    return {
      node: <Text bold color={palette.accent}>{row.header}</Text>,
      w: stringWidth(row.header),
    };
  }
  if ("prose" in row) {
    return { node: <Text dimColor>{"  " + row.prose}</Text>, w: stringWidth(row.prose) + 2 };
  }
  if ("cont" in row) {
    const text = "  " + " ".repeat(keyW) + row.cont;
    return { node: <Text dimColor={row.muted}>{text}</Text>, w: stringWidth(text) };
  }
  const key = pad(row.key, keyW);
  return {
    node: (
      <>
        {
          /* An unbound chord is dimmed and never bold — it must not read as a
            live binding just because it appears in the key column. */
        }
        <Text bold={!row.muted} dimColor={row.muted}>{"  " + key}</Text>
        <Text dimColor={row.muted}>{row.desc}</Text>
      </>
    ),
    w: 2 + stringWidth(key) + stringWidth(row.desc),
  };
}

/** Each column carries its own key width, recovered from the widest key in it. */
function colKeyWidth(rows: Row[]): number {
  const keys = rows.filter((r): r is { key: string; desc: string } => "key" in r);
  return keys.length ? Math.max(...keys.map((r) => stringWidth(r.key))) + 2 : 0;
}

export function Help(
  { rows = 30, width = 80, scroll = 0 }: { rows?: number; width?: number; scroll?: number },
) {
  const avail = availRows(rows);
  const { cols, colW, height, maxScroll } = layout(rows, width);
  const off = Math.max(0, Math.min(scroll, maxScroll));
  const shownH = maxScroll > 0 ? avail - 1 : height;
  const keyWs = cols.map(colKeyWidth);
  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      backgroundColor={palette.panel}
      borderColor={palette.border}
      paddingX={1}
    >
      <Text wrap="truncate">
        <Text bold>keys</Text>
        <Text dimColor>
          {"  "}
          {maxScroll > 0 ? "j/k or ↑/↓ scroll · any other key closes" : "any key closes"}
        </Text>
      </Text>
      <Text wrap="truncate">{ESSENTIALS}</Text>
      {Array.from({ length: shownH }, (_, i) => {
        const y = off + i;
        const left = cell(cols[0][y], keyWs[0]);
        const right = cols[1] ? cell(cols[1][y], keyWs[1]) : { node: null, w: 0 };
        // A blank left cell still paints one space to keep the row's height, so
        // it costs a column — not counting it pushed every right-hand row whose
        // left neighbour was blank one place out of alignment.
        const leftW = left.node ? left.w : 1;
        return (
          <Text key={y} wrap="truncate">
            {left.node ?? " "}
            {right.node ? " ".repeat(Math.max(1, colW + GAP - leftW)) : null}
            {right.node}
          </Text>
        );
      })}
      {maxScroll > 0
        ? (
          <Text dimColor wrap="truncate">
            {off < maxScroll ? `↓ ${height - off - shownH} more — j/k or ↑/↓ · wheel` : "end — ↑/k"}
          </Text>
        )
        : null}
    </Box>
  );
}
