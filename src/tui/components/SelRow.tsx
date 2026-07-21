// The shared selection light-bar for panel lists (sessions/conversation/changes/
// model/mcp/theme). Rendering a row's spans as one inverse Text only paints the
// bar behind the text runs — the row reads half-highlighted. SelRow pads the bar
// to the panel's full content width with an inverse filler that flex-grows into
// whatever space the text doesn't use.
import { Box } from "ink";
import { Text } from "./Text.tsx";
import type { ReactNode } from "react";

// Wider than any sane terminal; the box clips it to the space that's left.
const FILL = " ".repeat(400);

/** The grow-to-fit remainder of a selection bar. Exported for rows that can't
 * wrap all their spans in one inverse run (e.g. theme swatches keep colors).
 * The spaces WRAP (ink preserves whitespace-only lines) and the 1-row box
 * clips the rest — `truncate` would append a visible "…" to the bar. */
export function SelFill({ sel }: { sel: boolean }) {
  return (
    <Box flexGrow={1} flexBasis={0} height={1} overflowY="hidden">
      <Text inverse={sel}>{FILL}</Text>
    </Box>
  );
}

/** One list row: content in a single inverse run, the bar padded full-width.
 * `right` pins trailing metadata to the row's right edge (two-space gap baked
 * in) with the bar continuing across the middle. */
export function SelRow(
  { sel, children, right }: { sel: boolean; children: ReactNode; right?: ReactNode },
) {
  return (
    <Box>
      <Text inverse={sel} wrap="truncate">{children}</Text>
      <SelFill sel={sel} />
      {right !== undefined && <Text inverse={sel} wrap="truncate">{"  "}{right}</Text>}
    </Box>
  );
}
