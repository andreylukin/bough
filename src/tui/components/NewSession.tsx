import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import type { DirHit } from "../api.ts";

// New-session dialog: fuzzy workspace autocomplete over GET /fs/dirs (the TUI
// sibling of the web's new-session modal). Enter on a hit creates the session
// with that workspace; enter with nothing picked creates a workspace-less one.
export function NewSession(
  { query, cursor, hits, selected }: {
    query: string;
    cursor: number;
    hits: DirHit[];
    selected: number;
  },
) {
  const at = query[cursor];
  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      backgroundColor={palette.panel}
      borderColor={palette.border}
      paddingX={1}
    >
      <Text bold>new session</Text>
      <Text>
        <Text dimColor>{"project folder: "}</Text>
        {query.slice(0, cursor)}
        <Text inverse>{at ?? " "}</Text>
        {at === undefined ? "" : query.slice(cursor + 1)}
      </Text>
      {hits.map((h, i) => (
        <Text key={h.path} inverse={i === selected} wrap="truncate">
          {h.repo ? <Text color={palette.accent}>{"◆ "}</Text> : <Text dimColor>{"◇ "}</Text>}
          {h.display}
        </Text>
      ))}
      {hits.length === 0 && (query.trim() === ""
        ? <Text dimColor>enter creates without a project folder (runs in the server cwd)</Text>
        : (
          <Text color={palette.warn}>
            no matching folder — enter does nothing; clear the query to create without one
          </Text>
        ))}
      <Text dimColor>◆ git repo · ◇ plain folder</Text>
      {/* The status bar no longer carries per-mode hints — the modal owns its keys. */}
      {/* "enter create" would lie while a no-match query makes enter inert. */}
      {hits.length === 0 && query.trim() !== ""
        ? <Text dimColor>↑↓ pick · esc back</Text>
        : <Text dimColor>↑↓ pick · enter create · esc back</Text>}
    </Box>
  );
}
