import { Box, Text } from "ink";

export function Composer(
  { input, queued, busy }: { input: string; queued: string[]; busy: boolean },
) {
  return (
    <Box flexDirection="column">
      {queued.map((q, i) => <Text key={i} dimColor>⧖ queued: {q}</Text>)}
      <Box borderStyle="round" borderColor={busy ? "yellow" : "gray"} paddingX={1}>
        <Text color="cyan">{"› "}</Text>
        <Text wrap="wrap">{input}</Text>
        <Text inverse>{" "}</Text>
      </Box>
    </Box>
  );
}
