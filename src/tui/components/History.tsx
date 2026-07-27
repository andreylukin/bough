/**
 * The open conversation as a tree — pi's `/tree`.
 *
 * The rows are built by `historytree.ts`, which is pure and tested; this file is
 * the paint. A WINDOW around the cursor, because a long session has more turns
 * than the panel has rows and the whole point of the view is that any of them is
 * reachable.
 */
import { Box, Text } from "ink";
import type { TreeRow } from "../historytree.ts";
import { palette } from "../theme.ts";

export function ConversationTree(
  { rows, selected, height }: { rows: TreeRow[]; selected: number; height: number },
) {
  if (rows.length === 0) {
    return <Text dimColor>this conversation has no turns yet — send one</Text>;
  }
  const body = Math.max(3, height - 1);
  const at = Math.max(0, Math.min(selected, rows.length - 1));
  const start = Math.max(0, Math.min(at - Math.floor(body / 2), rows.length - body));
  const window = rows.slice(start, Math.max(start + body, 1));
  return (
    <Box flexDirection="column">
      {window.map((r, i) => {
        const on = start + i === at;
        return (
          <Text key={`${r.id}-${i}`} wrap="truncate" inverse={on}>
            <Text
              color={r.kind === "branch" ? palette.info : r.active ? palette.accent : undefined}
              dimColor={r.kind === "message" && r.role !== "user" && !on}
            >
              {r.text}
            </Text>
          </Text>
        );
      })}
      <Text dimColor wrap="truncate">
        {rows.length > body ? `${at + 1}/${rows.length} · ` : ""}
        ↑↓ move · ⏎ branch from this turn · esc back
      </Text>
    </Box>
  );
}
