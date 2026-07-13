import { Box, Text } from "ink";
import type { DirHit } from "../api.ts";

// New-session dialog: fuzzy workspace autocomplete over GET /fs/dirs (the TUI
// sibling of the web's new-session modal). Enter on a hit creates the session
// with that workspace; enter with nothing picked creates a workspace-less one.
export function NewSession(
  { query, hits, selected }: { query: string; hits: DirHit[]; selected: number },
) {
  return (
    <Box flexDirection="column" borderStyle="round" paddingX={1}>
      <Text bold>new session</Text>
      <Text>
        <Text dimColor>{"workspace: "}</Text>
        {query}
        <Text inverse>{" "}</Text>
      </Text>
      {hits.map((h, i) => (
        <Text key={h.path} inverse={i === selected} wrap="truncate">
          {h.repo ? <Text color="green">{"◆ "}</Text> : <Text dimColor>{"◇ "}</Text>}
          {h.display}
        </Text>
      ))}
      {hits.length === 0 && (
        <Text dimColor>
          {query.trim() === ""
            ? "enter creates without a workspace (runs in the server cwd)"
            : "no matches — clear the query to create without a workspace"}
        </Text>
      )}
    </Box>
  );
}
