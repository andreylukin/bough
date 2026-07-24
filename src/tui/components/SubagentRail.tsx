import { Box } from "ink";
import { Text } from "./Text.tsx";
import { palette } from "../theme.ts";
import type { Branch } from "../lines.ts";

/** Glyph + status tail for one branch — same hue semantics as the transcript
 * card (blue = in flight, amber = stopped/attention, red = failed). */
function branchStatus(b: Branch): { color: string; tail: string } {
  if (b.busy) return { color: palette.info, tail: "⋯ working" };
  if (b.status === "orphaned") {
    return { color: palette.warn, tail: "◼ interrupted — server restarted" };
  }
  if (b.status === "interrupted") return { color: palette.warn, tail: "◼ interrupted" };
  if (b.status === "error" || b.ok === false) return { color: palette.error, tail: "✗ failed" };
  if (b.ok === true && b.checkPassed === false) {
    return { color: palette.warn, tail: "✓ done (check failed)" };
  }
  return { color: palette.accent, tail: "✓ done" };
}

/** The subagent rail: one row per branch of the open session, pinned under the
 * status bar (Claude-Code parity). ↓ from an empty composer moves into it, ↑/↓
 * walk the rows, enter opens the branch. `sel` is null while the composer has
 * focus — the rail still renders, just without a cursor. */
export function SubagentRail(
  { branches, sel }: { branches: Branch[]; sel: number | null },
) {
  if (branches.length === 0) return null;
  return (
    <Box flexDirection="column">
      {branches.map((b, i) => {
        const { color, tail } = branchStatus(b);
        const on = sel === i;
        return (
          <Text key={b.id} wrap="truncate">
            <Text color={on ? palette.accent : undefined} bold={on}>{on ? "▸" : " "}</Text>
            <Text color={color}>{" "}◆</Text>
            <Text bold={on}>{" "}{b.title.replace(/^subagent · /, "")}</Text>
            <Text color={color} dimColor={!on}>{"  "}{tail}</Text>
            {on ? <Text dimColor>{"  "}↩ open · esc composer</Text> : null}
          </Text>
        );
      })}
      {sel === null
        ? (
          <Text dimColor wrap="truncate">
            {`  ↓ ${branches.length} subagent${branches.length > 1 ? "s" : ""}`}
          </Text>
        )
        : null}
    </Box>
  );
}
