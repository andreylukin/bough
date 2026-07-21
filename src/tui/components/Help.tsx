// The `?` overlay: keybindings rendered from KEYMAP (keys.ts) so the docs are
// always the bindings that actually run. Long descriptions pre-wrap to the
// terminal width (one Row per painted line) and the list windows behind a
// scroll offset when it's taller than the screen — Ink's clipping of an
// overflowing background Box merges rows into garbage, so overflow must never
// happen, and truncation dropped real content (the glyph legend lost its tail).
import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import { KEYMAP } from "../keys.ts";

// Key column sized to the longest binding so descriptions always keep a gutter.
const PAD = Math.max(...KEYMAP.flatMap((s) => s.keys.map(([k]) => k.length))) + 2;

// Border (2) + paddingX (2) columns around the list; rows are indented 2 more.
const CHROME_W = 4;

// One plain-language line beginners can act on before reading any bindings.
const INTRO = "type a message and press enter to talk to bough";

type Row =
  | { header: string }
  | { key: string; desc: string }
  | { cont: string } // wrapped continuation of the previous row's description
  | { blank: true };

/** Greedy word-wrap of `text` into lines of at most `w` columns. */
function wrapText(text: string, w: number): string[] {
  const out: string[] = [];
  let line = "";
  for (const word of text.split(" ")) {
    if (line && line.length + 1 + word.length > w) {
      out.push(line);
      line = word;
    } else line = line ? `${line} ${word}` : word;
  }
  out.push(line);
  return out;
}

function displayRows(width: number, spaced: boolean): Row[] {
  const descW = Math.max(8, width - CHROME_W - 2 - PAD);
  const rows: Row[] = [];
  for (const sec of KEYMAP) {
    if (spaced && rows.length > 0) rows.push({ blank: true });
    rows.push({ header: sec.section });
    for (const [key, desc] of sec.keys) {
      const [first, ...rest] = wrapText(desc, descW);
      rows.push({ key, desc: first });
      for (const cont of rest) rows.push({ cont });
    }
  }
  return rows;
}

/** List rows that fit inside the overlay: 2 border rows + the "keys" title +
 * the intro line + the status bar are the chrome around them. */
const availRows = (rows: number): number => Math.max(4, rows - 5);

/** The list at this size (section spacing dropped when tight) + how far it can
 * scroll — 0 means everything fits; otherwise one list row goes to the hint. */
function layout(rows: number, width: number): { list: Row[]; maxScroll: number } {
  const avail = availRows(rows);
  const spaced = displayRows(width, true);
  const list = spaced.length <= avail ? spaced : displayRows(width, false);
  return { list, maxScroll: Math.max(0, list.length - (avail - 1)) };
}

/** Furthest the overlay scrolls at this size — App clamps its offset with it. */
export const helpMaxScroll = (rows: number, width: number): number =>
  layout(rows, width).maxScroll;

function Line({ row }: { row: Row }) {
  if ("blank" in row) return <Text>{" "}</Text>;
  if ("header" in row) return <Text bold wrap="truncate">{row.header}</Text>;
  if ("cont" in row) return <Text wrap="truncate">{"  " + " ".repeat(PAD) + row.cont}</Text>;
  return (
    <Text wrap="truncate">
      {"  "}
      <Text color={palette.accent}>{row.key.padEnd(PAD)}</Text>
      {row.desc}
    </Text>
  );
}

export function Help(
  { rows = 30, width = 80, scroll = 0 }: { rows?: number; width?: number; scroll?: number },
) {
  const avail = availRows(rows);
  const { list, maxScroll } = layout(rows, width);
  const off = Math.max(0, Math.min(scroll, maxScroll));
  const shown = maxScroll > 0 ? list.slice(off, off + avail - 1) : list;
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
      <Text dimColor wrap="truncate">{INTRO}</Text>
      {shown.map((r, i) => <Line key={off + i} row={r} />)}
      {maxScroll > 0
        ? (
          <Text dimColor wrap="truncate">
            {off < maxScroll
              ? `↓ ${list.length - off - (avail - 1)} more — j/k or ↑/↓ · wheel`
              : "end — ↑/k scrolls back"}
          </Text>
        )
        : null}
    </Box>
  );
}
