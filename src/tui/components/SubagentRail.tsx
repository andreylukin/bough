/**
 * The live-work rail: everything running *right now* on this session's behalf.
 *
 * THE INVARIANT THIS HOLDS: **the rail pins LIVE work only.** It is a "what is
 * happening" surface, not a history — a finished branch belongs in the tree
 * (`Tree.tsx`) and in the transcript's report note, both of which outlive the run. The
 * old tree learned this the hard way (commit `0b56e12`, "pin only live subagents to the
 * rail"): a rail that kept every branch it ever saw grew past the terminal on any real
 * fan-out and pushed the composer off screen, so the one thing it existed to show — the
 * two agents currently working — was the part you could not see.
 *
 * IT NOW HOLDS THREE KINDS, not one (spec §5: nothing runs invisibly, and every unit
 * is attributed separately). A background shell used to live as a card at the TAIL of
 * the transcript, visible only while you were scrolled to the bottom; a workflow run
 * lived in a tab you had to open. Both ran with no pixels on screen for as long as you
 * were reading anything else. `liveUnits` (store.ts) reduces shells, subagents and
 * runs to one shape, this renders one row each, and `x` stops whichever the cursor is
 * on — one list, one key, no per-kind surface.
 *
 * The rows are pre-filtered and ordered by `liveUnits`; nothing is re-derived here.
 * `unitLine` (format.ts) words a row, so what a row SAYS is testable without a
 * terminal, and every row is padded to the full width — an unpadded short row leaves
 * the tail of the longer one that was there before it (`padRow`).
 *
 * The cursor is OPTIONAL and null while the composer has focus: the rail still renders,
 * it simply carries no selection. That is what makes ↓-from-an-empty-composer a
 * reversible move rather than a mode switch.
 */
import type { SessionRow } from "../api.ts";
import type { LiveUnit } from "../store.ts";
import { bold, dim, info, truncateAnsi, unitLine } from "../format.ts";
import { padRow, styledRow } from "./Message.tsx";
import { DELEGATED_KINDS, isDelegated } from "../forest.ts";

/**
 * The delegated children of the open session with a turn in flight.
 *
 * Still the lineage rule the rail is built on: a busy FORK is a sibling conversation,
 * not delegated work, and it belongs in the tree rather than pinned under the composer.
 * `liveUnits` takes the result of this — it filters on `busy`, not on kind.
 */
export function liveSubagents(children: readonly SessionRow[]): SessionRow[] {
  return children
    .filter((s) => isDelegated(s.kind) && s.busy)
    .sort((a, b) => a.createdAt - b.createdAt);
}

/**
 * The one-line hint shown when the composer, not the rail, has the cursor.
 *
 * It counts by KIND, because "3 running" does not tell you whether to worry: three
 * shells is a build, three agents is a fan-out, and one of each is a turn that has
 * spread out. The plural is per kind for the same reason.
 */
export function railHint(units: readonly LiveUnit[]): string {
  const count = (kind: LiveUnit["kind"], one: string, many: string) => {
    const n = units.filter((u) => u.kind === kind).length;
    return n === 0 ? "" : `${n} ${n === 1 ? one : many}`;
  };
  const bits = [
    count("shell", "shell", "shells"),
    count("subagent", "agent", "agents"),
    count("workflow", "run", "runs"),
  ].filter(Boolean);
  return `↓ ${bits.join(" · ")} running`;
}

export function SubagentRail(
  { units, sel, width, armedId }: {
    /** Already filtered and ordered by `liveUnits` — this does not re-derive it. */
    units: readonly LiveUnit[];
    /** null while the composer has focus: rendered, but with no cursor. */
    sel: number | null;
    width: number;
    /** The unit a first `x` armed: the next one stops it (spec §7). */
    armedId?: string | null;
  },
) {
  if (units.length === 0) return null;
  const w = Math.max(1, width);
  // Chunks, never raw escapes, and padded to the full row — the two halves of the fix
  // that stopped a short row keeping the tail of the longer one under it (see
  // `styledRow`). The rail redraws once a second, so it is exactly the surface that
  // bug shows on.
  const row = (key: string, text: string) => (
    <text key={key} wrapMode="none" content={styledRow(padRow(truncateAnsi(text, w), w))} />
  );
  return (
    <box flexDirection="column">
      {units.map((u, i) => {
        const on = sel === i;
        // The armed row says what the next press destroys, in the row's own space —
        // spec §7: consent is never inferred, and the scope is said out loud.
        const hint = armedId === u.id
          ? dim("  x again stops it · esc cancels")
          : on
          ? dim("  ⏎ open · x stop · esc composer")
          : "";
        const head = on ? bold(info("❯")) : " ";
        return row(u.id, `${head} ${unitLine(u, w - 2)}${hint}`);
      })}
      {sel === null ? row("hint", dim(`  ${railHint(units)}`)) : null}
    </box>
  );
}

/** Re-exported so a caller filters with the same lineage rule the tree collapses by. */
export { DELEGATED_KINDS };
