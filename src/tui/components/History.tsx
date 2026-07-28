/**
 * The open conversation as a tree — pi's `/tree`.
 *
 * The rows are built by `historytree.ts`, which is pure and tested; this file is
 * the paint. A WINDOW around the cursor, because a long session has more turns
 * than the panel has rows and the whole point of the view is that any of them is
 * reachable.
 */
import { TextAttributes } from "@opentui/core";
import type { TreeRow } from "../historytree.ts";
import { palette } from "../theme.ts";

export function ConversationTree(
  { rows, selected, height }: { rows: TreeRow[]; selected: number; height: number },
) {
  if (rows.length === 0) {
    return (
      <text attributes={TextAttributes.DIM}>this conversation has no turns yet — send one</text>
    );
  }
  // The legend is the one row of chrome, and the floor is 0 rather than 3: a floor
  // is a claim about how much room there is, and below four rows it was false —
  // OpenTUI answers an overrun by shrinking rows onto each other (`Panel.tsx`).
  const body = Math.max(0, height - 1);
  const at = Math.max(0, Math.min(selected, rows.length - 1));
  const start = Math.max(0, Math.min(at - Math.floor(body / 2), rows.length - body));
  const window = body === 0 ? [] : rows.slice(start, start + body);
  return (
    <box flexDirection="column">
      {window.map((r, i) => {
        const on = start + i === at;
        return (
          <text
            key={`${r.id}-${i}`}
            wrapMode="none"
          >
            {/* `❯` and an accent, not INVERSE: reverse video renders white-on-white
                after the OpenTUI migration (see `CARET_FG` in `Composer.tsx`), so
                the selected row was marked with nothing at all. This is the cursor
                every other list in the panel uses. */}
            <span fg={palette.accent}>{on ? "❯" : " "}</span>
            <span
              fg={r.kind === "branch" ? palette.info : r.active ? palette.accent : undefined}
              attributes={r.kind === "message" && r.role !== "user" && !on
                ? TextAttributes.DIM
                : TextAttributes.NONE}
            >
              {r.text}
            </span>
          </text>
        );
      })}
      <text attributes={TextAttributes.DIM} wrapMode="none">
        {rows.length > body ? `${at + 1}/${rows.length} · ` : ""}
        ↑↓ move · pgup/pgdn page · ⏎ branch from this turn · s branch + summary · esc back
      </text>
    </box>
  );
}
