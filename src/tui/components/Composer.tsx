import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";

// The input box: multiline-capable, with a real cursor (block over the character
// at the cursor position; a marker at end-of-text/end-of-line). Rendering is
// capped at maxRows — a large paste must not grow the box past the viewport —
// with an internal window that follows the cursor; the text itself is intact.
export function Composer(
  { input, cursor, busy, width, maxRows, ghost = "" }: {
    input: string;
    cursor: number;
    busy: boolean;
    width: number;
    maxRows: number;
    /** Dim autocomplete preview appended after the input (caller guarantees the
     * cursor sits at end-of-input while one is shown; tab accepts it). */
    ghost?: string;
  },
) {
  // Wrap ourselves (fixed-width chunks) so the cursor→row mapping is exact;
  // each row is its own truncated line, so ink never re-flows the block.
  const innerW = Math.max(4, width - 4); // border + paddingX
  // Empty composer: a dim in-box placeholder so the first action is visible
  // (first-run audit: the composer read as decoration without one). Kept even
  // when a ghost exists — a ghost only shows once you've started typing, so the
  // two never collide, and coupling them let a prediction eat the guidance.
  const placeholder = input === "" ? "type a message · enter sends" : "";
  // A shown ghost gets a subtle keycap so tab-accept is discoverable.
  const ghostHint = ghost ? "  ⇥ tab" : "";
  const full = "› " + input + ghost + ghostHint;
  const ghostStart = 2 + input.length;
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
  // A context hint under the box: `!` arms a real local-shell run (say so, and
  // how to back out); a plain Enter mid-turn steers the running turn rather than
  // starting a new one.
  const hint = input.startsWith("!")
    ? "local shell — enter runs · esc esc clears"
    : busy && input !== ""
    ? "enter interjects this turn"
    : "";
  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      backgroundColor={palette.panelInset}
      // Accent when awaiting input: the composer is the focused element in chat
      // mode, and a hairline border made the first action invisible (audit).
      borderColor={busy ? palette.warn : palette.accent}
      paddingX={1}
    >
      {shown.map((r, i) => {
        const hasCursor = top + i === curRow;
        const col = cur - r.start;
        const at = hasCursor ? r.text[col] : undefined;
        const prefix = r.start === 0 ? 2 : 0; // the accent "› " on the first row
        // Where this row crosses into ghost text — everything from there is dim.
        const gcol = Math.max(prefix, Math.min(ghostStart - r.start, r.text.length));
        return (
          <Text key={r.start} wrap="truncate">
            {prefix ? <Text color={palette.accent}>{"› "}</Text> : null}
            {hasCursor
              ? (
                <>
                  {r.text.slice(prefix, col)}
                  <Text inverse>{at ?? " "}</Text>
                  {placeholder ? <Text dimColor>{placeholder}</Text> : null}
                  {at === undefined ? "" : (
                    <Text dimColor={col + 1 >= gcol}>{r.text.slice(col + 1)}</Text>
                  )}
                </>
              )
              : r.text.length <= prefix
              ? " "
              : (
                <>
                  {r.text.slice(prefix, gcol)}
                  {gcol < r.text.length ? <Text dimColor>{r.text.slice(gcol)}</Text> : null}
                </>
              )}
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
      {hint ? <Text dimColor>{hint}</Text> : null}
    </Box>
  );
}
