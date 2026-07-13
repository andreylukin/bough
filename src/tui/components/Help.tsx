// The `?` overlay: keybindings rendered from KEYMAP (keys.ts) so the docs are
// always the bindings that actually run.
import { Box, Text } from "ink";
import { KEYMAP } from "../keys.ts";

// Key column sized to the longest binding so descriptions always keep a gutter.
const PAD = Math.max(...KEYMAP.flatMap((s) => s.keys.map(([k]) => k.length))) + 2;

export function Help() {
  return (
    <Box flexDirection="column" borderStyle="round" paddingX={1}>
      <Text bold>keys</Text>
      {KEYMAP.map((sec) => (
        <Box key={sec.section} flexDirection="column" marginTop={1}>
          <Text dimColor>{sec.section}</Text>
          {sec.keys.map(([key, desc]) => (
            <Text key={key} wrap="truncate">
              {"  "}
              <Text color="cyan">{key.padEnd(PAD)}</Text>
              {desc}
            </Text>
          ))}
        </Box>
      ))}
    </Box>
  );
}
