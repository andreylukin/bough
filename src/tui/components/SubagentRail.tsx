/**
 * The subagent rail: the delegated work running *right now* under the open session.
 *
 * THE INVARIANT THIS HOLDS: **the rail pins LIVE subagents only.** It is a
 * "what is happening" surface, not a history — a finished branch belongs in the tree
 * (`Tree.tsx`) and in the transcript's report note, both of which outlive the run. The
 * old tree learned this the hard way (commit `0b56e12`, "pin only live subagents to the
 * rail"): a rail that kept every branch it ever saw grew past the terminal on any real
 * fan-out and pushed the composer off screen, so the one thing it existed to show — the
 * two agents currently working — was the part you could not see.
 *
 * `liveSubagents` is therefore the whole module in one function, and it is pure: given
 * the drill-in children of a session, it keeps the delegated kinds whose turn is in
 * flight, in start order. Nothing else is a rail row. `busy` is the server's derived
 * "a turn is running" field (`server/sessions.ts`), live-updated by `message.started` /
 * `message.finished` in the store — so a subagent that finishes disappears from the rail
 * on its own event, with no cleanup pass and no client-side timer.
 *
 * The cursor is OPTIONAL and null while the composer has focus: the rail still renders,
 * it simply carries no selection. That is what makes ↓-from-an-empty-composer a
 * reversible move rather than a mode switch.
 *
 * NOTE on colour: `tui/theme.ts` (T9.2) is not in this task's owned set, so ink's named
 * colours stand in for the served palette, confined to `RAIL_COLOR` below.
 */
import { Box, Text } from "ink";
import type { SessionRow } from "../api.ts";
import { DELEGATED_KINDS, isDelegated, titleOf } from "./Tree.tsx";

/** One hue, one meaning — cyan is in flight, and the rail holds nothing else. */
const RAIL_COLOR = "cyan";

/**
 * The rail's rows: delegated children of the open session with a turn in flight.
 *
 * Ordered by `createdAt`, so an agent's position does not move while it works — a rail
 * that reorders under the cursor makes ⏎ open the wrong branch.
 */
export function liveSubagents(children: readonly SessionRow[]): SessionRow[] {
  return children
    .filter((s) => isDelegated(s.kind) && s.busy)
    .sort((a, b) => a.createdAt - b.createdAt);
}

/** The row's trailing text. Always "working": the rail holds nothing that is not. */
export function railLabel(s: SessionRow): string {
  return `${titleOf(s)} — ⋯ working`;
}

/** The one-line hint shown when the composer, not the rail, has the cursor. */
export function railHint(count: number): string {
  return `↓ ${count} subagent${count === 1 ? "" : "s"} working`;
}

export function SubagentRail(
  { branches, sel }: {
    /** Already filtered by `liveSubagents` — the component does not re-derive it. */
    branches: readonly SessionRow[];
    /** null while the composer has focus: rendered, but with no cursor. */
    sel: number | null;
  },
) {
  if (branches.length === 0) return null;
  return (
    <Box flexDirection="column">
      {branches.map((b, i) => {
        const on = sel === i;
        return (
          <Text key={b.id} wrap="truncate">
            <Text color={on ? RAIL_COLOR : undefined} bold={on}>{on ? "❯" : " "}</Text>
            <Text color={RAIL_COLOR}>{" "}◆</Text>
            <Text bold={on}>{" "}{titleOf(b)}</Text>
            <Text color={RAIL_COLOR} dimColor={!on}>{"  "}⋯ working</Text>
            {on ? <Text dimColor>{"  "}⏎ open · esc composer</Text> : null}
          </Text>
        );
      })}
      {sel === null
        ? <Text dimColor wrap="truncate">{`  ${railHint(branches.length)}`}</Text>
        : null}
    </Box>
  );
}

/** Re-exported so a caller filters with the same lineage rule the tree collapses by. */
export { DELEGATED_KINDS };
