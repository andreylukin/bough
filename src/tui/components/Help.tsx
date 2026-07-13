// The `?` overlay: keybindings rendered from KEYMAP (keys.ts) so the docs are
// always the bindings that actually run.
import { Box, Text } from "ink";
import { KEYMAP } from "../keys.ts";

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
              <Text color="cyan">{key.padEnd(8)}</Text>
              {desc}
            </Text>
          ))}
        </Box>
      ))}
    </Box>
  );
}
