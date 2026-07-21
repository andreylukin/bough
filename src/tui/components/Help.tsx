// The `?` overlay: keybindings rendered from KEYMAP (keys.ts) so the docs are
// always the bindings that actually run. Height-aware: the flat list renders
// with section spacing when it fits, drops the spacing when tight, and splits
// into two columns when even that overflows — Ink's clipping of an overflowing
// background Box merges rows into garbage, so overflow must never happen.
import { palette } from "../theme.ts";
import { Box, Text } from "ink";
import { KEYMAP } from "../keys.ts";

// Key column sized to the longest binding so descriptions always keep a gutter.
const PAD = Math.max(...KEYMAP.flatMap((s) => s.keys.map(([k]) => k.length))) + 2;

// Widest rendered row (indented key+desc, or a bare section header): two
// columns only when both fit side by side — truncated descriptions read as
// broken (visual audit found `…` cuts at 110 cols).
const WIDEST = Math.max(
  ...KEYMAP.map((s) => s.section.length),
  ...KEYMAP.flatMap((s) => s.keys.map(([, d]) => 2 + PAD + d.length)),
);

// One plain-language line beginners can act on before reading any bindings.
const INTRO = "type a message and press enter to talk to bough";

type Row = { header: string } | { key: string; desc: string } | { blank: true };

function flatRows(spaced: boolean): Row[] {
  const rows: Row[] = [];
  for (const sec of KEYMAP) {
    if (spaced && rows.length > 0) rows.push({ blank: true });
    rows.push({ header: sec.section });
    for (const [key, desc] of sec.keys) rows.push({ key, desc });
  }
  return rows;
}

function Line({ row }: { row: Row }) {
  if ("blank" in row) return <Text>{" "}</Text>;
  if ("header" in row) return <Text bold>{row.header}</Text>;
  return (
    <Text wrap="truncate">
      {"  "}
      <Text color={palette.accent}>{row.key.padEnd(PAD)}</Text>
      {row.desc}
    </Text>
  );
}

export function Help({ rows = 30, width = 80 }: { rows?: number; width?: number }) {
  // Chrome around the list: 2 border rows + title + intro + the status bar.
  const avail = Math.max(4, rows - 5);
  const spaced = flatRows(true);
  const tight = flatRows(false);
  const fit = spaced.length <= avail ? spaced : tight;
  // Column width = half the inner box minus col0's gutter; see WIDEST.
  const twoColFit = Math.floor((width - 4) / 2) - 2 >= WIDEST;
  if (fit.length <= avail || !twoColFit) {
    // One column; when even the tight list overflows a short terminal, cut it
    // and say so rather than letting Ink clip (which corrupts rows).
    const cut = fit.length > avail;
    const shown = cut ? fit.slice(0, avail - 1) : fit;
    return (
      <Box
        flexDirection="column"
        borderStyle="round"
        backgroundColor={palette.panel}
        borderColor={palette.border}
        paddingX={1}
      >
        <Text>
          <Text bold>keys</Text>
          <Text dimColor>{"  "}any key closes</Text>
        </Text>
        <Text dimColor>{INTRO}</Text>
        {shown.map((r, i) => <Line key={i} row={r} />)}
        {cut ? <Text dimColor>… more — enlarge the window to see all</Text> : null}
      </Box>
    );
  }
  // Two columns: full list, halved width, truncated cells beat merged rows.
  const half = Math.ceil(tight.length / 2);
  const cols = [tight.slice(0, half), tight.slice(half)];
  return (
    <Box
      flexDirection="row"
      borderStyle="round"
      backgroundColor={palette.panel}
      borderColor={palette.border}
      paddingX={1}
    >
      {cols.map((col, c) => (
        <Box key={c} flexDirection="column" width="50%" paddingRight={c === 0 ? 2 : 0}>
          {c === 0
            ? (
              <>
                <Text>
                  <Text bold>keys</Text>
                  <Text dimColor>{"  "}any key closes</Text>
                </Text>
                <Text dimColor>{INTRO}</Text>
              </>
            )
            : (
              <>
                <Text>{" "}</Text>
                <Text>{" "}</Text>
              </>
            )}
          {col.map((r, i) => <Line key={i} row={r} />)}
        </Box>
      ))}
    </Box>
  );
}
