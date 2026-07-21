import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";

// The input box: multiline-capable, with a real cursor (block over the character
// at the cursor position; a marker at end-of-text/end-of-line).
export function Composer(
  { input, cursor, busy }: { input: string; cursor: number; busy: boolean },
) {
  const before = input.slice(0, cursor);
  const at = input[cursor];
  const after = at === undefined ? "" : input.slice(cursor + 1);
  return (
    <Box
      borderStyle="round"
      backgroundColor={palette.panelInset}
      borderColor={busy ? palette.warn : palette.border}
      paddingX={1}
    >
      <Text wrap="wrap">
        <Text color={palette.accent}>{"› "}</Text>
        {before}
        <Text inverse>{at === undefined || at === "\n" ? " " : at}</Text>
        {at === "\n" ? "\n" + after : after}
      </Text>
    </Box>
  );
}
