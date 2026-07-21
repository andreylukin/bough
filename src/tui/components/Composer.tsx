import { palette } from "../theme.ts";
import { Box, Text } from "ink";

// The input box: multiline-capable, with a real cursor (block over the character
// at the cursor position; a marker at end-of-text/end-of-line). Rendering is
// capped at maxRows — a large paste must not grow the box past the viewport —
// with an internal window that follows the cursor; the text itself is intact.
export function Composer(
  { input, cursor, busy, width, maxRows }: {
    input: string;
    cursor: number;
    busy: boolean;
    width: number;
    maxRows: number;
  },
) {
  // Wrap ourselves (fixed-width chunks) so the cursor→row mapping is exact;
  // each row is its own truncated line, so ink never re-flows the block.
  const innerW = Math.max(4, width - 4); // border + paddingX
  const full = "› " + input;
  const cur = cursor + 2;
  const rows: { start: number; text: string }[] = [];
  let off = 0;
  for (const line of full.split("\n")) {
    for (let i = 0;; i += innerW) {
      rows.push({ start: off + i, text: line.slice(i, i + innerW) });
      if (i + innerW >= line.length) break;
    }
    off += line.length + 1;
  }
  // The cursor's row: within [start, start+len), or sitting at the row's end
  // when nothing continues it there (end of a logical line / end of text).
  const curRow = rows.findIndex((r, i) => {
    const end = r.start + r.text.length;
    return cur >= r.start &&
      (cur < end || (cur === end && (rows[i + 1]?.start ?? Infinity) > end));
  });
  const cap = Math.max(2, maxRows);
  const clipped = rows.length > cap;
  const shownCount = clipped ? cap - 1 : rows.length; // one row for the … counter
  const top = clipped
    ? Math.max(0, Math.min(curRow - (shownCount >> 1), rows.length - shownCount))
    : 0;
  const shown = rows.slice(top, top + shownCount);
  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      backgroundColor={palette.panelInset}
      borderColor={busy ? palette.warn : palette.border}
      paddingX={1}
    >
      {shown.map((r, i) => {
        const hasCursor = top + i === curRow;
        const col = cur - r.start;
        const at = hasCursor ? r.text[col] : undefined;
        const prefix = r.start === 0 ? 2 : 0; // the accent "› " on the first row
        return (
          <Text key={r.start} wrap="truncate">
            {prefix ? <Text color={palette.accent}>{"› "}</Text> : null}
            {hasCursor
              ? (
                <>
                  {r.text.slice(prefix, col)}
                  <Text inverse>{at ?? " "}</Text>
                  {at === undefined ? "" : r.text.slice(col + 1)}
                </>
              )
              : (r.text.slice(prefix) || " ")}
          </Text>
        );
      })}
      {clipped
        ? (
          <Text dimColor>
            … {top} line{top === 1 ? "" : "s"} above · {rows.length - top - shownCount} below
          </Text>
        )
        : null}
    </Box>
  );
}
